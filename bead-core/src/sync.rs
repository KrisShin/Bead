//! 编辑联动的唯一入口。
//!
//! 一次请求必须携带全部目标语言并返回全部译文：免费大模型只有 1 并发，若每个目标语言各发一次，
//! 「改一处其余同步」就会退化成排队，用户在中文输入法里连续打字时尤其明显。把目标语言集合放进
//! `SyncPlan`，也让 provider 层在工单 04 只需实现一次调用。
//!
//! 每次 commit 递增 `generation`，`apply` 只接受与当前值相等的结果，过期结果整包丢弃而不是逐卡比对：
//! 逐卡比对会让一次旧请求的一部分写进新文档，产生「打字快了内容倒退」这种无法复现的 bug。
//! 最后编辑的卡被记为权威源，`apply` 永不覆盖它；其余卡在同步中保留旧文本，失败也只标错不清空。

use std::collections::hash_map::DefaultHasher;
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

pub struct SyncCoordinator<C: Clock> {
    pool: LanguagePool,
    document: Document,
    generation: u64,
    last_clipboard_hash: Option<u64>,
    clock: C,
    detector: Box<dyn LanguageDetector>,
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
    /// - 完全识别不出（如西里尔字母/纯符号）→ 回落主语言卡，保持旧行为。
    pub fn accept_clipboard(&mut self, text: &str, force: bool) -> Option<SyncPlan> {
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
            Some(_) => return None,
            None => self.pool.primary().clone(),
        };
        self.commit(&source, text).ok()
    }

    /// 变更语言池（工单 03 的设置动作）：保留语言的 slot 文本原样带过去，被移除的丢弃；
    /// 原权威源被移除时回落主语言卡，还在则保持。槽位状态一律回 Idle——换池是显式
    /// 设置动作，旧池的同步目标已不存在。
    pub fn replace_pool(&mut self, pool: LanguagePool) {
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
        coordinator_with(Box::new(NullDetector), codes)
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
    fn undetected_content_falls_back_to_the_primary_card() {
        // 纯符号/无法判别书写系统：仍回落主语言卡
        let mut coordinator = coordinator_with(Box::new(FixedDetector(None)), &["zh", "en"]);
        let plan = coordinator.accept_clipboard("1234", false).unwrap();
        assert_eq!(plan.source_lang, Lang::new("zh"));
        assert_eq!(
            coordinator.document().slot(&Lang::new("zh")).unwrap().text,
            "1234"
        );
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
}
