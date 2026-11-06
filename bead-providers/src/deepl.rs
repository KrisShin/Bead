//! DeepL Free：按目标语言逐对请求（D29 解释适用），免费端点 api-free.deepl.com。
//!
//! key 不写入日志；错误直接透传 provider 的 JSON 说明。

use bead_core::{ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::{json, Value};

use crate::transport::{HttpMethod, HttpRequest, Transport};

pub struct DeepLProvider {
    transport: Box<dyn Transport>,
    api_key: String,
}

impl DeepLProvider {
    pub fn new(transport: Box<dyn Transport>, api_key: String) -> Self {
        Self { transport, api_key }
    }
}

fn deepl_code(code: &str) -> String {
    code.to_uppercase()
}

impl Translator for DeepLProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            let body = json!({
                "text": [request.source_text],
                "target_lang": deepl_code(target.code()),
                "source_lang": deepl_code(request.source_lang.code()),
            })
            .to_string();
            let request = HttpRequest {
                method: HttpMethod::Post,
                url: "https://api-free.deepl.com/v2/translate".into(),
                headers: vec![
                    (
                        "Authorization".into(),
                        format!("DeepL-Auth-Key {}", self.api_key),
                    ),
                    ("Content-Type".into(), "application/json".into()),
                ],
                body: Some(body),
            };
            let item = match self.transport.send(&request) {
                Ok(response) if response.status == 200 => {
                    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    let text = value
                        .get("translations")
                        .and_then(|t| t.get(0))
                        .and_then(|t| t.get("text"))
                        .and_then(|t| t.as_str())
                        .map(String::from);
                    match text {
                        Some(text) => Ok(text),
                        None => Err(ProviderError("DeepL 响应缺少译文".into())),
                    }
                }
                Ok(response) => Err(ProviderError(format!(
                    "DeepL 请求失败（HTTP {}）：{}",
                    response.status,
                    &response.body[..response.body.len().min(160)]
                ))),
                Err(error) => Err(error),
            };
            items.push(TranslationItem {
                lang: target.clone(),
                text: item,
            });
        }
        Ok(TranslationSet {
            generation: request.generation,
            items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpResponse;
    use bead_core::{Lang, SyncPlan};
    use std::sync::{Arc, Mutex};
    
    

    struct FakeTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
        status: u16,
        response: String,
    }

    impl Transport for FakeTransport {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.captured.lock().unwrap().push(request.clone());
            Ok(HttpResponse {
                status: self.status,
                body: self.response.clone(),
            })
        }
    }

    fn plan() -> SyncPlan {
        SyncPlan {
            generation: 1,
            source_lang: Lang::new("zh"),
            source_text: "你好".into(),
            targets: vec![Lang::new("en"), Lang::new("ja")],
            created_at_ms: 0,
        }
    }

    #[test]
    fn each_target_gets_an_uppercased_language_pair_against_the_free_endpoint() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            status: 200,
            response: r#"{"translations":[{"detected_source_language":"ZH","text":"Hello"}]}"#.into(),
        });
        let provider = DeepLProvider::new(transport, "key".into());
        let result = provider.translate(&plan()).unwrap();

        assert_eq!(result.items[0].text.as_deref(), Ok("Hello"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.contains("api-free.deepl.com"));
        assert!(requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v.contains("DeepL-Auth-Key")));
        assert!(requests[0].body.as_ref().unwrap().contains("\"target_lang\":\"EN\""));
        assert!(requests[1].body.as_ref().unwrap().contains("\"target_lang\":\"JA\""));
    }

    #[test]
    fn a_403_response_becomes_a_card_error_with_the_provider_message() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            status: 403,
            response: r#"{"message":"Wrong key"}"#.into(),
        });
        let provider = DeepLProvider::new(transport, "bad".into());
        let result = provider.translate(&plan()).unwrap();
        let message = result.items[0].text.as_ref().unwrap_err().0.clone();
        assert!(message.contains("403") && message.contains("Wrong key"));
    }
}
