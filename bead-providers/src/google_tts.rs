//! 在线 TTS：Google translate_tts（无 key、免费）。
//!
//! 本网络该域名实测不可达（见 STATE.md Facts）——这正好构成「在线失败自动降级离线」
//! 的真实路径；可联网环境则直接可用。音频字节经 ByteTransport 获取（与 JSON provider
//! 的 Transport 分开，避免文本化破坏二进制）。

use bead_core::{Lang, SpeechError};

use crate::transport::{encode_query, HttpRequest};

/// 抓取音频字节的传输抽象（ureq 实现返回原始字节）。
pub trait ByteTransport: Send {
    fn fetch_bytes(&self, request: &HttpRequest) -> Result<Vec<u8>, SpeechError>;
}

pub struct UreqByteTransport;

impl ByteTransport for UreqByteTransport {
    fn fetch_bytes(&self, request: &HttpRequest) -> Result<Vec<u8>, SpeechError> {
        // 朗读是即时交互：网络不可达时宁可 2.5s 后走离线 SAPI，也不要让用户干等。
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
    /// 抓取一段语音。文本长度上限 200 字符（Google 端点限制），超长由调用方分段或截断。
    pub fn fetch(
        transport: &dyn ByteTransport,
        language: &Lang,
        text: &str,
    ) -> Result<Vec<u8>, SpeechError> {
        let clipped: String = text.chars().take(200).collect();
        let url = format!(
            "https://translate.googleapis.com/translate_tts?ie=UTF-8&client=tw-ob&tl={}&q={}",
            encode_query(language.code()),
            encode_query(&clipped),
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
    use std::cell::RefCell;
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
}
