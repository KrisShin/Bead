//! OpenAI 兼容 provider：一次请求携带全部目标语言（规则 10 的正面实现）。
//!
//! 智谱 / 硅基流动 / 自定义只是 base_url 与默认模型不同的同一套协议；模型清单由
//! 运行时探测（shell 层职责），这里只负责「构造请求 → 解析严格 JSON 译文」。
//! 模型输出常被围栏或寒暄包裹，解析前先抠 JSON 对象。

use bead_core::{ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::{json, Value};

use crate::transport::{extract_json_object, HttpMethod, HttpRequest, Transport};

pub struct OpenAiCompatibleProvider {
    transport: Box<dyn Transport>,
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAiCompatibleProvider {
    pub fn new(transport: Box<dyn Transport>, base_url: String, model: String, api_key: String) -> Self {
        Self {
            transport,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            api_key,
        }
    }

    pub fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    /// 运行时探测模型清单（工单 04：不写死模型 id）。
    pub fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let request = HttpRequest {
            method: HttpMethod::Get,
            url: self.models_url(),
            headers: vec![("Authorization".into(), format!("Bearer {}", self.api_key))],
            body: None,
        };
        let response = self.transport.send(&request)?;
        if response.status != 200 {
            return Err(ProviderError(format!(
                "模型清单获取失败（HTTP {}）",
                response.status
            )));
        }
        let value: Value = serde_json::from_str(&response.body)
            .map_err(|error| ProviderError(format!("模型清单解析失败：{error}")))?;
        Ok(value
            .get("data")
            .and_then(|data| data.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("id").and_then(|id| id.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }
}

impl Translator for OpenAiCompatibleProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let target_list = request
            .targets
            .iter()
            .map(|lang| lang.code())
            .collect::<Vec<_>>()
            .join(", ");
        let body = json!({
            "model": self.model,
            "temperature": 0.1,
            "messages": [
                {
                    "role": "system",
                    "content": "你是翻译引擎。把用户给出的原文分别翻译成列出的每种目标语言，只输出一个 JSON 对象，格式 {\"translations\":[{\"lang\":\"<语言代码>\",\"text\":\"<译文>\"}]}，覆盖全部目标语言，不要输出任何其他文字。",
                },
                {
                    "role": "user",
                    "content": format!("目标语言：{}\n原文：{}", target_list, request.source_text),
                },
            ],
        })
        .to_string();

        let http_request = HttpRequest {
            method: HttpMethod::Post,
            url: format!("{}/chat/completions", self.base_url),
            headers: vec![
                ("Authorization".into(), format!("Bearer {}", self.api_key)),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: Some(body),
        };
        let response = self.transport.send(&http_request)?;
        if response.status != 200 {
            return Err(ProviderError(format!(
                "翻译请求失败（HTTP {}）：{}",
                response.status,
                &response.body[..response.body.len().min(200)]
            )));
        }
        let value: Value = serde_json::from_str(&response.body)
            .map_err(|error| ProviderError(format!("响应解析失败：{error}")))?;
        let content = value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|content| content.as_str())
            .ok_or_else(|| ProviderError("响应缺少 choices[0].message.content".into()))?;

        let json_text = extract_json_object(content)
            .ok_or_else(|| ProviderError("模型输出中没有 JSON 对象".into()))?;
        let parsed: Value = serde_json::from_str(json_text)
            .map_err(|error| ProviderError(format!("模型输出 JSON 解析失败：{error}")))?;
        let translations = parsed
            .get("translations")
            .and_then(|t| t.as_array())
            .ok_or_else(|| ProviderError("模型输出缺少 translations 数组".into()))?;

        let lookup = |code: &str| -> Option<String> {
            translations
                .iter()
                .find(|item| {
                    item.get("lang").and_then(|lang| lang.as_str()) == Some(code)
                })
                .and_then(|item| item.get("text").and_then(|text| text.as_str()))
                .map(String::from)
        };
        let items = request
            .targets
            .iter()
            .map(|target| TranslationItem {
                lang: target.clone(),
                text: lookup(target.code()).ok_or_else(|| {
                    ProviderError(format!("模型输出缺少目标语言 {}", target.code()))
                }),
            })
            .collect();
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
            generation: 7,
            source_lang: Lang::new("zh"),
            source_text: "你好世界".into(),
            targets: vec![Lang::new("en"), Lang::new("ja")],
            created_at_ms: 0,
        }
    }

    const CONTENT: &str = "以下是翻译：\n```json\n{\"translations\":[{\"lang\":\"en\",\"text\":\"Hello world\"},{\"lang\":\"ja\",\"text\":\"こんにちは世界\"}]}\n```";

    #[test]
    fn one_request_carries_every_target_language_and_returns_every_translation() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: json!({"choices":[{"message":{"content": CONTENT}}]}).to_string(),
        });
        let provider = OpenAiCompatibleProvider::new(
            transport,
            "https://open.bigmodel.cn/api/paas/v4".into(),
            "glm-4.7-flash".into(),
            "key".into(),
        );
        let result = provider.translate(&plan()).unwrap();

        assert_eq!(result.items[0].text.as_deref(), Ok("Hello world"));
        assert_eq!(result.items[1].text.as_deref(), Ok("こんにちは世界"));
        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1); // 单次请求，不是每目标一次
        assert!(requests[0].body.as_ref().unwrap().contains("en"));
        assert!(requests[0].body.as_ref().unwrap().contains("ja"));
        assert!(requests[0].body.as_ref().unwrap().contains("你好世界"));
    }

    #[test]
    fn a_missing_target_language_is_an_error_for_that_card_only() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: json!({"choices":[{"message":{"content":"{\"translations\":[{\"lang\":\"en\",\"text\":\"Hello\"}]}"}}]}).to_string(),
        });
        let provider = OpenAiCompatibleProvider::new(
            transport,
            "https://api.siliconflow.cn/v1".into(),
            "m".into(),
            "k".into(),
        );
        let result = provider.translate(&plan()).unwrap();
        assert!(result.items[0].text.is_ok());
        assert!(result.items[1].text.is_err());
    }

    #[test]
    fn http_errors_surface_the_provider_body_as_the_error() {
        // FakeTransport 恒返回 200；这里用带状态码的变体校验错误正文。
        struct StatusTransport;
        impl Transport for StatusTransport {
            fn send(&self, _request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
                Ok(HttpResponse {
                    status: 401,
                    body: r#"{"error":"invalid api key"}"#.into(),
                })
            }
        }
        let provider = OpenAiCompatibleProvider::new(
            Box::new(StatusTransport),
            "https://x/v1".into(),
            "m".into(),
            "bad".into(),
        );
        let error = provider.translate(&plan()).unwrap_err();
        assert!(error.0.contains("401"));
    }

    #[test]
    fn model_listing_reads_the_runtime_model_ids() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: json!({"data":[{"id":"glm-4.7-flash"},{"id":"glm-4.6"}]}).to_string(),
        });
        let provider = OpenAiCompatibleProvider::new(
            transport,
            "https://open.bigmodel.cn/api/paas/v4".into(),
            "glm-4.7-flash".into(),
            "key".into(),
        );
        let models = provider.list_models().unwrap();
        assert_eq!(models, vec!["glm-4.7-flash", "glm-4.6"]);
        assert_eq!(captured.lock().unwrap()[0].url, "https://open.bigmodel.cn/api/paas/v4/models");
    }
}
