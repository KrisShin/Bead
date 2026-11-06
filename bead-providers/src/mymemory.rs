//! MyMemory：零配置默认引擎。
//!
//! 规则 10 的解释（D29）在这里落地：MyMemory 的 API 按语言对设计，provider 内部逐对
//! 请求，对上层仍是一次 `translate()` 返回全部译文。免费引擎尽力而为：配额告警、
//! 非文本内容都按单卡错误处理，绝不 panic、绝不阻塞其他卡。

use bead_core::{Lang, ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::Value;

use crate::transport::{encode_query, HttpMethod, HttpRequest, Transport};

pub struct MyMemoryProvider {
    transport: Box<dyn Transport>,
}

impl MyMemoryProvider {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self { transport }
    }
}

impl Translator for MyMemoryProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            items.push(translate_pair(&self.transport, request, target));
        }
        Ok(TranslationSet {
            generation: request.generation,
            items,
        })
    }
}

fn translate_pair(
    transport: &Box<dyn Transport>,
    request: &SyncPlan,
    target: &Lang,
) -> TranslationItem {
    let url = format!(
        "https://api.mymemory.translated.net/get?q={}&langpair={}%7C{}",
        encode_query(&request.source_text),
        encode_query(request.source_lang.code()),
        encode_query(target.code()),
    );
    let request = HttpRequest {
        method: HttpMethod::Get,
        url,
        headers: Vec::new(),
        body: None,
    };
    let response = match transport.send(&request) {
        Ok(response) => response,
        Err(error) => {
            return TranslationItem {
                lang: target.clone(),
                text: Err(error),
            }
        }
    };
    let value: Value = match serde_json::from_str(&response.body) {
        Ok(value) => value,
        Err(error) => {
            return TranslationItem {
                lang: target.clone(),
                text: Err(ProviderError(format!("MyMemory 响应解析失败：{error}"))),
            }
        }
    };
    let status = value
        .get("responseStatus")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(0);
    let text = value
        .get("responseData")
        .and_then(|data| data.get("translatedText"))
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    // 配额耗尽时 MyMemory 仍返回 200，把告警文本塞进 translatedText——必须按错误处理。
    if status != 200 || text.is_empty() || text.starts_with("MYMEMORY WARNING") || text.starts_with("QUERY LENGTH LIMIT") {
        TranslationItem {
            lang: target.clone(),
            text: Err(ProviderError(format!(
                "MyMemory 引擎不可用（尽力而为的免费引擎）：{text}"
            ))),
        }
    } else {
        TranslationItem {
            lang: target.clone(),
            text: Ok(text.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HttpResponse;
    use std::sync::{Arc, Mutex};
    
    

    struct FakeTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
        response: String,
    }

    impl Transport for FakeTransport {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.captured.lock().unwrap().push(request.clone());
            Ok(HttpResponse {
                status: 200,
                body: self.response.clone(),
            })
        }
    }

    fn plan() -> SyncPlan {
        SyncPlan {
            generation: 3,
            source_lang: Lang::new("zh"),
            source_text: "你好世界".into(),
            targets: vec![Lang::new("en"), Lang::new("ja")],
            created_at_ms: 0,
        }
    }

    #[test]
    fn each_target_is_one_language_pair_call_and_the_set_returns_all_translations() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"responseData":{"translatedText":"Hello world"},"responseStatus":200}"#.into(),
        });
        let provider = MyMemoryProvider::new(transport);
        let result = provider.translate(&plan()).unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello world"));
        assert_eq!(result.items[1].text.as_deref(), Ok("Hello world"));
        assert_eq!(result.generation, 3);
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].url.contains("q=%E4%BD%A0%E5%A5%BD%E4%B8%96%E7%95%8C"));
        assert!(requests[0].url.contains("langpair=zh%7Cen"));
        assert!(requests[1].url.contains("langpair=zh%7Cja"));
    }

    #[test]
    fn the_quota_warning_is_reported_as_a_card_error_not_as_a_translation() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"responseData":{"translatedText":"MYMEMORY WARNING: YOU USED ALL FREE DAILY QUOTES"},"responseStatus":200}"#.into(),
        });
        let provider = MyMemoryProvider::new(transport);
        let result = provider.translate(&plan()).unwrap();
        assert!(result.items[0].text.is_err());
        assert!(result.items[1].text.is_err());
    }
}
