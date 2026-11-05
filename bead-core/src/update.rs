//! 更新检查：只比较、只提示（产品红线——绝不下载、绝不安装、绝不替换自身文件）。
//!
//! 版本比较支持语义化版本与预发布 tag（`-beta.1` 一类）：数字段逐段比较，
//! 缺失段按 0 补齐；同为数字相等时按预发布规则——有预发布标识的一方更小。
//! 解析失败的版本段按 0 处理，保证比较永不 panic。
//! 拉取失败静默（返回 Failed，壳层不提示）：检查更新永远不值得打扰用户。

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    Network(String),
    Parse(String),
}

pub trait ReleaseFetcher: Send {
    fn latest_tag(&self) -> Result<String, UpdateError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable(String),
    /// 静默失败：离线、超时、限流都走这里，壳层不提示。
    Failed,
}

/// 解析一个版本号为数字段 + 预发布标识。非法段按 0。
fn parse_version(version: &str) -> (Vec<u64>, Option<String>) {
    let (core, pre) = match version.trim_start_matches('v').split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_string())),
        None => (version.trim_start_matches('v'), None),
    };
    let numbers = core
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect();
    (numbers, pre)
}

fn numeric_compare(a: &[u64], b: &[u64]) -> Ordering {
    let len = a.len().max(b.len());
    for index in 0..len {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

/// 语义化版本比较：含预发布规则（`1.0.0-beta` < `1.0.0`；同为预发布按字典序）。
pub fn compare_versions(current: &str, latest: &str) -> Ordering {
    let (current_numbers, current_pre) = parse_version(current);
    let (latest_numbers, latest_pre) = parse_version(latest);
    match numeric_compare(&current_numbers, &latest_numbers) {
        Ordering::Equal => match (current_pre, latest_pre) {
            (None, None) => Ordering::Equal,
            (Some(_), None) => Ordering::Less,    // 预发布 < 正式
            (None, Some(_)) => Ordering::Greater, // 正式 > 预发布
            (Some(a), Some(b)) => a.cmp(&b),
        },
        other => other,
    }
}

/// 检查一次：latest 比当前新 → 提示；相等或更旧 → 不提示；拉取失败 → 静默。
pub fn check<F: ReleaseFetcher>(fetcher: &F, current_version: &str) -> UpdateStatus {
    match fetcher.latest_tag() {
        Ok(latest) => {
            if compare_versions(current_version, &latest) == Ordering::Less {
                UpdateStatus::UpdateAvailable(latest)
            } else {
                UpdateStatus::UpToDate
            }
        }
        Err(_) => UpdateStatus::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeFetcher {
        tag: RefCell<Result<String, UpdateError>>,
    }

    impl ReleaseFetcher for FakeFetcher {
        fn latest_tag(&self) -> Result<String, UpdateError> {
            self.tag.borrow().clone()
        }
    }

    #[test]
    fn equal_versions_do_not_prompt() {
        let fetcher = FakeFetcher { tag: RefCell::new(Ok("v0.1.0".into())) };
        assert_eq!(check(&fetcher, "0.1.0"), UpdateStatus::UpToDate);
    }

    #[test]
    fn a_newer_release_prompts_with_its_tag() {
        let fetcher = FakeFetcher { tag: RefCell::new(Ok("v0.2.1".into())) };
        assert_eq!(
            check(&fetcher, "0.1.0"),
            UpdateStatus::UpdateAvailable("v0.2.1".into())
        );
    }

    #[test]
    fn a_prerelease_tag_orders_below_the_stable_release() {
        assert_eq!(compare_versions("1.0.0-beta.1", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-beta.1"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0-beta.1", "0.1.0-beta.2"), Ordering::Less);
    }

    #[test]
    fn numeric_segments_compare_numerically_not_lexically() {
        assert_eq!(compare_versions("0.9.10", "0.10.0"), Ordering::Less);
        assert_eq!(compare_versions("0.2.0", "0.1.9"), Ordering::Greater);
    }

    #[test]
    fn network_failures_stay_silent() {
        let fetcher = FakeFetcher {
            tag: RefCell::new(Err(UpdateError::Network("timeout".into()))),
        };
        assert_eq!(check(&fetcher, "0.1.0"), UpdateStatus::Failed);
    }

    #[test]
    fn malformed_versions_fall_back_to_zero_segments_without_panicking() {
        assert_eq!(compare_versions("abc", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0.1", "1.0.0"), Ordering::Greater);
    }
}
