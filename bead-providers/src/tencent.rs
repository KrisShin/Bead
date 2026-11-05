//! 腾讯云文本翻译（TMT）：TC3-HMAC-SHA256 签名，单次请求一个目标语言（D29 解释适用）。

use bead_core::{Lang, ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::{json, Value};

use crate::signing::{hmac_sha256_hex, hmac_sha256_raw, sha256_hex, utc_parts};
use crate::transport::{HttpMethod, HttpRequest, HttpResponse, Transport};

const HOST: &str = "tmt.tencentcloudapi.com";
const SERVICE: &str = "tmt";
const ACTION: &str = "TextTranslate";
const VERSION: &str = "2018-03-21";
const REGION: &str = "ap-guangzhou";

pub struct TencentProvider {
    transport: Box<dyn Transport>,
    secret_id: String,
    secret_key: String,
    now_secs: Box<dyn Fn() -> u64 + Send>,
}

impl TencentProvider {
    pub fn new(
        transport: Box<dyn Transport>,
        secret_id: String,
        secret_key: String,
        now_secs: Box<dyn Fn() -> u64 + Send>,
    ) -> Self {
        Self {
            transport,
            secret_id,
            secret_key,
            now_secs,
        }
    }
}

impl Translator for TencentProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            let timestamp = (self.now_secs)();
            let (date, _, _) = utc_parts(timestamp);
            let body = json!({
                "Source": request.source_lang.code(),
                "Target": target.code(),
                "ProjectId": 0,
                "SourceText": request.source_text,
            })
            .to_string();

            // TC3 规范请求：头部按字典序（content-type, host, x-tc-action），action 小写。
            let canonical_request = format!(
                "POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:{HOST}\nx-tc-action:{}\n\ncontent-type;host;x-tc-action\n{}",
                ACTION.to_lowercase(),
                sha256_hex(body.as_bytes()),
            );
            let string_to_sign = format!(
                "TC3-HMAC-SHA256\n{timestamp}\n{date}/{SERVICE}/tc3_request\n{}",
                sha256_hex(canonical_request.as_bytes()),
            );
            let k_date = hmac_sha256_raw(format!("TC3{}", self.secret_key).as_bytes(), date.as_bytes());
            let k_service = hmac_sha256_raw(&k_date, SERVICE.as_bytes());
            let k_signing = hmac_sha256_raw(&k_service, b"tc3_request");
            let signature = hmac_sha256_hex(&k_signing, string_to_sign.as_bytes());

            let request = HttpRequest {
                method: HttpMethod::Post,
                url: format!("https://{HOST}"),
                headers: vec![
                    ("Content-Type".into(), "application/json; charset=utf-8".into()),
                    ("X-TC-Action".into(), ACTION.into()),
                    ("X-TC-Version".into(), VERSION.into()),
                    ("X-TC-Timestamp".into(), timestamp.to_string()),
                    ("X-TC-Region".into(), REGION.into()),
                    (
                        "Authorization".into(),
                        format!(
                            "TC3-HMAC-SHA256 Credential={}/{date}/{SERVICE}/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature={signature}",
                            self.secret_id
                        ),
                    ),
                ],
                body: Some(body),
            };
            let item = match self.transport.send(&request) {
                Ok(response) => {
                    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    if let Some(error) = value.pointer("/Response/Error").filter(|v| v.is_object()) {
                        let code = error.get("Code").and_then(|v| v.as_str()).unwrap_or("?");
                        let message =
                            error.get("Message").and_then(|v| v.as_str()).unwrap_or("?");
                        Err(ProviderError(format!("腾讯错误 {code}：{message}")))
                    } else {
                        match value.pointer("/Response/TargetText").and_then(|v| v.as_str()) {
                            Some(text) => Ok(text.to_string()),
                            None => Err(ProviderError("腾讯响应缺少 TargetText".into())),
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
    fn tc3_headers_and_credential_scope_are_deterministic_for_a_fixed_clock() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"Response":{"RequestId":"x","TargetText":"Hello","Error":null}}"#.into(),
        });
        let provider = TencentProvider::new(transport, "AKIDtest".into(), "secret".into(), Box::new(|| 1_793_923_200));
        let result = provider.translate(&plan()).unwrap();
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello"));

        let request = &captured.lock().unwrap()[0];
        let auth = request
            .headers
            .iter()
            .find(|(k, _)| k == "Authorization")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(auth.starts_with("TC3-HMAC-SHA256 Credential=AKIDtest/2026-11-06/tmt/tc3_request"));
        assert!(auth.contains("SignedHeaders=content-type;host;x-tc-action"));
        assert_eq!(
            request
                .headers
                .iter()
                .find(|(k, _)| k == "X-TC-Timestamp")
                .map(|(_, v)| v.clone())
                .unwrap(),
            "1793923200"
        );
        assert!(request.body.as_ref().unwrap().contains("\"Target\":\"en\""));
    }

    #[test]
    fn api_errors_surface_code_and_message() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"Response":{"Error":{"Code":"AuthFailure.SignatureFailure","Message":"签名错误"}}}"#.into(),
        });
        let provider = TencentProvider::new(transport, "id".into(), "k".into(), Box::new(|| 0));
        let result = provider.translate(&plan()).unwrap();
        let message = result.items[0].text.as_ref().unwrap_err().0.clone();
        assert!(message.contains("AuthFailure.SignatureFailure"));
    }
}
