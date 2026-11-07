//! 编辑联动的唯一入口。
//!
//! 一次请求必须携带全部目标语言并返回全部译文：免费大模型只有 1 并发，若每个目标语言各发一次，
//! 「改一处其余同步」就会退化成排队，用户在中文输入法里连续打字时尤其明显。把目标语言集合放进
//! `SyncPlan`，也让 provider 层在工单 04 只需实现一次调用。
//!
//! 每次 commit 递增 `generation`，`apply` 只接受与当前值相等的结果，过期结果整包丢弃而不是逐卡比对：
//! 逐卡比对会让一次旧请求的一部分写进新文档，产生「打字快了内容倒退」这种无法复现的 bug。
//! 最后编辑的卡被记为权威源，`apply` 永不覆盖它；其余卡在同步中保留旧文本，失败也只标错不清空。
//!
//! 同一段源文本、同一目标语言集合、同一引擎的成功结果会在进程内缓存：面板收起再展开、或反复
//! 提交同一段文本时直接写回上次译文，不再重发网络请求。键里带目标集合与引擎指纹，所以增删
//! 卡片或切换引擎/模型必然落空；失败结果不缓存，手动重试也从不读缓存。

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use crate::detector::LanguageDetector;
use crate::document::{Document, DocumentError, Lang, LanguagePool, SlotStatus};

/// 只用于「和上次是否同一段内容」的判断，不是安全哈希，也不写入日志。
pub fn content_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub trait Clock {
    fn now_ms(&self) -> u64;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncError {
    UnknownLanguage(Lang),
    Document(DocumentError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPlan {
    pub generation: u64,
    pub source_lang: Lang,
    pub source_text: String,
    pub targets: Vec<Lang>,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationItem {
    pub lang: Lang,
    pub text: Result<String, ProviderError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationSet {
    pub generation: u64,
    pub items: Vec<TranslationItem>,
}

pub trait Translator {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    StaleDiscarded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    /// 没有可复用的结果，必须真正发起一次翻译。
    Translate(SyncPlan),
    /// 命中缓存：结果已同步写回文档，不产生任何网络请求。
    Cached { generation: u64 },
}

/// 一次翻译结果的缓存键。
///
/// 目标语言参与键计算前先排序：拖拽换序不改变「要翻成哪些语言」，不应让缓存失效；
/// 增删卡片会改变集合，键随之不同，必然落空。引擎指纹由壳层提供（引擎 + 模型 + 入口），
/// 不含任何密钥。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TranslationCacheKey {
    source_lang: Lang,
    source_text: String,
    targets: Vec<Lang>,
    engine: String,
}

impl TranslationCacheKey {
    fn from_plan(plan: &SyncPlan, engine: &str) -> Self {
        let mut targets = plan.targets.clone();
        targets.sort();
        targets.dedup();
        Self {
            source_lang: plan.source_lang.clone(),
            source_text: plan.source_text.clone(),
            targets,
            engine: engine.to_string(),
        }
    }
}

/// 结果里是否每个目标语言都成功；成功时返回 (语言, 译文) 列表。
///
/// 缓存与翻译历史共用这条判定：只有整包成功才值得持久化/复用。
pub fn complete_translations(plan: &SyncPlan, result: &TranslationSet) -> Option<Vec<(Lang, String)>> {
    let mut items = Vec::with_capacity(plan.targets.len());
    for target in &plan.targets {
        let item = result.items.iter().find(|item| &item.lang == target)?;
        let text = item.text.as_ref().ok()?;
        items.push((target.clone(), text.clone()));
    }
    Some(items)
}

/// 进程内、不落盘的翻译结果缓存（持久化历史的上层热缓存）。
///
/// 只保存「全部目标语言都成功」的结果：失败或部分成功的集合不缓存，这样手动重试失败卡
/// 永远走真实请求。容量固定为 8 条并按插入顺序淘汰最旧的一条：一次会话里的工作集通常
/// 只覆盖当前文档的少量改动，8 条足以吸收「收起再展开、反复提交同一段」的重复请求，
/// 又不会让用户粘贴的长文本无限堆积；落盘历史另有 10 条上限。
pub struct TranslationCache {
    capacity: usize,
    entries: HashMap<TranslationCacheKey, BTreeMap<Lang, String>>,
    insertion_order: VecDeque<TranslationCacheKey>,
}

impl TranslationCache {
    pub const DEFAULT_CAPACITY: usize = 8;

    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.insertion_order.clear();
    }

    fn lookup(&self, plan: &SyncPlan, engine: &str) -> Option<TranslationSet> {
        let texts = self.entries.get(&TranslationCacheKey::from_plan(plan, engine))?;
        let mut items = Vec::with_capacity(plan.targets.len());
        for target in &plan.targets {
            let text = texts.get(target)?.clone();
            items.push(TranslationItem {
                lang: target.clone(),
                text: Ok(text),
            });
        }
        Some(TranslationSet {
            generation: plan.generation,
            items,
        })
    }

    /// 只有每个目标语言都拿到成功译文才写入；任何一张失败都整包放弃。
    fn record(&mut self, plan: &SyncPlan, engine: &str, result: &TranslationSet) {
        if self.capacity == 0 || plan.targets.is_empty() {
            return;
        }
        let Some(items) = complete_translations(plan, result) else {
            return;
        };
        let texts: BTreeMap<Lang, String> = items.into_iter().collect();

        let key = TranslationCacheKey::from_plan(plan, engine);
        if !self.entries.contains_key(&key) {
            while self.entries.len() >= self.capacity {
                let Some(oldest) = self.insertion_order.pop_front() else {
                    break;
                };
                self.entries.remove(&oldest);
            }
            self.insertion_order.push_back(key.clone());
        }
        self.entries.insert(key, texts);
    }
}

pub struct SyncCoordinator<C: Clock> {
    pool: LanguagePool,
    document: Document,
    generation: u64,
    last_clipboard_hash: Option<u64>,
    clock: C,
    detector: Box<dyn LanguageDetector>,
    cache: TranslationCache,
}

impl<C: Clock> SyncCoordinator<C> {
    pub fn new(pool: LanguagePool, clock: C, detector: Box<dyn LanguageDetector>) -> Self {
        let document = Document::for_pool(&pool);
        Self {
            pool,
            document,
            generation: 0,
            last_clipboard_hash: None,
            clock,
            detector,
            cache: TranslationCache::new(TranslationCache::DEFAULT_CAPACITY),
        }
    }

    pub fn document(&self) -> &Document {
        &self.document
    }

    /// 语言池的槽位顺序即卡片顺序；改语言只替换同一槽位，不重排。
    pub fn pool(&self) -> &LanguagePool {
        &self.pool
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 清空文档并让在途翻译失效：`generation` 递增后，旧计划回来的结果在
    /// `apply` 里整包丢弃，不会把刚清空的卡又被填回去。
    pub fn clear(&mut self) {
        self.document.clear();
        self.last_clipboard_hash = None;
        self.generation += 1;
        // 刻意不清翻译缓存：面板收起会走到这里，再次悬停读到同一段内容应直接复用译文。
    }

    /// 把一张卡设成权威源，并产出一次覆盖所有目标语言的请求。
    pub fn commit(&mut self, source: &Lang, text: impl Into<String>) -> Result<SyncPlan, SyncError> {
        if !self.pool.contains(source) {
            return Err(SyncError::UnknownLanguage(source.clone()));
        }
        self.document
            .set_text(source, text)
            .map_err(SyncError::Document)?;
        self.document
            .set_status(source, SlotStatus::Idle)
            .map_err(SyncError::Document)?;
        self.document
            .set_authoritative(source)
            .map_err(SyncError::Document)?;
        self.generation += 1;

        let targets = self.pool.targets_excluding(source);
        for target in &targets {
            self.document
                .set_status(target, SlotStatus::Syncing)
                .map_err(SyncError::Document)?;
        }

        let source_text = self
            .document
            .slot(source)
            .expect("source slot was just written")
            .text
            .clone();
        Ok(SyncPlan {
            generation: self.generation,
            source_lang: source.clone(),
            source_text,
            targets,
            created_at_ms: self.clock.now_ms(),
        })
    }

    /// 编辑提交的缓存入口：同一源文本 + 同一目标集合 + 同一引擎直接复用上次成功结果。
    pub fn commit_with_cache(
        &mut self,
        source: &Lang,
        text: impl Into<String>,
        engine: &str,
    ) -> Result<CommitOutcome, SyncError> {
        let plan = self.commit(source, text)?;
        Ok(self.resolve_with_cache(plan, engine))
    }

    fn resolve_with_cache(&mut self, plan: SyncPlan, engine: &str) -> CommitOutcome {
        match self.cache.lookup(&plan, engine) {
            Some(result) => {
                // 命中即同步写回：调用方拿到的快照里目标卡已经是 Idle，不会停在「同步中」。
                let _ = self.apply(result);
                CommitOutcome::Cached {
                    generation: plan.generation,
                }
            }
            None => CommitOutcome::Translate(plan),
        }
    }

    /// 把一次成功结果写入缓存；调用方只在确实发过网络且整包成功时调用。
    pub fn record_translation(&mut self, plan: &SyncPlan, engine: &str, result: &TranslationSet) {
        if result.generation != plan.generation {
            return;
        }
        self.cache.record(plan, engine, result);
    }

    /// 引擎/模型变化时整体失效：旧引擎的译文不能与新配置混用。
    pub fn invalidate_translation_cache(&mut self) {
        self.cache.clear();
    }

    pub fn cached_translations(&self) -> usize {
        self.cache.len()
    }

    /// 过期结果整包丢弃；权威源永不覆盖；单卡失败保留旧文本并单独标错。
    pub fn apply(&mut self, result: TranslationSet) -> ApplyOutcome {
        if result.generation != self.generation {
            return ApplyOutcome::StaleDiscarded;
        }
        let authoritative = self.document.authoritative().cloned();
        for item in result.items {
            if Some(&item.lang) == authoritative.as_ref() {
                continue;
            }
            match item.text {
                Ok(text) => {
                    let _ = self.document.set_text(&item.lang, text);
                    let _ = self.document.set_status(&item.lang, SlotStatus::Idle);
                }
                Err(_) => {
                    let _ = self.document.set_status(&item.lang, SlotStatus::Error);
                }
            }
        }
        ApplyOutcome::Applied
    }

    /// hover 读到的剪贴板文本入口。`force` 对应界面上的「刷新」，忽略哈希去重。
    /// 空文本按空态处理，不发起请求；初始落卡走语言检测：
    /// - 命中池内语言 → 落到对应卡（该卡即权威源）；
    /// - 识别到语言但池里没有这张卡 → **不粘贴**（不回退到权威源或其他卡）；
    /// - 无法确定语言（短文本/品牌名/URL/代码/纯符号/混排）→ **同样不粘贴**，
    ///   绝不能把非英语内容默认当成 en 或主语言卡翻出错误结果。
    pub fn accept_clipboard(&mut self, text: &str, force: bool) -> Option<SyncPlan> {
        match self.accept_clipboard_inner(text, force, None)? {
            CommitOutcome::Translate(plan) => Some(plan),
            CommitOutcome::Cached { .. } => None,
        }
    }

    /// 剪贴板路径的缓存入口。`force` 是用户显式「刷新」，必须重新发请求、跳过缓存。
    pub fn accept_clipboard_with_cache(
        &mut self,
        text: &str,
        force: bool,
        engine: &str,
    ) -> Option<CommitOutcome> {
        self.accept_clipboard_inner(text, force, Some(engine))
    }

    fn accept_clipboard_inner(
        &mut self,
        text: &str,
        force: bool,
        engine: Option<&str>,
    ) -> Option<CommitOutcome> {
        let hash = content_hash(text);
        if !force && self.last_clipboard_hash == Some(hash) {
            return None;
        }
        self.last_clipboard_hash = Some(hash);
        if text.trim().is_empty() {
            return None;
        }
        let source = match self.detector.detect(text) {
            Some(lang) if self.pool.contains(&lang) => lang,
            // 识别到池外语言、或根本无法确定语言：都不落卡、不翻译。
            Some(_) | None => return None,
        };
        let plan = self.commit(&source, text).ok()?;
        Some(match engine {
            Some(engine) if !force => self.resolve_with_cache(plan, engine),
            _ => CommitOutcome::Translate(plan),
        })
    }

    /// 变更语言池（工单 03 的设置动作）：保留语言的 slot 文本原样带过去，被移除的丢弃；
    /// 原权威源被移除时回落主语言卡，还在则保持。槽位状态一律回 Idle——换池是显式
    /// 设置动作，旧池的同步目标已不存在。
    ///
    /// 成员一旦变化，目标集合就变了：清空缓存并重置剪贴板去重基线，下一次提交必须真实请求
    /// （新增的卡才会被翻译）。仅拖拽换序时成员不变，缓存保留、卡片顺序不动。
    pub fn replace_pool(&mut self, pool: LanguagePool) {
        let membership_changed = {
            let old: BTreeSet<&str> = self.pool.languages().iter().map(|lang| lang.code()).collect();
            let new: BTreeSet<&str> = pool.languages().iter().map(|lang| lang.code()).collect();
            old != new
        };
        let old_authoritative = self.document.authoritative().cloned();
        let mut document = Document::for_pool(&pool);
        for language in pool.languages() {
            if let Some(slot) = self.document.slot(language) {
                if !slot.text.is_empty() {
                    let _ = document.set_text(language, slot.text.clone());
                }
            }
        }
        self.pool = pool;
        self.document = document;
        match old_authoritative {
            Some(language) if self.pool.contains(&language) => {
                let _ = self.document.set_authoritative(&language);
            }
            Some(_) => {
                let _ = self.document.set_authoritative(self.pool.primary());
            }
            None => {}
        }
        if membership_changed {
            self.cache.clear();
            self.last_clipboard_hash = None;
        }
    }

    /// 对 apply 标错的卡单独重试：源仍是权威源卡的现文本，一次请求只携带失败语言
    /// （仍是单个请求，不是每语言各发一次）。无失败卡时返回 None、不递增 generation；
    /// 「失败不清空已填内容」由 apply 保证，这里不做文本改动。
    pub fn retry_failed(&mut self) -> Option<SyncPlan> {
        let authoritative = self.document.authoritative().cloned()?;
        let source_text = self.document.slot(&authoritative)?.text.clone();
        let targets: Vec<Lang> = self
            .pool
            .targets_excluding(&authoritative)
            .into_iter()
            .filter(|lang| {
                self.document
                    .slot(lang)
                    .is_some_and(|slot| slot.status == SlotStatus::Error)
            })
            .collect();
        if targets.is_empty() {
            return None;
        }
        self.generation += 1;
        for target in &targets {
            let _ = self.document.set_status(target, SlotStatus::Syncing);
        }
        Some(SyncPlan {
            generation: self.generation,
            source_lang: authoritative,
            source_text,
            targets,
            created_at_ms: self.clock.now_ms(),
        })
    }

    /// 用历史译文回填当前文档。
    ///
    /// - 历史条目多出的目标语言（当前池里没有）直接忽略，不报错。
    /// - 历史条目缺少的目标语言合并成**一次**增量计划（`targets` 只含缺失项）。
    /// - 全部目标都有译文时返回 None，不需要任何网络请求。
    /// - 源语言不在当前池里时仍可回填：把池内所有语言当作目标，源文本只用于后续补翻请求。
    pub fn restore_history(
        &mut self,
        source_lang: &Lang,
        source_text: &str,
        translations: &BTreeMap<Lang, String>,
    ) -> Option<SyncPlan> {
        let source_in_pool = self.pool.contains(source_lang);
        if source_in_pool {
            let _ = self.document.set_text(source_lang, source_text.to_string());
            let _ = self.document.set_status(source_lang, SlotStatus::Idle);
            let _ = self.document.set_authoritative(source_lang);
        }

        self.generation += 1;
        let candidates: Vec<Lang> = if source_in_pool {
            self.pool
                .languages()
                .iter()
                .filter(|lang| lang != &source_lang)
                .cloned()
                .collect()
        } else {
            self.pool.languages().to_vec()
        };

        let mut missing = Vec::new();
        for lang in candidates {
            match translations.get(&lang) {
                Some(text) => {
                    let _ = self.document.set_text(&lang, text.clone());
                    let _ = self.document.set_status(&lang, SlotStatus::Idle);
                }
                None => {
                    let _ = self.document.set_status(&lang, SlotStatus::Syncing);
                    missing.push(lang);
                }
            }
        }

        if missing.is_empty() {
            return None;
        }
        Some(SyncPlan {
            generation: self.generation,
            source_lang: source_lang.clone(),
            source_text: source_text.to_string(),
            targets: missing,
            created_at_ms: self.clock.now_ms(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClock(u64);

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    struct EchoTranslator;

    impl Translator for EchoTranslator {
        fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
            Ok(TranslationSet {
                generation: request.generation,
                items: request
                    .targets
                    .iter()
                    .map(|lang| TranslationItem {
                        lang: lang.clone(),
                        text: Ok(format!("[{}] {}", lang.code(), request.source_text)),
                    })
                    .collect(),
            })
        }
    }

    struct NullDetector;

    impl LanguageDetector for NullDetector {
        fn detect(&self, _text: &str) -> Option<Lang> {
            None
        }
    }

    struct FixedDetector(Option<Lang>);

    impl LanguageDetector for FixedDetector {
        fn detect(&self, _text: &str) -> Option<Lang> {
            self.0.clone()
        }
    }

    fn coordinator(codes: &[&str]) -> SyncCoordinator<FakeClock> {
        // 多数用例假设「剪贴板文本总是识别为主语言」，显式用 FixedDetector 表达，
        // 不再依赖检测器返回 None 时的历史兜底行为。
        let primary = Lang::new(codes[0]);
        coordinator_with(Box::new(FixedDetector(Some(primary))), codes)
    }

    fn coordinator_with(
        detector: Box<dyn LanguageDetector>,
        codes: &[&str],
    ) -> SyncCoordinator<FakeClock> {
        let pool = LanguagePool::new(codes.iter().map(|code| Lang::new(*code)).collect())
            .expect("test pool is valid");
        SyncCoordinator::new(pool, FakeClock(1_000), detector)
    }

    #[test]
    fn committing_one_language_plans_a_single_request_for_every_other_language() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();

        assert_eq!(plan.targets, vec![Lang::new("en")]);
        assert_eq!(plan.source_lang, Lang::new("zh"));
        assert_eq!(plan.source_text, "你好");
        assert_eq!(plan.generation, 1);
        assert_eq!(plan.created_at_ms, 1_000);
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().status,
            SlotStatus::Syncing
        );
    }

    #[test]
    fn a_single_plan_carries_all_target_languages_at_once() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        assert_eq!(plan.targets, vec![Lang::new("en"), Lang::new("ja")]);
        assert_eq!(EchoTranslator.translate(&plan).unwrap().items.len(), 2);
    }

    #[test]
    fn applying_a_fake_translation_fills_the_other_card() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        let result = EchoTranslator.translate(&plan).unwrap();

        assert_eq!(coordinator.apply(result), ApplyOutcome::Applied);
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] 你好"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().status,
            SlotStatus::Idle
        );
    }

    #[test]
    fn a_stale_generation_result_is_discarded_without_overwriting_any_card() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let stale_plan = coordinator.commit(&Lang::new("zh"), "旧文本").unwrap();
        let stale_result = EchoTranslator.translate(&stale_plan).unwrap();
        let _fresh_plan = coordinator.commit(&Lang::new("zh"), "新文本").unwrap();

        assert_eq!(coordinator.apply(stale_result), ApplyOutcome::StaleDiscarded);
        assert_eq!(coordinator.document().slot(&Lang::new("zh")).unwrap().text, "新文本");
        assert_eq!(coordinator.document().slot(&Lang::new("en")).unwrap().text, "");
    }

    #[test]
    fn the_authoritative_card_is_never_overwritten_by_apply() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let plan = coordinator.commit(&Lang::new("zh"), "我写的").unwrap();
        let result = TranslationSet {
            generation: plan.generation,
            items: vec![TranslationItem {
                lang: Lang::new("zh"),
                text: Ok("机器改回去的".into()),
            }],
        };

        assert_eq!(coordinator.apply(result), ApplyOutcome::Applied);
        assert_eq!(coordinator.document().slot(&Lang::new("zh")).unwrap().text, "我写的");
    }

    #[test]
    fn a_failed_card_keeps_its_old_text_and_is_marked_as_error() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let first = coordinator.commit(&Lang::new("zh"), "第一版").unwrap();
        coordinator.apply(EchoTranslator.translate(&first).unwrap());
        let second = coordinator.commit(&Lang::new("zh"), "第二版").unwrap();

        coordinator.apply(TranslationSet {
            generation: second.generation,
            items: vec![TranslationItem {
                lang: Lang::new("en"),
                text: Err(ProviderError("timeout".into())),
            }],
        });

        let slot = coordinator.document().slot(&Lang::new("en")).unwrap();
        assert_eq!(slot.text, "[en] 第一版");
        assert_eq!(slot.status, SlotStatus::Error);
    }

    #[test]
    fn the_same_clipboard_content_does_not_request_a_second_translation() {
        let mut coordinator = coordinator(&["zh", "en"]);
        assert!(coordinator.accept_clipboard("Hello world", false).is_some());
        assert!(coordinator.accept_clipboard("Hello world", false).is_none());
        assert_eq!(coordinator.generation(), 1);
    }

    #[test]
    fn clearing_the_document_discards_a_translation_that_was_already_in_flight() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        let result = EchoTranslator.translate(&plan).unwrap();

        coordinator.clear();

        assert_eq!(coordinator.apply(result), ApplyOutcome::StaleDiscarded);
        assert_eq!(coordinator.document().slot(&Lang::new("en")).unwrap().text, "");
        assert_eq!(coordinator.document().authoritative(), None);
    }

    #[test]
    fn refresh_forces_a_new_request_for_unchanged_clipboard_content() {
        let mut coordinator = coordinator(&["zh", "en"]);
        coordinator.accept_clipboard("Hello world", false);
        assert!(coordinator.accept_clipboard("Hello world", true).is_some());
        assert_eq!(coordinator.generation(), 2);
    }

    #[test]
    fn empty_clipboard_text_shows_an_empty_state_without_a_request() {
        let mut coordinator = coordinator(&["zh", "en"]);
        assert!(coordinator.accept_clipboard("   ", false).is_none());
        assert_eq!(coordinator.generation(), 0);
        assert_eq!(coordinator.document().slot(&Lang::new("zh")).unwrap().text, "");
    }

    #[test]
    fn retrying_retranslates_only_the_failed_cards_and_keeps_successful_ones() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        let mut result = EchoTranslator.translate(&plan).unwrap();
        result.items[1].text = Err(ProviderError("timeout".into()));
        coordinator.apply(result);

        let retry = coordinator.retry_failed().expect("failed slot exists");
        assert_eq!(retry.targets, vec![Lang::new("ja")]);
        assert_eq!(retry.source_lang, Lang::new("zh"));
        assert_eq!(retry.source_text, "你好");

        coordinator.apply(EchoTranslator.translate(&retry).unwrap());
        let retried = coordinator.document().slot(&Lang::new("ja")).unwrap();
        assert_eq!(retried.text, "[ja] 你好");
        assert_eq!(retried.status, SlotStatus::Idle);
        let untouched = coordinator.document().slot(&Lang::new("en")).unwrap();
        assert_eq!(untouched.text, "[en] 你好");
        assert_eq!(untouched.status, SlotStatus::Idle);
    }

    #[test]
    fn retry_without_any_failed_card_is_a_noop() {
        let mut coordinator = coordinator(&["zh", "en"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        coordinator.apply(EchoTranslator.translate(&plan).unwrap());
        let before = coordinator.generation();

        assert!(coordinator.retry_failed().is_none());
        assert_eq!(coordinator.generation(), before);
    }

    #[test]
    fn detected_language_lands_in_its_own_card_and_translates_the_rest() {
        let mut coordinator = coordinator_with(
            Box::new(FixedDetector(Some(Lang::new("ja")))),
            &["zh", "en", "ja"],
        );
        let plan = coordinator.accept_clipboard("テストです", false).unwrap();
        assert_eq!(plan.source_lang, Lang::new("ja"));
        coordinator.apply(EchoTranslator.translate(&plan).unwrap());

        assert_eq!(
            coordinator.document().slot(&Lang::new("ja")).unwrap().text,
            "テストです"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] テストです"
        );
        assert_eq!(coordinator.document().authoritative(), Some(&Lang::new("ja")));
    }

    #[test]
    fn english_content_lands_on_the_english_card() {
        let mut coordinator = coordinator_with(
            Box::new(crate::detector::HeuristicDetector),
            &["zh", "en"],
        );
        let plan = coordinator.accept_clipboard("Hello world", false).unwrap();
        assert_eq!(plan.source_lang, Lang::new("en"));
        assert_eq!(coordinator.document().authoritative(), Some(&Lang::new("en")));
    }

    #[test]
    fn undetected_content_is_not_pasted_at_all() {
        // 低置信度/纯符号：既不落主语言卡，也不翻译，卡片保持原样。
        let mut coordinator = coordinator_with(Box::new(NullDetector), &["zh", "en"]);
        assert!(coordinator.accept_clipboard("1234", false).is_none());
        assert_eq!(coordinator.document().slot(&Lang::new("zh")).unwrap().text, "");
        assert_eq!(coordinator.document().slot(&Lang::new("en")).unwrap().text, "");
        assert_eq!(coordinator.document().authoritative(), None);
        assert_eq!(coordinator.generation(), 0);
    }

    #[test]
    fn a_detected_language_outside_the_pool_is_not_pasted_at_all() {
        let mut coordinator = coordinator_with(
            Box::new(FixedDetector(Some(Lang::new("ko")))),
            &["zh", "en"],
        );
        // 池里没有 ko 卡：不粘贴，也不回退到 zh/en
        assert!(coordinator.accept_clipboard("안녕하세요", false).is_none());
        assert_eq!(coordinator.document().slot(&Lang::new("zh")).unwrap().text, "");
        assert_eq!(coordinator.document().slot(&Lang::new("en")).unwrap().text, "");
    }

    #[test]
    fn changing_the_pool_keeps_overlapping_cards_and_drops_removed_ones() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        coordinator.apply(EchoTranslator.translate(&plan).unwrap());

        coordinator.replace_pool(
            LanguagePool::new(vec![Lang::new("zh"), Lang::new("en")]).expect("two languages"),
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("zh")).unwrap().text,
            "你好"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] 你好"
        );
        assert_eq!(coordinator.document().slot(&Lang::new("ja")), None);
        assert_eq!(coordinator.document().authoritative(), Some(&Lang::new("zh")));
    }

    #[test]
    fn removing_the_authoritative_card_falls_back_to_the_primary() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let plan = coordinator.commit(&Lang::new("ja"), "テスト").unwrap();
        coordinator.apply(EchoTranslator.translate(&plan).unwrap());
        assert_eq!(coordinator.document().authoritative(), Some(&Lang::new("ja")));

        coordinator.replace_pool(
            LanguagePool::new(vec![Lang::new("zh"), Lang::new("en")]).expect("two languages"),
        );
        assert_eq!(coordinator.document().authoritative(), Some(&Lang::new("zh")));
        assert_eq!(coordinator.document().slot(&Lang::new("ja")), None);
    }

    #[test]
    fn replacing_a_card_language_keeps_its_slot_position() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let plan = coordinator.commit(&Lang::new("zh"), "你好").unwrap();
        coordinator.apply(EchoTranslator.translate(&plan).unwrap());

        coordinator.replace_pool(
            LanguagePool::new(vec![Lang::new("fr"), Lang::new("en"), Lang::new("ja")])
                .expect("three languages"),
        );

        let order: Vec<&str> = coordinator
            .pool()
            .languages()
            .iter()
            .map(|l| l.code())
            .collect();
        assert_eq!(order, vec!["fr", "en", "ja"]);
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] 你好"
        );
    }

    fn engine() -> &'static str {
        "zerokey||"
    }

    /// 提交并走一次真实翻译（模拟壳层：请求 → 结果 → 写缓存 → 写回文档）。
    fn translate_and_apply(
        coordinator: &mut SyncCoordinator<FakeClock>,
        source: &str,
        text: &str,
        engine: &str,
    ) -> SyncPlan {
        let plan = match coordinator
            .commit_with_cache(&Lang::new(source), text, engine)
            .unwrap()
        {
            CommitOutcome::Translate(plan) => plan,
            CommitOutcome::Cached { .. } => panic!("empty cache must translate"),
        };
        let result = EchoTranslator.translate(&plan).unwrap();
        coordinator.record_translation(&plan, engine, &result);
        coordinator.apply(result);
        plan
    }

    #[test]
    fn committing_the_same_text_and_targets_twice_reuses_the_cached_result() {
        let mut coordinator = coordinator(&["zh", "en"]);
        translate_and_apply(&mut coordinator, "zh", "你好", engine());
        let before = coordinator.generation();

        let outcome = coordinator
            .commit_with_cache(&Lang::new("zh"), "你好", engine())
            .unwrap();

        assert_eq!(outcome, CommitOutcome::Cached { generation: before + 1 });
        let slot = coordinator.document().slot(&Lang::new("en")).unwrap();
        assert_eq!(slot.text, "[en] 你好");
        assert_eq!(slot.status, SlotStatus::Idle); // 命中不留在「同步中」
    }

    #[test]
    fn reopening_after_a_clear_reuses_the_cached_translation_instead_of_requesting_again() {
        // 面板收起会 clear 文档并重置剪贴板去重基线；再悬停读到同一段剪贴板时
        // 曾经会整包重翻。缓存命中后仍重新落卡，但不再发网络请求。
        let mut coordinator = coordinator(&["zh", "en"]);
        let plan = match coordinator
            .accept_clipboard_with_cache("Hello world", false, engine())
            .unwrap_or_else(|| panic!("first read must translate"))
        {
            CommitOutcome::Translate(plan) => plan,
            CommitOutcome::Cached { .. } => panic!("first read must translate"),
        };
        let result = EchoTranslator.translate(&plan).unwrap();
        coordinator.record_translation(&plan, engine(), &result);
        coordinator.apply(result);
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] Hello world"
        );

        coordinator.clear();
        let outcome = coordinator
            .accept_clipboard_with_cache("Hello world", false, engine())
            .expect("同一段剪贴板应重新落卡");

        assert!(matches!(outcome, CommitOutcome::Cached { .. }));
        assert_eq!(
            coordinator.document().slot(&Lang::new("zh")).unwrap().text,
            "Hello world"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] Hello world"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().status,
            SlotStatus::Idle
        );
    }

    #[test]
    fn adding_a_language_card_misses_the_cache_and_requests_again() {
        let mut coordinator = coordinator(&["zh", "en"]);
        translate_and_apply(&mut coordinator, "zh", "你好", engine());
        assert_eq!(coordinator.cached_translations(), 1);

        coordinator.replace_pool(
            LanguagePool::new(vec![Lang::new("zh"), Lang::new("en"), Lang::new("ja")])
                .expect("three languages"),
        );
        assert_eq!(coordinator.cached_translations(), 0); // 目标集合变了，旧结果作废

        let outcome = coordinator
            .commit_with_cache(&Lang::new("zh"), "你好", engine())
            .unwrap();
        match outcome {
            CommitOutcome::Translate(plan) => {
                assert_eq!(plan.targets, vec![Lang::new("en"), Lang::new("ja")]);
            }
            CommitOutcome::Cached { .. } => panic!("新增卡片后必须真实请求"),
        }
    }

    #[test]
    fn switching_the_engine_misses_the_cache_and_requests_again() {
        let mut coordinator = coordinator(&["zh", "en"]);
        translate_and_apply(&mut coordinator, "zh", "你好", "zerokey||");

        let outcome = coordinator
            .commit_with_cache(&Lang::new("zh"), "你好", "zhipu|glm-4.7-flash|")
            .unwrap();

        match outcome {
            CommitOutcome::Translate(plan) => assert_eq!(plan.source_text, "你好"),
            CommitOutcome::Cached { .. } => panic!("换引擎后必须真实请求"),
        }
    }

    #[test]
    fn a_cached_success_does_not_block_a_manual_refresh() {
        let mut coordinator = coordinator(&["zh", "en"]);
        translate_and_apply(&mut coordinator, "zh", "你好", engine());

        let outcome = coordinator
            .accept_clipboard_with_cache("你好", true, engine())
            .expect("refresh must always produce a request");

        assert!(matches!(outcome, CommitOutcome::Translate(_)));
    }

    #[test]
    fn a_manual_retry_after_a_failed_translation_always_requests_a_new_translation() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let plan = match coordinator
            .commit_with_cache(&Lang::new("zh"), "你好", engine())
            .unwrap()
        {
            CommitOutcome::Translate(plan) => plan,
            CommitOutcome::Cached { .. } => panic!("empty cache must translate"),
        };
        // 部分成功：en 成功、ja 失败 → 整包不写缓存
        let mut result = EchoTranslator.translate(&plan).unwrap();
        result.items[1].text = Err(ProviderError("timeout".into()));
        coordinator.record_translation(&plan, engine(), &result);
        coordinator.apply(result);
        assert_eq!(coordinator.cached_translations(), 0);

        // 手动重试只带失败卡，且不受任何缓存影响
        let retry = coordinator.retry_failed().expect("failed slot exists");
        assert_eq!(retry.targets, vec![Lang::new("ja")]);

        // 同文再次提交也必须重新请求（失败结果从未进缓存）
        let outcome = coordinator
            .commit_with_cache(&Lang::new("zh"), "你好", engine())
            .unwrap();
        assert!(matches!(outcome, CommitOutcome::Translate(_)));
    }

    #[test]
    fn history_restore_ignores_targets_that_are_not_in_the_current_pool() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let translations: BTreeMap<Lang, String> = [
            (Lang::new("en"), "[en] 你好".to_string()),
            (Lang::new("ja"), "[ja] 你好".to_string()),
            // 历史条目里多出来的语言：当前池没有 fr，应当直接忽略
            (Lang::new("fr"), "[fr] 你好".to_string()),
        ]
        .into_iter()
        .collect();

        let missing = coordinator.restore_history(&Lang::new("zh"), "你好", &translations);

        assert!(missing.is_none());
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] 你好"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("ja")).unwrap().text,
            "[ja] 你好"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("ja")).unwrap().status,
            SlotStatus::Idle
        );
        assert_eq!(coordinator.document().slot(&Lang::new("zh")).unwrap().text, "你好");
    }

    #[test]
    fn history_restore_only_requests_the_missing_targets() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let translations: BTreeMap<Lang, String> =
            [(Lang::new("en"), "[en] 你好".to_string())].into_iter().collect();

        let plan = coordinator
            .restore_history(&Lang::new("zh"), "你好", &translations)
            .expect("ja is missing");

        assert_eq!(plan.targets, vec![Lang::new("ja")]);
        assert_eq!(plan.source_lang, Lang::new("zh"));
        assert_eq!(plan.source_text, "你好");
        assert_eq!(
            coordinator.document().slot(&Lang::new("en")).unwrap().text,
            "[en] 你好"
        );
        assert_eq!(
            coordinator.document().slot(&Lang::new("ja")).unwrap().status,
            SlotStatus::Syncing
        );
    }

    #[test]
    fn history_restore_of_a_complete_entry_needs_no_request() {
        let mut coordinator = coordinator(&["zh", "en", "ja"]);
        let translations: BTreeMap<Lang, String> = [
            (Lang::new("en"), "[en] 你好".to_string()),
            (Lang::new("ja"), "[ja] 你好".to_string()),
        ]
        .into_iter()
        .collect();

        assert!(coordinator
            .restore_history(&Lang::new("zh"), "你好", &translations)
            .is_none());
        assert_eq!(
            coordinator.document().slot(&Lang::new("ja")).unwrap().status,
            SlotStatus::Idle
        );
    }
}
