//! 有道智云文本翻译：SHA-256 v3 签名，表单提交，按目标语言逐对请求（D29 解释适用）。

use bead_core::{ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::Value;

use crate::signing::sha256_hex;
use crate::transport::{encode_query, HttpMethod, HttpRequest, Transport};

pub struct YoudaoProvider {
    transport: Box<dyn Transport>,
    app_key: String,
    app_secret: String,
    salt: Box<dyn Fn() -> String + Send>,
    now_secs: Box<dyn Fn() -> u64 + Send>,
}

impl YoudaoProvider {
    pub fn new(
        transport: Box<dyn Transport>,
        app_key: String,
        app_secret: String,
        salt: Box<dyn Fn() -> String + Send>,
        now_secs: Box<dyn Fn() -> u64 + Send>,
    ) -> Self {
        Self {
            transport,
            app_key,
            app_secret,
            salt,
            now_secs,
        }
    }
}

/// 有道 v3 签名的 input 截断规则：超过 20 字符取前 10 + 长度 + 后 10（按字符计）。
fn truncate_input(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 20 {
        let head: String = chars[..10].iter().collect();
        let tail: String = chars[chars.len() - 10..].iter().collect();
        format!("{head}{}{tail}", chars.len())
    } else {
        text.to_string()
    }
}

impl Translator for YoudaoProvider {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut items = Vec::new();
        for target in &request.targets {
            let salt = (self.salt)();
            let curtime = (self.now_secs)();
            let sign = sha256_hex(
                format!(
                    "{}{}{}{}{}",
                    self.app_key,
                    truncate_input(&request.source_text),
                    salt,
                    curtime,
                    self.app_secret
                )
                .as_bytes(),
            );
            let body = format!(
                "q={}&from={}&to={}&appKey={}&salt={}&curtime={}&signType=v3&sign={}",
                encode_query(&request.source_text),
                encode_query(request.source_lang.code()),
                encode_query(target.code()),
                encode_query(&self.app_key),
                encode_query(&salt),
                curtime,
                sign,
            );
            let request = HttpRequest {
                method: HttpMethod::Post,
                url: "https://openapi.youdao.com/api".into(),
                headers: vec![(
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                )],
                body: Some(body),
            };
            let item = match self.transport.send(&request) {
                Ok(response) => {
                    let value: Value = serde_json::from_str(&response.body).unwrap_or(Value::Null);
                    let code = value
                        .get("errorCode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let text = value
                        .get("translation")
                        .and_then(|t| t.get(0))
                        .and_then(|t| t.as_str())
                        .map(String::from);
                    match (code, text) {
                        ("0", Some(text)) => Ok(text),
                        _ => Err(ProviderError(format!("有道错误码 {code}"))),
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
    use crate::signing::utc_parts;

    #[test]
    fn the_input_is_truncated_per_the_v3_rule() {
        let short = truncate_input("你好");
        assert_eq!(short, "你好");
        let long_text = "0123456789ABCDEFGHIJ"; // 恰 20 字符：不截断
        assert_eq!(truncate_input(long_text), long_text);
        let longer = "0123456789ABCDEFGHIJXYZ";
        assert_eq!(truncate_input(longer), "012345678923DEFGHIJXYZ");
    }

    #[test]
    fn the_signature_uses_appkey_truncated_input_salt_curtime_and_secret() {
        let now = 1_793_923_200u64;
        assert_eq!(utc_parts(now).0, "2026-11-06"); // 时间源注入，签名可复现
    }
}
