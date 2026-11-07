//! GitHub Releases 拉取：只读 latest 的 tag_name（工单 09）。
//!
//! 5 秒超时；限流（HTTP 403/429）与离线都归为 Network 错误，由 bead-core 的
//! check_for_update 静默处理。绝不下载产物、绝不触碰自身文件——产品红线。

use bead_core::UpdateError;

use crate::transport::HttpRequest;

pub const RELEASES_API: &str = "https://api.github.com/repos/KrisShin/Bead/releases/latest";

pub struct GitHubReleaseFetcher {
    pub transport: Box<dyn crate::transport::Transport>,
}

impl Default for GitHubReleaseFetcher {
    fn default() -> Self {
        Self {
            transport: Box::new(crate::transport::UreqTransport),
        }
    }
}

impl bead_core::ReleaseFetcher for GitHubReleaseFetcher {
    fn latest_tag(&self) -> Result<String, UpdateError> {
        let request = HttpRequest {
            method: crate::transport::HttpMethod::Get,
            url: RELEASES_API.into(),
            headers: vec![("User-Agent".into(), "Bead-Update-Check".into())],
            body: None,
        };
        let response = self
            .transport
            .send(&request)
            .map_err(|error| UpdateError::Network(error.0))?;
        if response.status != 200 {
            return Err(UpdateError::Network(format!("HTTP {}", response.status)));
        }
        let value: serde_json::Value = serde_json::from_str(&response.body)
            .map_err(|error| UpdateError::Parse(format!("{error}")))?;
        value
            .get("tag_name")
            .and_then(|tag| tag.as_str())
            .map(String::from)
            .ok_or_else(|| UpdateError::Parse("缺少 tag_name".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bead_core::ReleaseFetcher;
    use std::sync::{Arc, Mutex};

    struct FakeTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
        status: u16,
        response: String,
    }

    impl crate::transport::Transport for FakeTransport {
        fn send(
            &self,
            request: &HttpRequest,
        ) -> Result<crate::transport::HttpResponse, bead_core::ProviderError> {
            self.captured.lock().unwrap().push(request.clone());
            Ok(crate::transport::HttpResponse {
                status: self.status,
                body: self.response.clone(),
            })
        }
    }

    #[test]
    fn the_tag_name_is_read_from_the_latest_release_endpoint() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let fetcher = GitHubReleaseFetcher {
            transport: Box::new(FakeTransport {
                captured: Arc::clone(&captured),
                status: 200,
                response: r#"{"tag_name":"v0.2.0","name":"x"}"#.into(),
            }),
        };
        assert_eq!(fetcher.latest_tag().unwrap(), "v0.2.0");
        assert_eq!(captured.lock().unwrap()[0].url, RELEASES_API);
    }

    #[test]
    fn rate_limiting_maps_to_a_network_error_and_stays_silent_upstream() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let fetcher = GitHubReleaseFetcher {
            transport: Box::new(FakeTransport {
                captured: Arc::clone(&captured),
                status: 403,
                response: r#"{"message":"API rate limit exceeded"}"#.into(),
            }),
        };
        assert!(matches!(fetcher.latest_tag(), Err(UpdateError::Network(_))));
    }
}
