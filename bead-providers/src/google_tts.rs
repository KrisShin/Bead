//! 在线 TTS：Google translate_tts（无 key、免费）。
//!
//! 只在「该语言没有本地音色」且用户允许在线兜底时才使用；有本地音色时绝不联网。
//! 单次上限 200 字符，超过直接报错——切块是上层 `bead_core::SpeechManager` 的职责，
//! 这里绝不静默截断（否则用户会以为长卡片读完了）。
//!
//! 音频字节经 ByteTransport 获取（与 JSON provider 的 Transport 分开，避免文本化破坏二进制）。

use bead_core::{Lang, SpeechError};

use crate::transport::{encode_query, HttpRequest};

/// 抓取音频字节的传输抽象（ureq 实现返回原始字节）。
pub trait ByteTransport: Send {
    fn fetch_bytes(&self, request: &HttpRequest) -> Result<Vec<u8>, SpeechError>;
}

pub struct UreqByteTransport;

impl ByteTransport for UreqByteTransport {
    fn fetch_bytes(&self, request: &HttpRequest) -> Result<Vec<u8>, SpeechError> {
        // 在线朗读是即时交互：网络不可达时宁可 2.5s 后报错，也不要让用户干等。
        // 翻译链另有自己的 10s 超时，这里不受影响。
        let mut req = ureq::get(&request.url).timeout(std::time::Duration::from_millis(2500));
        for (name, value) in &request.headers {
            req = req.set(name, value);
        }
        let response = req.call().map_err(|error| SpeechError::OnlineFailed(format!("TTS 网络错误：{error}")))?;
        if response.status() != 200 {
            return Err(SpeechError::OnlineFailed(format!(
                "TTS HTTP {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|error| SpeechError::OnlineFailed(format!("TTS 读取失败：{error}")))?;
        Ok(bytes)
    }
}

pub struct GoogleTts;

impl GoogleTts {
    /// 抓取一段语音。文本长度上限 200 字符（Google 端点限制）：超过直接报错，
    /// 由上层切块；这里的任何输入都不会被悄悄截短。
    pub fn fetch(
        transport: &dyn ByteTransport,
        language: &Lang,
        text: &str,
    ) -> Result<Vec<u8>, SpeechError> {
        let char_count = text.chars().count();
        if char_count > 200 {
            return Err(SpeechError::OnlineFailed(format!(
                "在线朗读单次上限 200 字符，收到 {char_count} 字符；调用方应先切块"
            )));
        }
        let url = format!(
            "https://translate.googleapis.com/translate_tts?ie=UTF-8&client=tw-ob&tl={}&q={}",
            encode_query(language.code()),
            encode_query(text),
        );
        let request = HttpRequest {
            method: crate::transport::HttpMethod::Get,
            url,
            headers: vec![(
                "User-Agent".into(),
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64)".into(),
            )],
            body: None,
        };
        transport.fetch_bytes(&request)
    }
}

use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpRequest;
    use std::sync::{Arc, Mutex};

    struct FakeByteTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
        fail: bool,
    }

    impl ByteTransport for FakeByteTransport {
        fn fetch_bytes(&self, request: &HttpRequest) -> Result<Vec<u8>, SpeechError> {
            self.captured.lock().unwrap().push(request.clone());
            if self.fail {
                Err(SpeechError::OnlineFailed("unreachable".into()))
            } else {
                Ok(b"mp3".to_vec())
            }
        }
    }

    #[test]
    fn the_tts_url_carries_language_and_encoded_text() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = FakeByteTransport { captured: Arc::clone(&captured), fail: false };
        let bytes = GoogleTts::fetch(&transport, &Lang::new("en"), "Hello world").unwrap();
        assert_eq!(bytes, b"mp3");
        let url = &captured.lock().unwrap()[0].url;
        assert!(url.contains("tl=en"));
        assert!(url.contains("q=Hello%20world"));
        assert!(url.starts_with("https://translate.googleapis.com/translate_tts"));
    }

    #[test]
    fn transport_failures_map_to_online_failed() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = FakeByteTransport { captured: Arc::clone(&captured), fail: true };
        let error = GoogleTts::fetch(&transport, &Lang::new("en"), "Hi").unwrap_err();
        assert!(matches!(error, SpeechError::OnlineFailed(_)));
    }

    #[test]
    fn overlong_text_is_rejected_instead_of_silently_truncated() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = FakeByteTransport { captured: Arc::clone(&captured), fail: false };
        let text = "a".repeat(201);
        let error = GoogleTts::fetch(&transport, &Lang::new("en"), &text).unwrap_err();
        assert!(matches!(error, SpeechError::OnlineFailed(_)));
        assert!(captured.lock().unwrap().is_empty(), "超长请求不应发出");
    }
}
