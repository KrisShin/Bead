//! HTTP 传输抽象：provider 只描述「要发什么请求」，怎么发由实现决定。
//!
//! 单元测试注入 fake transport 回放响应体，请求构造与响应解析因此完全不触网；
//! 真实实现是 `UreqTransport`。所有 provider 共享 10 秒硬超时——重试与熔断由
//! bead-core 的 ProviderChain 负责，这里只做一次干净的调用。
//!
//! 连接复用：`UreqTransport` 的所有实例共用同一个进程级 `ureq::Agent`，从而复用
//! 连接池与 TLS 会话。旧实现每次发送都新建 Agent，每次都要重做握手，MyMemory 逐语言
//! 请求时这部分开销会叠加。连接超时单独收紧到 3 秒：连不上时没必要占满整体超时。
//! 整体超时保持 10 秒（实测单语言 0.59–1.71s，尾部抖动大；5 秒会让偶发慢请求变成
//! 超时→链重试，反而更慢）。

use std::sync::OnceLock;
use std::time::Duration;

use bead_core::ProviderError;

/// 单次请求的整体超时（连接 + 发送 + 读完响应）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 建立连接（含 DNS/TLS）的超时；连不上时快速失败，交给链的重试与熔断处理。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

fn shared_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait Transport: Send + Sync {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError>;
}

#[derive(Default)]
pub struct UreqTransport;

impl Transport for UreqTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
        let agent = shared_agent();
        let mut req = match request.method {
            HttpMethod::Get => agent.get(&request.url),
            HttpMethod::Post => agent.post(&request.url),
        }
        .timeout(REQUEST_TIMEOUT);
        for (name, value) in &request.headers {
            req = req.set(name, value);
        }
        // 4xx/5xx 的响应体携带 provider 的错误说明，必须读出来而不是只报状态码。
        let response = match request.method {
            HttpMethod::Get => req.call(),
            HttpMethod::Post => match &request.body {
                Some(body) => req.send_string(body),
                None => req.call(),
            },
        };
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(ProviderError(format!("网络错误：{error}"))),
        };
        let status = response.status();
        let body = response
            .into_string()
            .map_err(|error| ProviderError(format!("读取响应失败：{error}")))?;
        Ok(HttpResponse { status, body })
    }
}

/// 查询串编码：保留 RFC 3986 unreserved，其余全部转义。
pub fn encode_query(text: &str) -> String {
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
    const QUERY_SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(text, QUERY_SET).to_string()
}

/// 从可能被 ```json 围栏或寒暄包裹的模型输出里抠出 JSON 对象。
pub fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&text[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_extraction_finds_the_object_inside_fences_and_prose() {
        assert_eq!(
            extract_json_object("好的：\n```json\n{\"a\":1}\n```"),
            Some("{\"a\":1}")
        );
        assert_eq!(extract_json_object("{\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(extract_json_object("no json here"), None);
    }

    #[test]
    fn query_encoding_escapes_reserved_characters() {
        assert_eq!(encode_query("你好 world"), "%E4%BD%A0%E5%A5%BD%20world");
        assert_eq!(encode_query("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn every_transport_instance_reuses_the_same_connection_pooled_agent() {
        // 同一进程里多次 `UreqTransport` 收发必须落到同一个 Agent，连接与 TLS 会话才复用。
        assert!(std::ptr::eq(shared_agent(), shared_agent()));
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(3));
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(10));
    }
}
