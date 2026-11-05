//! 阿里云机器翻译（Alimt 2018-12-25）：RPC HMAC-SHA1 签名，按目标语言逐对请求（D29 解释适用）。

use bead_core::{Lang, ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::Value;

use crate::signing::{hmac_sha1_base64, utc_parts};
use crate::transport::{encode_query, HttpMethod, HttpRequest, HttpResponse, Transport};

pub struct AliyunProvider {
    transport: Box<dyn Transport>,
    access_key_id: String,
    access_key_secret: String,
    nonce: Box<dyn Fn() -> String + Send>,
    now_secs: Box<dyn Fn() -> u64 + Send>,
}

impl AliyunProvider {
    pub fn new(
        transport: Box<dyn Transport>,
        access_key_id: String,
        access_key_secret: String,
        nonce: Box<dyn Fn() -> String + Send>,
        now_secs: Box<dyn Fn() -> u64 + Send>,
    ) -> Self {
        Self {
            transport,
            access_key_id,
            access_key_secret,
            nonce,
            now_secs,
        }
    }
}

impl Translator for AliyunProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            let timestamp = (self.now_secs)();
            let (_, _, iso) = utc_parts(timestamp);
            // RPC 签名参数按字典序排列
            let mut params: Vec<(String, String)> = vec![
                ("AccessKeyId".into(), self.access_key_id.clone()),
                ("Action".into(), "Translate".into()),
                ("Format".into(), "text".into()),
                ("SignatureMethod".into(), "HMAC-SHA1".into()),
                ("SignatureNonce".into(), (self.nonce)()),
                ("SignatureVersion".into(), "1.0".into()),
                ("SourceLanguage".into(), request.source_lang.code().into()),
                ("SourceText".into(), request.source_text.clone()),
                ("TargetLanguage".into(), target.code().into()),
                ("Timestamp".into(), iso),
                ("Version".into(), "2018-12-25".into()),
            ];
            params.sort();
            let canonical_query = params
                .iter()
                .map(|(k, v)| format!("{}={}", encode_query(k), encode_query(v)))
                .collect::<Vec<_>>()
                .join("&");
            let string_to_sign = format!("POST&{}&{}", encode_query("/"), encode_query(&canonical_query));
            let signature = hmac_sha1_base64(
                format!("{}&", self.access_key_secret).as_bytes(),
                string_to_sign.as_bytes(),
            );
            let body = format!("{canonical_query}&Signature={}", encode_query(&signature));

            let request = HttpRequest {
                method: HttpMethod::Post,
                url: "https://mt.aliyuncs.com/".into(),
                headers: vec![(
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                )],
                body: Some(body),
            };
            let item = match self.transport.send(&request) {
                Ok(response) => {
                    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    if let Some(message) = value.get("Message").and_then(|v| v.as_str()) {
                        let code = value.get("Code").and_then(|v| v.as_str()).unwrap_or("?");
                        Err(ProviderError(format!("阿里云错误 {code}：{message}")))
                    } else {
                        match value.pointer("/Data/Translated").and_then(|v| v.as_str()) {
                            Some(text) => Ok(text.to_string()),
                            None => Err(ProviderError("阿里云响应缺少译文".into())),
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
    fn rpc_params_are_sorted_and_signed_with_hmac_sha1() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"RequestId":"x","Data":{"Translated":"Hello"}}"#.into(),
        });
        let provider = AliyunProvider::new(
            transport,
            "testid".into(),
            "testsecret".into(),
            Box::new(|| "nonce-1".into()),
            Box::new(|| 1_793_923_200),
        );
        let result = provider.translate(&plan()).unwrap();
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello"));

        let body = captured.lock().unwrap()[0].body.clone().unwrap();
        assert!(body.contains("Action=Translate"));
        assert!(body.contains("SignatureNonce=nonce-1"));
        assert!(body.contains("Timestamp=2026-11-06T00%3A00%3A00Z"));
        assert!(body.contains("&Signature="));
        // 参数字典序：AccessKeyId 在 Action 之前
        let access_pos = body.find("AccessKeyId=").unwrap();
        let action_pos = body.find("Action=Translate").unwrap();
        assert!(access_pos < action_pos);
    }

    #[test]
    fn aliyun_api_errors_surface_code_and_message() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"Code":"InvalidAccessKeyId","Message":"密钥不存在"}"#.into(),
        });
        let provider = AliyunProvider::new(
            transport,
            "bad".into(),
            "bad".into(),
            Box::new(|| "n".into()),
            Box::new(|| 0),
        );
        let result = provider.translate(&plan()).unwrap();
        let message = result.items[0].text.as_ref().unwrap_err().0.clone();
        assert!(message.contains("InvalidAccessKeyId"));
    }
}
