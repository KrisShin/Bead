//! 发音：本地优先、在线仅在缺音色时兜底、按（语言, 文本, 引擎参数）缓存。
//!
//! 编排逻辑在这里（纯逻辑，注入 fake 即可断言全部路径）；真实实现在别处——
//! 在线抓取走 HTTP，本地发声走系统 TTS（Windows SAPI，见 `src-tauri/src/speech_local.rs`）。
//! 在线产物由前端 `<audio>` 播放（返回文件路径），本地引擎自己发声。
//! 失败只给上层一个错误让它出轻提示，决不清空卡片状态，也绝不向用户暴露引擎选择。
//!
//! 音色匹配、长文本切块与缓存键都是纯函数：不依赖系统、不联网、可被单测覆盖。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::document::Lang;

/// 在线端点单次文本上限（Google translate_tts）。长文本必须切块，禁止静默截断。
pub const ONLINE_CHUNK_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechError {
    OnlineFailed(String),
    OfflineFailed(String),
}

/// 在线 TTS：返回音频字节（mp3）。
pub trait OnlineTts: Send + Sync {
    fn fetch(&self, language: &Lang, text: &str) -> Result<Vec<u8>, SpeechError>;
}

/// 系统已安装的一个音色。`culture` 是 BCP-47 形式（如 `zh-CN`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceInfo {
    pub id: String,
    pub culture: String,
    pub enabled: bool,
}

/// 一次合成使用的引擎参数。它参与缓存键：换音色/语速/后端后不得命中旧音频。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpeechProfile {
    pub backend: String,
    pub voice_id: String,
    pub rate: i32,
}

impl SpeechProfile {
    /// 在线音频是预渲染产物，语速由前端倍速控制；`rate` 仍入键，避免参数变化后
    /// 命中旧文件造成「语速设置看起来没生效」的错觉。
    pub fn online(rate: i32) -> Self {
        Self {
            backend: "google".into(),
            voice_id: String::new(),
            rate,
        }
    }
}

pub struct SpeechManager<O: OnlineTts> {
    online: O,
    cache_dir: PathBuf,
}

impl<O: OnlineTts> SpeechManager<O> {
    pub fn new(online: O, cache_dir: PathBuf) -> Self {
        Self { online, cache_dir }
    }

    pub fn cache_path(&self, language: &Lang, text: &str, profile: &SpeechProfile) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        (language.code(), text, profile).hash(&mut hasher);
        self.cache_dir.join(format!(
            "{}-{:016x}.mp3",
            language.code(),
            hasher.finish()
        ))
    }

    /// 抓取整段文本的在线音频并落盘。长文本按 `ONLINE_CHUNK_CHARS` 切块后拼接，
    /// 全程成功才写文件；任一块失败都返回错误，避免「只读了前半段」。
    pub fn request(
        &self,
        language: &Lang,
        text: &str,
        profile: &SpeechProfile,
    ) -> Result<PathBuf, SpeechError> {
        let path = self.cache_path(language, text, profile);
        if Path::new(&path).exists() {
            return Ok(path);
        }

        let chunks = chunk_text(text, ONLINE_CHUNK_CHARS);
        let mut bytes = Vec::new();
        for chunk in &chunks {
            let mut part = self.online.fetch(language, chunk)?;
            bytes.append(&mut part);
        }
        if bytes.is_empty() {
            return Err(SpeechError::OnlineFailed("在线朗读返回空音频".into()));
        }

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 先写临时文件再 rename：进程中途退出时不会留下半截音频被当成缓存命中。
        let temp = path.with_extension("part");
        if let Err(error) = std::fs::write(&temp, &bytes) {
            let _ = std::fs::remove_file(&temp);
            return Err(SpeechError::OnlineFailed(format!("缓存写入失败：{error}")));
        }
        if std::fs::rename(&temp, &path).is_err() {
            let _ = std::fs::remove_file(&temp);
            if !path.exists() {
                return Err(SpeechError::OnlineFailed("缓存落盘失败".into()));
            }
        }
        Ok(path)
    }
}

/// 语言码 → Windows culture 的首选与次选。只覆盖语言池会出现的码；未知语言按同主语言匹配。
pub fn cultures_for(code: &str) -> &'static [&'static str] {
    match code {
        "zh" => &["zh-CN", "zh-TW", "zh-HK", "zh-SG"],
        "en" => &["en-US", "en-GB", "en-AU", "en-CA", "en-IN"],
        "ja" => &["ja-JP"],
        "ko" => &["ko-KR"],
        "fr" => &["fr-FR", "fr-CA"],
        "de" => &["de-DE", "de-AT", "de-CH"],
        "es" => &["es-ES", "es-MX", "es-AR"],
        "ru" => &["ru-RU"],
        "pt" => &["pt-BR", "pt-PT"],
        "it" => &["it-IT"],
        "ar" => &["ar-SA", "ar-EG"],
        "th" => &["th-TH"],
        "vi" => &["vi-VN"],
        "id" => &["id-ID"],
        "nl" => &["nl-NL"],
        "tr" => &["tr-TR"],
        _ => &[],
    }
}

/// 按语言从已安装音色中选一个。顺序：精确 culture → 同主语言其他 region。
/// 禁止用英语音色去读中文/日文来「凑合」——错误发音比不可用更糟。
pub fn choose_voice(language: &Lang, voices: &[VoiceInfo]) -> Option<VoiceInfo> {
    let code = language.code();
    let preferred = cultures_for(code);

    for culture in preferred {
        if let Some(voice) = voices
            .iter()
            .find(|voice| voice.enabled && voice.culture.eq_ignore_ascii_case(culture))
        {
            return Some(voice.clone());
        }
    }

    let code_lower = code.to_ascii_lowercase();
    voices
        .iter()
        .find(|voice| {
            if !voice.enabled {
                return false;
            }
            let primary = voice
                .culture
                .split(['-', '_'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            primary == code_lower
        })
        .cloned()
}

/// 按标点/换行切句后装箱到 `max_chars` 以内；超长单句硬切。只做切分，不做截断。
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return Vec::new();
    }
    if trimmed.chars().count() <= max_chars {
        return vec![trimmed.to_string()];
    }

    let mut sentences = Vec::new();
    let mut buffer = String::new();
    for ch in trimmed.chars() {
        buffer.push(ch);
        if matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            let sentence = buffer.trim();
            if !sentence.is_empty() {
                sentences.push(sentence.to_string());
            }
            buffer.clear();
        }
    }
    let rest = buffer.trim();
    if !rest.is_empty() {
        sentences.push(rest.to_string());
    }

    let mut pieces = Vec::new();
    for sentence in sentences {
        if sentence.chars().count() <= max_chars {
            pieces.push(sentence);
        } else {
            pieces.extend(hard_split(&sentence, max_chars));
        }
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for piece in pieces {
        if current.is_empty() {
            current = piece;
        } else if current.chars().count() + piece.chars().count() + 1 <= max_chars {
            if needs_space(&current, &piece) {
                current.push(' ');
            }
            current.push_str(&piece);
        } else {
            chunks.push(current);
            current = piece;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn hard_split(text: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.chars().count() >= max_chars {
            parts.push(current.trim().to_string());
            current.clear();
        }
    }
    let rest = current.trim();
    if !rest.is_empty() {
        parts.push(rest.to_string());
    }
    parts
}

fn needs_space(left: &str, right: &str) -> bool {
    let left_ascii = left
        .chars()
        .last()
        .map(|ch| ch.is_ascii_alphanumeric())
        .unwrap_or(false);
    let right_ascii = right
        .chars()
        .next()
        .map(|ch| ch.is_ascii_alphanumeric())
        .unwrap_or(false);
    left_ascii && right_ascii
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeOnline {
        fail: bool,
        calls: Mutex<Vec<String>>,
        bytes: Vec<u8>,
    }

    impl OnlineTts for FakeOnline {
        fn fetch(&self, _language: &Lang, text: &str) -> Result<Vec<u8>, SpeechError> {
            self.calls.lock().unwrap().push(text.to_string());
            if self.fail {
                Err(SpeechError::OnlineFailed("网络不可达".into()))
            } else {
                Ok(self.bytes.clone())
            }
        }
    }

    fn manager(fail: bool, dir: PathBuf) -> SpeechManager<FakeOnline> {
        SpeechManager::new(
            FakeOnline {
                fail,
                calls: Mutex::new(Vec::new()),
                bytes: b"mp3-bytes".to_vec(),
            },
            dir,
        )
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bead-speech-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn voice(id: &str, culture: &str) -> VoiceInfo {
        VoiceInfo {
            id: id.into(),
            culture: culture.into(),
            enabled: true,
        }
    }

    #[test]
    fn online_success_downloads_and_caches_the_audio() {
        let dir = temp_dir("online");
        let manager = manager(false, dir.clone());
        let profile = SpeechProfile::online(0);
        let path = manager
            .request(&Lang::new("en"), "Hello", &profile)
            .unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"mp3-bytes");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_profile_hits_the_cache_without_a_second_online_request() {
        let dir = temp_dir("cache");
        let manager = manager(false, dir.clone());
        let profile = SpeechProfile::online(0);
        let first = manager.request(&Lang::new("en"), "Hello", &profile).unwrap();
        let second = manager.request(&Lang::new("en"), "Hello", &profile).unwrap();
        assert_eq!(first, second);
        assert_eq!(manager.online.calls.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn different_languages_cache_separately_for_the_same_text() {
        let dir = temp_dir("langs");
        let manager = manager(false, dir.clone());
        let profile = SpeechProfile::online(0);
        let en = manager.request(&Lang::new("en"), "Hello", &profile).unwrap();
        let ja = manager.request(&Lang::new("ja"), "Hello", &profile).unwrap();
        assert_ne!(en, ja);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn changing_rate_or_backend_changes_the_cache_key() {
        let dir = temp_dir("profile");
        let manager = manager(false, dir.clone());
        let fast = manager
            .request(&Lang::new("en"), "Hello", &SpeechProfile::online(0))
            .unwrap();
        let slow = manager
            .request(&Lang::new("en"), "Hello", &SpeechProfile::online(5))
            .unwrap();
        let other = manager
            .request(
                &Lang::new("en"),
                "Hello",
                &SpeechProfile {
                    backend: "azure".into(),
                    voice_id: "x".into(),
                    rate: 0,
                },
            )
            .unwrap();
        assert_ne!(fast, slow);
        assert_ne!(fast, other);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn long_text_is_chunked_instead_of_silently_truncated() {
        let dir = temp_dir("chunks");
        let manager = manager(false, dir.clone());
        let text = format!("{}{}{}", "第一句。".repeat(80), "Second. ".repeat(80), "第三句。".repeat(80));
        let path = manager
            .request(&Lang::new("zh"), &text, &SpeechProfile::online(0))
            .unwrap();
        assert!(path.exists());
        let calls = manager.online.calls.lock().unwrap().clone();
        assert!(calls.len() > 1);
        assert!(calls.iter().all(|chunk| chunk.chars().count() <= ONLINE_CHUNK_CHARS));
        let rejoined: String = calls.join("");
        for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
            assert!(
                rejoined.chars().any(|candidate| candidate == ch),
                "切块后丢失字符 {ch:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn online_failure_is_an_error_without_touching_cards() {
        let dir = temp_dir("fail");
        let manager = manager(true, dir.clone());
        assert!(manager
            .request(&Lang::new("en"), "Hello", &SpeechProfile::online(0))
            .is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn voice_choice_prefers_exact_culture_then_same_language() {
        let voices = vec![
            voice("English", "en-US"),
            voice("Huihui", "zh-CN"),
            voice("Haruka", "ja-JP"),
            voice("Yaoyao", "zh-TW"),
        ];
        assert_eq!(
            choose_voice(&Lang::new("zh"), &voices).unwrap().id,
            "Huihui"
        );
        assert_eq!(
            choose_voice(&Lang::new("ja"), &voices).unwrap().id,
            "Haruka"
        );
        assert_eq!(
            choose_voice(&Lang::new("en"), &voices).unwrap().id,
            "English"
        );
    }

    #[test]
    fn a_voice_with_a_region_variant_matches_by_primary_language() {
        let voices = vec![voice("Yaoyao", "zh-TW")];
        assert_eq!(
            choose_voice(&Lang::new("zh"), &voices).unwrap().id,
            "Yaoyao"
        );
    }

    #[test]
    fn disabled_or_missing_voices_are_not_used_as_a_fallback() {
        let mut disabled = voice("Huihui", "zh-CN");
        disabled.enabled = false;
        assert!(choose_voice(&Lang::new("zh"), &[disabled]).is_none());
        assert!(choose_voice(&Lang::new("zh"), &[voice("English", "en-US")]).is_none());
        assert!(choose_voice(&Lang::new("ja"), &[]).is_none());
    }

    #[test]
    fn short_text_stays_in_one_chunk() {
        assert_eq!(chunk_text("你好", 200), vec!["你好".to_string()]);
        assert!(chunk_text("   ", 200).is_empty());
    }

    #[test]
    fn chunking_packs_sentences_and_keeps_every_non_space_character() {
        let text = "第一句。第二句！Third? Fourth!";
        let chunks = chunk_text(text, 12);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 12));
        let rejoined: String = chunks.join("");
        for ch in text.chars().filter(|ch| !ch.is_whitespace()) {
            assert!(rejoined.contains(ch));
        }
    }

    #[test]
    fn a_single_overlong_sentence_is_hard_split() {
        let text = "a".repeat(25);
        let chunks = chunk_text(&text, 10);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 10));
    }
}
