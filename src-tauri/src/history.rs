//! 持久化的翻译历史（进程内缓存的落盘版）。
//!
//! 语义：最近使用优先（MRU），固定 10 条；命中或回填即置顶；超出淘汰最旧。
//! 只记录「整包成功」的真实翻译结果，且源文本必须通过 `SensitiveFilter` 才会写盘：
//! 命中敏感规则的文本只留在内存，不写文件，也不会重写/破坏已有历史。
//!
//! 文件是应用数据目录下的 `history.json`，写入走「临时文件 + rename」，读回失败
//! （损坏或版本不认识）时按空历史处理并重建，启动路径不会因此失败。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use bead_core::{SensitiveFilter, SensitiveVerdict};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "history.json";
const FORMAT_VERSION: u32 = 1;

pub const DEFAULT_CAPACITY: usize = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    #[serde(default)]
    pub id: u64,
    pub source_text: String,
    pub source_lang: String,
    /// 目标语言集合；存储时已排序去重，顺序不参与语义。
    #[serde(default)]
    pub targets: Vec<String>,
    /// 语言代码 → 译文。只包含 `targets` 内成功的语言。
    #[serde(default)]
    pub translations: BTreeMap<String, String>,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub created_at_ms: u64,
}

impl HistoryEntry {
    fn same_key(&self, source_text: &str, source_lang: &str, targets: &[String], engine: &str) -> bool {
        self.source_text == source_text
            && self.source_lang == source_lang
            && self.engine == engine
            && sorted_set(&self.targets) == sorted_set(targets)
    }
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    version: u32,
    entries: Vec<HistoryEntry>,
}

fn sorted_set(codes: &[String]) -> Vec<String> {
    let mut set = codes.to_vec();
    set.sort();
    set.dedup();
    set
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 进程内历史表 + 文件镜像。`path == None` 表示还没拿到应用数据目录（启动早期）。
pub struct TranslationHistory {
    path: Option<PathBuf>,
    capacity: usize,
    entries: Vec<HistoryEntry>,
}

impl TranslationHistory {
    /// 启动早期的空表；拿到应用数据目录后用 `load` 替换。
    pub fn empty(capacity: usize) -> Self {
        Self {
            path: None,
            capacity: capacity.max(1),
            entries: Vec::new(),
        }
    }

    /// 从 `dir/history.json` 读取。任何读取/解析失败都退化成空历史，并尝试重建文件。
    pub fn load(dir: &Path, capacity: usize) -> Self {
        let path = dir.join(FILE_NAME);
        let (entries, should_rebuild) = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<HistoryFile>(&raw) {
                Ok(file) if file.version == FORMAT_VERSION => (file.entries, false),
                // 版本不认识：按空历史处理并重建，不猜测旧格式语义。
                Ok(_) => (Vec::new(), true),
                Err(_) => (Vec::new(), true),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), false),
            Err(_) => (Vec::new(), true),
        };

        let history = Self {
            path: Some(path),
            capacity: capacity.max(1),
            entries: sanitize(entries, capacity.max(1)),
        };
        if should_rebuild {
            let _ = history.save();
        }
        history
    }

    #[allow(dead_code)]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: u64) -> Option<HistoryEntry> {
        self.entries.iter().find(|entry| entry.id == id).cloned()
    }

    /// 记录一次整包成功的翻译。命中敏感规则时直接放弃（不写盘、不改动已有历史）。
    pub fn record(
        &mut self,
        source_text: &str,
        source_lang: &str,
        targets: &[String],
        translations: &BTreeMap<String, String>,
        engine: &str,
    ) -> bool {
        if !matches!(SensitiveFilter::classify(source_text), SensitiveVerdict::Allowed) {
            return false;
        }
        let targets = sorted_set(targets);
        let mut filtered = BTreeMap::new();
        for (lang, text) in translations {
            if targets.iter().any(|target| target == lang) {
                filtered.insert(lang.clone(), text.clone());
            }
        }
        if targets.is_empty() {
            return false;
        }

        let now = now_ms();
        if let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.same_key(source_text, source_lang, &targets, engine))
        {
            let mut entry = self.entries.remove(position);
            entry.targets = targets;
            entry.translations = filtered;
            entry.created_at_ms = now;
            self.entries.insert(0, entry);
        } else {
            let id = self.next_id();
            self.entries.insert(
                0,
                HistoryEntry {
                    id,
                    source_text: source_text.to_string(),
                    source_lang: source_lang.to_string(),
                    targets,
                    translations: filtered,
                    engine: engine.to_string(),
                    created_at_ms: now,
                },
            );
        }
        self.trim();
        let _ = self.save();
        true
    }

    /// 同键命中：置顶但不改时间戳（没有产生新的翻译）。
    pub fn touch_matching(
        &mut self,
        source_text: &str,
        source_lang: &str,
        targets: &[String],
        engine: &str,
    ) -> bool {
        let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.same_key(source_text, source_lang, targets, engine))
        else {
            return false;
        };
        self.move_to_front(position);
        true
    }

    /// 按 id 置顶（点击历史回填时使用）。
    pub fn touch(&mut self, id: u64) -> bool {
        let Some(position) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.move_to_front(position);
        true
    }

    /// 增量补翻成功后合并进原条目：目标集合取并集、时间刷新、仍置顶。
    /// 引擎不一致时不合并（避免把不同引擎的译文混成一条）。
    pub fn merge(
        &mut self,
        id: u64,
        translations: &BTreeMap<String, String>,
        engine: &str,
    ) -> bool {
        let Some(position) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        if self.entries[position].engine != engine {
            return false;
        }
        let mut entry = self.entries.remove(position);
        for (lang, text) in translations {
            entry.translations.insert(lang.clone(), text.clone());
        }
        entry.targets = sorted_set(&entry.translations.keys().cloned().collect::<Vec<_>>());
        entry.created_at_ms = now_ms();
        self.entries.insert(0, entry);
        let _ = self.save();
        true
    }

    /// 找到可与当前源文本复用的最近条目：源文本、源语言、引擎都必须一致，
    /// 且至少有一个当前目标语言能直接从条目里取到译文。多余的目标语言由调用方忽略。
    pub fn find_compatible(
        &self,
        source_text: &str,
        source_lang: &str,
        current_targets: &[String],
        engine: &str,
    ) -> Option<HistoryEntry> {
        self.entries
            .iter()
            .find(|entry| {
                entry.source_text == source_text
                    && entry.source_lang == source_lang
                    && entry.engine == engine
                    && entry
                        .translations
                        .keys()
                        .any(|lang| current_targets.iter().any(|target| target == lang))
            })
            .cloned()
    }

    /// 清空历史并写回一个空文件（不是删文件，避免残留旧内容的可恢复副本）。
    pub fn clear(&mut self) {
        self.entries.clear();
        let _ = self.save();
    }

    fn next_id(&self) -> u64 {
        self.entries.iter().map(|entry| entry.id).max().unwrap_or(0) + 1
    }

    fn trim(&mut self) {
        if self.entries.len() > self.capacity {
            self.entries.truncate(self.capacity);
        }
    }

    fn move_to_front(&mut self, position: usize) {
        if position == 0 {
            return;
        }
        let entry = self.entries.remove(position);
        self.entries.insert(0, entry);
        let _ = self.save();
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = HistoryFile {
            version: FORMAT_VERSION,
            entries: self.entries.clone(),
        };
        let body = serde_json::to_vec_pretty(&file)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error.to_string()))?;
        // 同目录临时文件 + rename：读者要么看到旧文件，要么看到完整新文件。
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn sanitize(mut entries: Vec<HistoryEntry>, capacity: usize) -> Vec<HistoryEntry> {
    for entry in entries.iter_mut() {
        entry.targets = sorted_set(&entry.targets);
    }
    // 同键只保留最靠前（最新）的一条。
    let mut seen = HashSet::new();
    entries.retain(|entry| {
        let key = format!(
            "{}\u{0}{}\u{0}{}\u{0}{}",
            entry.source_lang,
            entry.source_text,
            entry.engine,
            entry.targets.join(",")
        );
        seen.insert(key)
    });
    entries.truncate(capacity);

    // id 缺失或重复时补发，保证点击回填时能稳定定位。
    let mut used = HashSet::new();
    let mut max_id = 0u64;
    for entry in entries.iter_mut() {
        if entry.id != 0 && used.insert(entry.id) {
            max_id = max_id.max(entry.id);
        } else {
            entry.id = 0;
        }
    }
    for entry in entries.iter_mut() {
        if entry.id == 0 {
            max_id += 1;
            entry.id = max_id;
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("bead-history-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn translations(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items
            .iter()
            .map(|(lang, text)| ((*lang).to_string(), (*text).to_string()))
            .collect()
    }

    fn record(
        history: &mut TranslationHistory,
        source_text: &str,
        targets: &[&str],
        translated: &[(&str, &str)],
    ) {
        let targets: Vec<String> = targets.iter().map(|t| (*t).to_string()).collect();
        assert!(history.record(
            source_text,
            "zh",
            &targets,
            &translations(translated),
            "zerokey||",
        ));
    }

    #[test]
    fn persisted_history_is_read_back_after_restart() {
        let dir = TempDir::new();
        {
            let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
            record(&mut history, "第一句", &["en"], &[("en", "First")]);
            record(&mut history, "第二句", &["en", "ja"], &[("en", "Second"), ("ja", "二番目")]);
        }
        let history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        assert_eq!(history.len(), 2);
        let entries = history.entries();
        assert_eq!(entries[0].source_text, "第二句");
        assert_eq!(entries[0].translations.get("ja"), Some(&"二番目".to_string()));
        assert_eq!(entries[1].source_text, "第一句");
        assert!(entries[0].id != entries[1].id);
    }

    #[test]
    fn mru_newest_first_and_touch_moves_the_used_entry_to_the_front() {
        let dir = TempDir::new();
        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        record(&mut history, "A", &["en"], &[("en", "A-en")]);
        record(&mut history, "B", &["en"], &[("en", "B-en")]);
        record(&mut history, "C", &["en"], &[("en", "C-en")]);
        let order: Vec<&str> = history.entries().iter().map(|e| e.source_text.as_str()).collect();
        assert_eq!(order, vec!["C", "B", "A"]);

        let id = history
            .entries()
            .iter()
            .find(|entry| entry.source_text == "A")
            .unwrap()
            .id;
        assert!(history.touch(id));
        let order: Vec<&str> = history.entries().iter().map(|e| e.source_text.as_str()).collect();
        assert_eq!(order, vec!["A", "C", "B"]);

        // 重启后 MRU 顺序也要保留
        let reloaded = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        let order: Vec<&str> = reloaded.entries().iter().map(|e| e.source_text.as_str()).collect();
        assert_eq!(order, vec!["A", "C", "B"]);
    }

    #[test]
    fn capacity_evicts_the_oldest_entry() {
        let dir = TempDir::new();
        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        for index in 0..(DEFAULT_CAPACITY + 1) {
            let text = format!("文本{index}");
            let translated = format!("text{index}");
            record(&mut history, &text, &["en"], &[("en", translated.as_str())]);
        }
        assert_eq!(history.len(), DEFAULT_CAPACITY);
        assert!(history.entries().iter().all(|entry| entry.source_text != "文本0"));
        assert_eq!(history.entries()[0].source_text, "文本10");

        let reloaded = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        assert_eq!(reloaded.len(), DEFAULT_CAPACITY);
        assert!(reloaded.entries().iter().all(|entry| entry.source_text != "文本0"));
    }

    #[test]
    fn sensitive_text_is_not_written_and_existing_history_survives() {
        let dir = TempDir::new();
        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        record(&mut history, "普通内容", &["en"], &[("en", "Plain")]);

        assert!(!history.record(
            "password=hunter2",
            "zh",
            &["en".to_string()],
            &translations(&[("en", "Password")]),
            "zerokey||",
        ));
        assert_eq!(history.len(), 1);
        assert_eq!(history.entries()[0].source_text, "普通内容");

        let reloaded = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.entries()[0].source_text, "普通内容");

        // 全新目录里只出现敏感内容时，连文件都不该生成
        let fresh = TempDir::new();
        let mut only_sensitive = TranslationHistory::load(&fresh.0, DEFAULT_CAPACITY);
        assert!(!only_sensitive.record(
            "sk-abcdef0123456789",
            "zh",
            &["en".to_string()],
            &translations(&[("en", "Secret")]),
            "zerokey||",
        ));
        assert!(!only_sensitive.path().unwrap().exists());
    }

    #[test]
    fn corrupt_file_is_treated_as_empty_and_rebuilt_on_the_next_write() {
        let dir = TempDir::new();
        let path = dir.0.join(FILE_NAME);
        std::fs::write(&path, "{ not json").expect("write corrupt file");

        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        assert!(history.is_empty());
        // load 已经尝试重建为空历史，之后可以继续正常写入
        record(&mut history, "恢复", &["en"], &[("en", "Recovered")]);
        let reloaded = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.entries()[0].source_text, "恢复");
    }

    #[test]
    fn unknown_format_version_is_treated_as_empty() {
        let dir = TempDir::new();
        let path = dir.0.join(FILE_NAME);
        std::fs::write(&path, r#"{"version":99,"entries":[{"source_text":"x","source_lang":"zh"}]}"#)
            .expect("write unknown format");
        let history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        assert!(history.is_empty());
    }

    #[test]
    fn compatible_lookup_ignores_extra_targets_and_requires_the_same_engine() {
        let dir = TempDir::new();
        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        record(
            &mut history,
            "你好",
            &["en", "ja"],
            &[("en", "hello"), ("ja", "こんにちは")],
        );

        let current = vec!["en".to_string()];
        let found = history
            .find_compatible("你好", "zh", &current, "zerokey||")
            .expect("extra targets should be ignored");
        assert_eq!(found.targets, vec!["en".to_string(), "ja".to_string()]);

        let unrelated = vec!["fr".to_string()];
        assert!(history.find_compatible("你好", "zh", &unrelated, "zerokey||").is_none());
        let other_engine = vec!["en".to_string()];
        assert!(history
            .find_compatible("你好", "zh", &other_engine, "deepl||")
            .is_none());
    }

    #[test]
    fn merging_missing_targets_updates_the_same_entry_and_keeps_mru() {
        let dir = TempDir::new();
        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        record(&mut history, "你好", &["en"], &[("en", "hello")]);
        record(&mut history, "第二句", &["en"], &[("en", "second")]);
        let id = history
            .entries()
            .iter()
            .find(|entry| entry.source_text == "你好")
            .unwrap()
            .id;

        assert!(history.merge(id, &translations(&[("ja", "こんにちは")]), "zerokey||"));
        assert_eq!(history.len(), 2);
        let merged = history.get(id).unwrap();
        assert_eq!(merged.targets, vec!["en".to_string(), "ja".to_string()]);
        assert_eq!(merged.translations.get("ja"), Some(&"こんにちは".to_string()));
        assert_eq!(history.entries()[0].id, id); // 合并后置顶

        assert!(!history.merge(id, &translations(&[("fr", "bonjour")]), "deepl||"));
    }

    #[test]
    fn clear_removes_memory_and_writes_an_empty_file() {
        let dir = TempDir::new();
        let mut history = TranslationHistory::load(&dir.0, DEFAULT_CAPACITY);
        record(&mut history, "你好", &["en"], &[("en", "hello")]);
        history.clear();
        assert!(history.is_empty());

        let path = dir.0.join(FILE_NAME);
        let raw = std::fs::read_to_string(path).expect("empty history file exists");
        let file: HistoryFile = serde_json::from_str(&raw).expect("valid empty history file");
        assert_eq!(file.version, FORMAT_VERSION);
        assert!(file.entries.is_empty());
        assert!(TranslationHistory::load(&dir.0, DEFAULT_CAPACITY).is_empty());
    }
}
