//! 发音：在线优先、离线自动降级、按（语言, 文本）缓存。
//!
//! 编排逻辑在这里（纯逻辑，注入 fake 即可断言全部路径）；真实实现在别处——
//! 在线抓取走 HTTP，离线发声走系统 TTS（Windows SAPI）。播放分两种：
//! 缓存/下载产物由前端 `<audio>` 播放（返回文件路径），离线 SAPI 自己发声（返回 Played）。
//! 失败只给上层一个错误让它出轻提示，绝不清空卡片状态，也绝不向用户暴露引擎选择。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::document::Lang;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechError {
    OnlineFailed(String),
    OfflineFailed(String),
}

/// 在线 TTS：返回音频字节（mp3）。
pub trait OnlineTts: Send {
    fn fetch(&self, language: &Lang, text: &str) -> Result<Vec<u8>, SpeechError>;
}

/// 离线 TTS：自己发声，不产生文件。
pub trait OfflineSpeaker: Send {
    fn speak(&self, language: &Lang, text: &str) -> Result<(), SpeechError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeechOutcome {
    /// 命中缓存或刚下载完成，前端用该文件路径播放。
    PlayFile(PathBuf),
    /// 离线引擎已自行发声，前端无需动作。
    Played,
}

pub struct SpeechManager<O: OnlineTts, F: OfflineSpeaker> {
    online: O,
    offline: F,
    cache_dir: PathBuf,
}

impl<O: OnlineTts, F: OfflineSpeaker> SpeechManager<O, F> {
    pub fn new(online: O, offline: F, cache_dir: PathBuf) -> Self {
        Self {
            online,
            offline,
            cache_dir,
        }
    }

    pub fn cache_path(&self, language: &Lang, text: &str) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        (language.code(), text).hash(&mut hasher);
        self.cache_dir
            .join(format!("{}-{:016x}.mp3", language.code(), hasher.finish()))
    }

    pub fn request(&self, language: &Lang, text: &str) -> Result<SpeechOutcome, SpeechError> {
        let path = self.cache_path(language, text);
        if Path::new(&path).exists() {
            return Ok(SpeechOutcome::PlayFile(path));
        }
        match self.online.fetch(language, text) {
            Ok(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&path, &bytes) {
                    Ok(()) => Ok(SpeechOutcome::PlayFile(path)),
                    Err(_) => {
                        // 缓存写不进去（磁盘等）：直接播在线字节没有前端通道，降级离线
                        self.offline
                            .speak(language, text)
                            .map(|_| SpeechOutcome::Played)
                    }
                }
            }
            Err(online_error) => self
                .offline
                .speak(language, text)
                .map(|_| SpeechOutcome::Played)
                .map_err(|offline_error| {
                    SpeechError::OnlineFailed(match online_error {
                        SpeechError::OnlineFailed(message) => message,
                        other => format!("{other:?}"),
                    })
                    .max_error(offline_error)
                }),
        }
    }
}

impl SpeechError {
    fn max_error(self, offline: SpeechError) -> SpeechError {
        // 两条路径都失败时，离线失败更贴近用户环境，保留它作为最终错误
        offline
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeOnline {
        fail: bool,
        calls: RefCell<usize>,
        bytes: Vec<u8>,
    }

    impl OnlineTts for FakeOnline {
        fn fetch(&self, _language: &Lang, _text: &str) -> Result<Vec<u8>, SpeechError> {
            *self.calls.borrow_mut() += 1;
            if self.fail {
                Err(SpeechError::OnlineFailed("网络不可达".into()))
            } else {
                Ok(self.bytes.clone())
            }
        }
    }

    struct FakeOffline {
        fail: bool,
        calls: RefCell<usize>,
    }

    impl OfflineSpeaker for FakeOffline {
        fn speak(&self, _language: &Lang, _text: &str) -> Result<(), SpeechError> {
            *self.calls.borrow_mut() += 1;
            if self.fail {
                Err(SpeechError::OfflineFailed("没有语音引擎".into()))
            } else {
                Ok(())
            }
        }
    }

    fn manager(
        online_fail: bool,
        offline_fail: bool,
        dir: PathBuf,
    ) -> SpeechManager<FakeOnline, FakeOffline> {
        SpeechManager::new(
            FakeOnline {
                fail: online_fail,
                calls: RefCell::new(0),
                bytes: b"mp3-bytes".to_vec(),
            },
            FakeOffline { fail: offline_fail, calls: RefCell::new(0) },
            dir,
        )
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bead-speech-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn online_success_downloads_and_caches_the_audio() {
        let dir = temp_dir("online");
        let manager = manager(false, false, dir.clone());
        let outcome = manager.request(&Lang::new("en"), "Hello").unwrap();
        match outcome {
            SpeechOutcome::PlayFile(path) => {
                assert!(path.exists());
                assert_eq!(std::fs::read(&path).unwrap(), b"mp3-bytes");
            }
            other => panic!("expected play file, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_same_text_hits_the_cache_without_a_second_online_request() {
        let dir = temp_dir("cache");
        let manager = manager(false, false, dir.clone());
        let first = manager.request(&Lang::new("en"), "Hello").unwrap();
        let second = manager.request(&Lang::new("en"), "Hello").unwrap();
        assert_eq!(first, second);
        assert_eq!(manager.online.calls.borrow().clone(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn different_languages_cache_separately_for_the_same_text() {
        let dir = temp_dir("langs");
        let manager = manager(false, false, dir.clone());
        let en = manager.request(&Lang::new("en"), "Hello").unwrap();
        let ja = manager.request(&Lang::new("ja"), "Hello").unwrap();
        assert_ne!(en, ja);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_online_failure_falls_back_to_the_offline_speaker() {
        let dir = temp_dir("fallback");
        let manager = manager(true, false, dir.clone());
        let outcome = manager.request(&Lang::new("en"), "Hello").unwrap();
        assert_eq!(outcome, SpeechOutcome::Played);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn both_paths_failing_is_a_light_error_without_touching_cards() {
        let dir = temp_dir("bothfail");
        let manager = manager(true, true, dir.clone());
        assert!(manager.request(&Lang::new("en"), "Hello").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
