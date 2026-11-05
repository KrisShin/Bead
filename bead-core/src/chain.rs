//! Provider 链：按顺序回退、每引擎重试上限、连续失败熔断。
//!
//! 链是纯逻辑：它只持有 `Translator` trait 对象与一个可注入的时钟，HTTP、签名、超时全部
//! 属于 provider 实现（bead-providers）。这样「回退顺序、重试上限、熔断恢复」才能在没有
//! 网络的环境里被逐条断言。
//!
//! 两个刻意的取舍：
//! - 部分成功视为成功。一个引擎返回「en 成功、ja 失败」的集合时不再换引擎重试整个请求，
//!   失败的那张卡保持标错，由用户显式「重试」走 `retry_failed`——否则一次免费引擎的偶发
//!   抖动就会把三张卡全部重打一遍。
//! - 熔断按引擎计数而不是按请求：连续失败达到阈值后该引擎进入冷却，冷却期内直接跳过，
//!   到期自动半开重试。计数在成功时清零。

use crate::document::Lang;
use crate::sync::{Clock, ProviderError, SyncPlan, TranslationItem, TranslationSet, Translator};

#[derive(Clone, Copy, Debug)]
pub struct ChainConfig {
    /// 每个引擎在一次翻译里最多尝试的次数（含首次）。
    pub retry_limit: usize,
    /// 连续失败达到该次数后熔断。
    pub open_after_failures: usize,
    /// 熔断冷却时长（毫秒），到期后半开重试。
    pub cooldown_ms: u64,
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            retry_limit: 2,
            open_after_failures: 3,
            cooldown_ms: 30_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Health {
    Available,
    Open { opened_at_ms: u64 },
}

struct ProviderSlot {
    consecutive_failures: usize,
    health: Health,
}

pub struct ProviderChain<C: Clock> {
    providers: Vec<Box<dyn Translator + Send>>,
    slots: Vec<ProviderSlot>,
    config: ChainConfig,
    clock: C,
}

impl<C: Clock> ProviderChain<C> {
    pub fn new(clock: C, config: ChainConfig) -> Self {
        Self {
            providers: Vec::new(),
            slots: Vec::new(),
            config,
            clock,
        }
    }

    pub fn push(&mut self, provider: Box<dyn Translator + Send>) {
        self.providers.push(provider);
        self.slots.push(ProviderSlot {
            consecutive_failures: 0,
            health: Health::Available,
        });
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub fn translate(&mut self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        let mut last_error: Option<ProviderError> = None;
        for index in 0..self.providers.len() {
            if self.is_cooling(index) {
                continue;
            }
            let mut attempt_error = None;
            for _ in 0..self.config.retry_limit {
                match self.providers[index].translate(request) {
                    Ok(result) => {
                        let slot = &mut self.slots[index];
                        slot.consecutive_failures = 0;
                        slot.health = Health::Available;
                        return Ok(result);
                    }
                    Err(error) => attempt_error = Some(error),
                }
            }
            let error = attempt_error.unwrap_or_else(|| ProviderError("未尝试".into()));
            let slot = &mut self.slots[index];
            slot.consecutive_failures += 1;
            if slot.consecutive_failures >= self.config.open_after_failures {
                slot.health = Health::Open {
                    opened_at_ms: self.clock.now_ms(),
                };
            }
            last_error = Some(error);
        }
        Err(last_error
            .unwrap_or_else(|| ProviderError("provider 链为空".into())))
    }

    fn is_cooling(&mut self, index: usize) -> bool {
        match self.slots[index].health {
            Health::Available => false,
            Health::Open { opened_at_ms } => {
                if self.clock.now_ms().saturating_sub(opened_at_ms) >= self.config.cooldown_ms {
                    // 冷却到期：半开，放行一次尝试；成败由 translate 记账。
                    self.slots[index].health = Health::Available;
                    false
                } else {
                    true
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct FakeClock(Cell<u64>);

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
    }

    /// 前 `fail_times` 次调用失败，其后成功；调用计数经 Rc 与测试共享。
    struct FlakyTranslator {
        calls: Arc<AtomicUsize>,
        fail_times: usize,
    }

    impl Translator for FlakyTranslator {
        fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
            let call = self.calls.load(Ordering::Relaxed);
            self.calls.store(call + 1, Ordering::Relaxed);
            if call < self.fail_times {
                return Err(ProviderError(format!("引擎第 {call} 次调用失败")));
            }
            Ok(TranslationSet {
                generation: request.generation,
                items: vec![TranslationItem {
                    lang: Lang::new("en"),
                    text: Ok("Hello".into()),
                }],
            })
        }
    }

    fn flaky(fail_times: usize) -> (FlakyTranslator, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            FlakyTranslator {
                calls: Arc::clone(&calls),
                fail_times,
            },
            calls,
        )
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

    fn config() -> ChainConfig {
        ChainConfig {
            retry_limit: 2,
            open_after_failures: 3,
            cooldown_ms: 30_000,
        }
    }

    #[test]
    fn a_failing_provider_falls_back_to_the_next_one() {
        let (first, _) = flaky(usize::MAX);
        let (second, _) = flaky(0);
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        chain.push(Box::new(first));
        chain.push(Box::new(second));

        let result = chain.translate(&plan()).unwrap();
        assert_eq!(result.items[0].text.as_deref(), Ok("Hello"));
    }

    #[test]
    fn a_provider_is_retried_up_to_the_limit_before_falling_back() {
        let (first, first_calls) = flaky(usize::MAX);
        let (second, second_calls) = flaky(0);
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        chain.push(Box::new(first));
        chain.push(Box::new(second));

        chain.translate(&plan()).unwrap();
        assert_eq!(first_calls.load(Ordering::Relaxed), 2); // 重试上限 2，不是无限
        assert_eq!(second_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_provider_that_fails_repeatedly_is_skipped_for_the_cooldown() {
        let (first, first_calls) = flaky(usize::MAX);
        let (second, second_calls) = flaky(0);
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        chain.push(Box::new(first));
        chain.push(Box::new(second));

        // 三轮各尝试 2 次 → 连续失败 6 次 ≥ 3，第三轮末熔断
        for _ in 0..3 {
            chain.translate(&plan()).unwrap();
        }
        assert_eq!(first_calls.load(Ordering::Relaxed), 6);

        chain.translate(&plan()).unwrap(); // 冷却期内直接跳过
        assert_eq!(first_calls.load(Ordering::Relaxed), 6);
        assert_eq!(second_calls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn an_open_provider_recovers_after_the_cooldown() {
        let (first, first_calls) = flaky(usize::MAX);
        let (second, _) = flaky(0);
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        chain.push(Box::new(first));
        chain.push(Box::new(second));

        for _ in 0..3 {
            chain.translate(&plan()).unwrap();
        }
        chain.translate(&plan()).unwrap();
        assert_eq!(first_calls.load(Ordering::Relaxed), 6); // 冷却期内仍被跳过

        chain.clock.0.set(1_000 + 30_000); // 冷却到期，半开放行
        chain.translate(&plan()).unwrap();
        assert_eq!(first_calls.load(Ordering::Relaxed), 8); // 重新尝试 2 次
    }

    #[test]
    fn a_success_resets_the_failure_count() {
        // 每次翻译「失败 1 次 + 成功 1 次」：连续计数永远到不了 3，永不熔断
        let (only, only_calls) = flaky(1);
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        chain.push(Box::new(only));

        for _ in 0..10 {
            chain.translate(&plan()).unwrap();
        }
        // 首轮「失败 1 次 + 成功 1 次」2 次调用，其后每次首轮即成功 1 次
        assert_eq!(only_calls.load(Ordering::Relaxed), 11);
        assert_eq!(chain.slots[0].health, Health::Available);
    }

    #[test]
    fn all_providers_failing_reports_an_error() {
        let (first, _) = flaky(usize::MAX);
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        chain.push(Box::new(first));

        assert!(chain.translate(&plan()).is_err());
    }

    #[test]
    fn an_empty_chain_reports_an_error_instead_of_panicking() {
        let mut chain = ProviderChain::new(FakeClock(Cell::new(1_000)), config());
        assert!(chain.translate(&plan()).is_err());
    }
}
