//! 火山引擎翻译：HMAC-SHA256 v4 风格签名，单次请求可携带多文本（TextList）。
//!
//! 与规则 10 对齐：一次请求的 TextList 就是全部原文（这里只有一个原文），目标语言逐对调用。

use bead_core::{ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::{json, Value};

use crate::signing::{hmac_sha256_hex, hmac_sha256_raw, sha256_hex, utc_parts};
use crate::transport::{HttpMethod, HttpRequest, Transport};

const HOST: &str = "translate.volcengineapi.com";
const REGION: &str = "cn-north-1";
const SERVICE: &str = "translate";

pub struct VolcProvider {
    transport: Box<dyn Transport>,
    access_key: String,
    secret_key: String,
    now_secs: Box<dyn Fn() -> u64 + Send>,
}

impl VolcProvider {
    pub fn new(
        transport: Box<dyn Transport>,
        access_key: String,
        secret_key: String,
        now_secs: Box<dyn Fn() -> u64 + Send>,
    ) -> Self {
        Self {
            transport,
            access_key,
            secret_key,
            now_secs,
        }
    }
}

impl Translator for VolcProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            let now = (self.now_secs)();
            let (date, x_date, _) = utc_parts(now);
            let body = json!({
                "SourceLanguage": request.source_lang.code(),
                "TargetLanguage": target.code(),
                "TextList": [request.source_text],
            })
            .to_string();
            let body_hash = sha256_hex(body.as_bytes());
            let query = "Action=Translate&Version=2020-06-01";

            // 规范请求：signed headers 按字典序 x-content-sha256;content-type;host;x-date
            let canonical_request = format!(
                "POST\n/\n{query}\nx-content-sha256:{body_hash}\ncontent-type:application/json\nhost:{HOST}\nx-date:{x_date}\n\nx-content-sha256;content-type;host;x-date\n{body_hash}"
            );
            let scope = format!("{date}/{REGION}/{SERVICE}/request");
            let string_to_sign = format!(
                "HMAC-SHA256\n{x_date}\n{scope}\n{}",
                sha256_hex(canonical_request.as_bytes()),
            );
            let k_date = hmac_sha256_raw(self.secret_key.as_bytes(), date.as_bytes());
            let k_region = hmac_sha256_raw(&k_date, REGION.as_bytes());
            let k_service = hmac_sha256_raw(&k_region, SERVICE.as_bytes());
            let k_signing = hmac_sha256_raw(&k_service, b"request");
            let signature = hmac_sha256_hex(&k_signing, string_to_sign.as_bytes());

            let request = HttpRequest {
                method: HttpMethod::Post,
                url: format!("https://{HOST}/?{query}"),
                headers: vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("X-Date".into(), x_date),
                    ("X-Content-Sha256".into(), body_hash),
                    (
                        "Authorization".into(),
                        format!(
                            "HMAC-SHA256 Credential={}/{scope}, SignedHeaders=x-content-sha256;content-type;host;x-date, Signature={signature}",
                            self.access_key
                        ),
                    ),
                ],
                body: Some(body),
            };
            let item = match self.transport.send(&request) {
                Ok(response) => {
                    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    if let Some(error) = value.pointer("/ResponseMetadata/Error") {
                        let code = error.get("Code").and_then(|v| v.as_str()).unwrap_or("?");
                        let message =
                            error.get("Message").and_then(|v| v.as_str()).unwrap_or("?");
                        Err(ProviderError(format!("火山错误 {code}：{message}")))
                    } else {
                        match value
                            .pointer("/TranslationList/0/Translation")
                            .and_then(|v| v.as_str())
                        {
                            Some(text) => Ok(text.to_string()),
                            None => Err(ProviderError("火山响应缺少译文".into())),
                        }
                    }
                }
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
            generation: 1,
            source_lang: Lang::new("zh"),
            source_text: "你好".into(),
            targets: vec![Lang::new("en")],
            created_at_ms: 0,
        }
    }

    #[test]
    fn volc_signature_scope_and_headers_are_deterministic_for_a_fixed_clock() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"TranslationList":[{"Translation":"Hello","DetectedSourceLanguage":"zh"}]}"#.into(),
        });
        let provider =
            VolcProvider::new(transport, "AKLTtest".into(), "secret".into(), Box::new(|| 1_793_923_200));
        let result = provider.translate(&plan()).unwrap();
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello"));

        let request = &captured.lock().unwrap()[0];
        assert!(request.url.contains("Action=Translate&Version=2020-06-01"));
        let auth = request
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(auth.contains("Credential=AKLTtest/2026-11-06/cn-north-1/translate/request"));
        let x_date = request
            .headers
            .iter()
            .find(|(k, _)| k == "X-Date")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert_eq!(x_date, "20261106T000000Z");
    }

    #[test]
    fn volc_api_errors_surface_code_and_message() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"ResponseMetadata":{"Error":{"Code":"InvalidSignature","Message":"签名不匹配"}}}"#.into(),
        });
        let provider = VolcProvider::new(transport, "ak".into(), "sk".into(), Box::new(|| 0));
        let result = provider.translate(&plan()).unwrap();
        let message = result.items[0].text.as_ref().unwrap_err().0.clone();
        assert!(message.contains("InvalidSignature"));
    }
}
