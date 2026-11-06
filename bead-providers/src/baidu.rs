//! 百度翻译开放平台：MD5 签名，按目标语言逐对请求（D29 解释适用）。

use bead_core::{Lang, ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::Value;

use crate::signing::md5_hex;
use crate::transport::{encode_query, HttpMethod, HttpRequest, Transport};

pub struct BaiduProvider {
    transport: Box<dyn Transport>,
    app_id: String,
    app_key: String,
    salt: Box<dyn Fn() -> String + Send>,
}

impl BaiduProvider {
    pub fn new(
        transport: Box<dyn Transport>,
        app_id: String,
        app_key: String,
        salt: Box<dyn Fn() -> String + Send>,
    ) -> Self {
        Self {
            transport,
            app_id,
            app_key,
            salt,
        }
    }
}

impl Translator for BaiduProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            let salt = (self.salt)();
            let sign = md5_hex(
                format!(
                    "{}{}{}{}",
                    self.app_id, request.source_text, salt, self.app_key
                )
                .as_bytes(),
            );
            let body = format!(
                "q={}&from={}&to={}&appid={}&salt={}&sign={}",
                encode_query(&request.source_text),
                encode_query(request.source_lang.code()),
                encode_query(target.code()),
                encode_query(&self.app_id),
                encode_query(&salt),
                sign,
            );
            let request = HttpRequest {
                method: HttpMethod::Post,
                url: "https://fanyi-api.baidu.com/api/trans/vip/translate".into(),
                headers: vec![(
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                )],
                body: Some(body),
            };
            let item = translate_one(&self.transport, target, &request);
            items.push(item);
        }
        Ok(TranslationSet {
            generation: request.generation,
            items,
        })
    }
}

fn translate_one(
    transport: &Box<dyn Transport>,
    target: &Lang,
    request: &HttpRequest,
) -> TranslationItem {
    let response = match transport.send(request) {
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
                text: Err(ProviderError(format!("百度响应解析失败：{error}"))),
            }
        }
    };
    if let Some(code) = value.get("error_code").and_then(|v| v.as_str()) {
        let message = value
            .get("error_msg")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        return TranslationItem {
            lang: target.clone(),
            text: Err(ProviderError(format!("百度错误 {code}：{message}"))),
        };
    }
    let text = value
        .get("trans_result")
        .and_then(|list| list.get(0))
        .and_then(|item| item.get("dst"))
        .and_then(|dst| dst.as_str())
        .map(String::from);
    TranslationItem {
        lang: target.clone(),
        text: text.ok_or_else(|| ProviderError("百度响应缺少译文".into())),
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
            generation: 2,
            source_lang: Lang::new("zh"),
            source_text: "你好".into(),
            targets: vec![Lang::new("en")],
            created_at_ms: 0,
        }
    }

    #[test]
    fn the_signature_covers_appid_text_salt_and_key() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"from":"zh","to":"en","trans_result":[{"src":"你好","dst":"Hello"}]}"#.into(),
        });
        let provider = BaiduProvider::new(
            transport,
            "20261106appid".into(),
            "secret".into(),
            Box::new(|| "42".into()),
        );
        let result = provider.translate(&plan()).unwrap();
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello"));
        let body = captured.lock().unwrap()[0].body.clone().unwrap();
        assert!(body.contains("appid=20261106appid"));
        assert!(body.contains("salt=42"));
        // sign = MD5("20261106appid" + "你好" + "42" + "secret")
        assert!(body.contains(&format!(
            "sign={}",
            md5_hex("20261106appid你好42secret".as_bytes())
        )));
        assert!(body.starts_with("q=%E4%BD%A0%E5%A5%BD"));
    }

    #[test]
    fn api_error_codes_surface_the_provider_message() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"error_code":"54001","error_msg":"Invalid Sign"}"#.into(),
        });
        let provider = BaiduProvider::new(transport, "id".into(), "k".into(), Box::new(|| "1".into()));
        let result = provider.translate(&plan()).unwrap();
        let message = result.items[0].text.as_ref().unwrap_err().0.clone();
        assert!(message.contains("54001") && message.contains("Invalid Sign"));
    }
}
