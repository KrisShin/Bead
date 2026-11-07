//! MyMemory：零配置默认引擎。
//!
//! 规则 10 的解释（D29）在这里落地：MyMemory 的 API 按语言对设计，provider 内部逐对
//! 请求，对上层仍是一次 `translate()` 返回全部译文。免费引擎尽力而为：配额告警、
//! 非文本内容都按单卡错误处理，绝不 panic、绝不阻塞其他卡。
//!
//! 语言对并发上限固定 2：串行时总耗时等于各语言之和，两路并发后约等于较慢的那一路；
//! 免费接口没有并发额度，再高只会更快触发 429，所以不做动态放大，也不在任何失败后
//! 重试（429/告警一律只记该卡错误，交给用户显式重试或链的重试上限）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use bead_core::{Lang, ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};
use serde_json::Value;

use crate::transport::{encode_query, HttpMethod, HttpRequest, Transport};

/// 免费接口的并发上限：两路足够把耗时压到较长的一路，再多只会更容易被限流。
pub const MAX_CONCURRENT_PAIRS: usize = 2;

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
        Ok(TranslationSet {
            generation: request.generation,
            items: translate_pairs(&*self.transport, request, MAX_CONCURRENT_PAIRS),
        })
    }
}

/// 需要真正发请求的目标语言：跳过源语言自身（源→源没有意义）并按 code 去重，
/// 其余保持原有顺序。MyMemory 按语言对计费/限流，重复请求纯属浪费配额。
pub fn targets_to_request<'a>(source: &Lang, targets: &'a [Lang]) -> Vec<&'a Lang> {
    let mut required: Vec<&Lang> = Vec::new();
    for target in targets {
        if target == source || required.iter().any(|existing| *existing == target) {
            continue;
        }
        required.push(target);
    }
    required
}

/// 实际并发度：没有目标就不起线程；否则不超过目标数，也不超过上限。
/// 上限为 0 按 1 处理——并发 0 无法完成任何请求。
pub fn worker_count(target_count: usize, limit: usize) -> usize {
    if target_count == 0 {
        0
    } else {
        target_count.min(limit.max(1))
    }
}

fn translate_pairs(
    transport: &dyn Transport,
    request: &SyncPlan,
    limit: usize,
) -> Vec<TranslationItem> {
    let targets = targets_to_request(&request.source_lang, &request.targets);
    let workers = worker_count(targets.len(), limit);
    if workers == 0 {
        return Vec::new();
    }
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<TranslationItem>>> = Mutex::new(vec![None; targets.len()]);
    // 固定数量的 worker 各自从游标取下一个语言对：并发上限是真正的上限，不是「分批等待」，
    // 4 个目标也不会因为分批而排成两轮。
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= targets.len() {
                    break;
                }
                let item = translate_pair(transport, request, targets[index]);
                slots.lock().expect("mymemory slots poisoned")[index] = Some(item);
            });
        }
    });
    slots
        .into_inner()
        .expect("mymemory slots poisoned")
        .into_iter()
        .flatten()
        .collect()
}

fn translate_pair(
    transport: &dyn Transport,
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
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    const EN_OK: &str = r#"{"responseData":{"translatedText":"Hello world"},"responseStatus":200}"#;

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

    /// 按语言对返回不同结果：用来验证单卡失败不影响另一张卡。
    struct PairTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
    }

    impl Transport for PairTransport {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.captured.lock().unwrap().push(request.clone());
            if request.url.contains("langpair=zh%7Cja") {
                Ok(HttpResponse {
                    status: 503,
                    body: r#"{"responseData":{"translatedText":""},"responseStatus":503}"#.into(),
                })
            } else {
                Ok(HttpResponse {
                    status: 200,
                    body: EN_OK.into(),
                })
            }
        }
    }

    /// 两个请求必须同时在途才算通过：串行实现等不到第二个请求，超时后 peak 停在 1。
    struct RendezvousTransport {
        captured: Arc<Mutex<Vec<HttpRequest>>>,
        active: Mutex<usize>,
        wake: Condvar,
        peak: Arc<AtomicUsize>,
    }

    impl Transport for RendezvousTransport {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.captured.lock().unwrap().push(request.clone());
            let mut active = self.active.lock().unwrap();
            *active += 1;
            self.peak.fetch_max(*active, Ordering::SeqCst);
            self.wake.notify_all();
            let (guard, _) = self
                .wake
                .wait_timeout_while(active, Duration::from_secs(1), |in_flight| *in_flight < 2)
                .unwrap();
            active = guard;
            *active -= 1;
            Ok(HttpResponse {
                status: 200,
                body: EN_OK.into(),
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

    fn requested_pairs(captured: &Arc<Mutex<Vec<HttpRequest>>>) -> Vec<String> {
        captured
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request
                    .url
                    .split("langpair=")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn each_target_is_one_language_pair_call_and_the_set_returns_all_translations() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: EN_OK.into(),
        });
        let provider = MyMemoryProvider::new(transport);
        let result = provider.translate(&plan()).unwrap();

        // 语言对并发执行，请求顺序不保证；结果顺序仍按 targets 固定。
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].lang, Lang::new("en"));
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello world"));
        assert_eq!(result.items[1].lang, Lang::new("ja"));
        assert_eq!(result.items[1].text.as_deref(), Ok("Hello world"));
        assert_eq!(result.generation, 3);
        let pairs = requested_pairs(&captured);
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().any(|pair| pair == "zh%7Cen"));
        assert!(pairs.iter().any(|pair| pair == "zh%7Cja"));
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

    #[test]
    fn a_failed_language_pair_only_marks_its_own_card() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(PairTransport {
            captured: Arc::clone(&captured),
        });
        let provider = MyMemoryProvider::new(transport);
        let result = provider.translate(&plan()).unwrap();
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello world"));
        assert!(result.items[1].text.is_err());
        assert_eq!(requested_pairs(&captured).len(), 2);
    }

    #[test]
    fn a_rate_limited_pair_is_reported_once_without_retrying() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: r#"{"responseData":{"translatedText":"RATE LIMIT"},"responseStatus":429}"#.into(),
        });
        let provider = MyMemoryProvider::new(transport);
        let result = provider.translate(&plan()).unwrap();
        assert!(result.items[0].text.is_err());
        assert!(result.items[1].text.is_err());
        // 每个语言对只发一次：429 不在 provider 内部重试，避免打爆免费配额。
        assert_eq!(captured.lock().unwrap().len(), 2);
    }

    #[test]
    fn two_language_pairs_are_in_flight_at_the_same_time() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let peak = Arc::new(AtomicUsize::new(0));
        let transport = Box::new(RendezvousTransport {
            captured: Arc::clone(&captured),
            active: Mutex::new(0),
            wake: Condvar::new(),
            peak: Arc::clone(&peak),
        });
        let provider = MyMemoryProvider::new(transport);
        let result = provider.translate(&plan()).unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_source_language_is_never_requested_and_duplicates_collapse_to_one_pair() {
        let source = Lang::new("zh");
        let targets = vec![
            Lang::new("en"),
            Lang::new("zh"),
            Lang::new("ja"),
            Lang::new("en"),
        ];
        let required: Vec<&str> = targets_to_request(&source, &targets)
            .iter()
            .map(|lang| lang.code())
            .collect();
        assert_eq!(required, vec!["en", "ja"]);
    }

    #[test]
    fn concurrency_is_capped_at_two_even_with_more_targets() {
        assert_eq!(worker_count(0, MAX_CONCURRENT_PAIRS), 0);
        assert_eq!(worker_count(1, MAX_CONCURRENT_PAIRS), 1);
        assert_eq!(worker_count(2, MAX_CONCURRENT_PAIRS), 2);
        assert_eq!(worker_count(4, MAX_CONCURRENT_PAIRS), 2);
        assert_eq!(worker_count(3, 0), 1);
    }

    #[test]
    fn an_empty_target_list_makes_no_network_call() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let transport = Box::new(FakeTransport {
            captured: Arc::clone(&captured),
            response: EN_OK.into(),
        });
        let provider = MyMemoryProvider::new(transport);
        let request = SyncPlan {
            targets: Vec::new(),
            ..plan()
        };
        let result = provider.translate(&request).unwrap();
        assert!(result.items.is_empty());
        assert!(captured.lock().unwrap().is_empty());
    }
}
