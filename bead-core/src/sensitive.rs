//! 敏感内容过滤：剪贴板入口的最后一道闸。
//!
//! 密码、令牌、密钥串这类内容一旦进入 provider 链，就会随请求发往第三方——哪怕译文
//! 也一样。所以过滤发生在链之前（壳层 consume 的读取分支），命中即跳过并提示原因，
//! 原文连日志都不落。用户手动在卡片里输入的内容不过滤：那是用户明确的选择。
//!
//! 启发式取向「宁可多拦」：误拦的代价是少翻一段文本，漏拦的代价是密钥外泄。
//! 已知边界：40+ 连续 base64/hex 串阈值定为 50，避免把 git 提交哈希（40 位）误伤。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensitiveVerdict {
    Allowed,
    Skipped(&'static str),
}

const KEY_PREFIXES: [&str; 12] = [
    "eyJ",           // JWT
    "sk-",           // OpenAI 风格
    "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", // GitHub
    "xoxb-", "xoxp-", "xoxa-", // Slack
    "AKIA", "ASIA",  // AWS
];

const ASSIGNMENT_MARKERS: [&str; 12] = [
    "password=", "password：", "password:",
    "passwd=", "pwd=",
    "token=", "token：", "token:",
    "secret=", "secret：",
    "api_key=", "apikey=",
];

pub struct SensitiveFilter;

impl SensitiveFilter {
    pub fn classify(text: &str) -> SensitiveVerdict {
        // 超长样本：翻译它没有价值，还容易触发引擎配额与限流
        if text.chars().count() > 2000 {
            return SensitiveVerdict::Skipped("内容超过 2000 字符");
        }
        let lowered = text.to_lowercase();
        if KEY_PREFIXES.iter().any(|prefix| text.contains(prefix)) {
            return SensitiveVerdict::Skipped("疑似密钥或令牌前缀");
        }
        if ASSIGNMENT_MARKERS.iter().any(|marker| lowered.contains(marker)) {
            return SensitiveVerdict::Skipped("疑似包含口令或令牌赋值");
        }
        if lowered.contains("bearer ") || lowered.contains("basic ") {
            return SensitiveVerdict::Skipped("疑似包含认证头");
        }
        if has_long_secret_run(text, 50) {
            return SensitiveVerdict::Skipped("疑似长密钥串");
        }
        if looks_like_card_number(text) {
            return SensitiveVerdict::Skipped("疑似银行卡号");
        }
        SensitiveVerdict::Allowed
    }
}

/// ≥ threshold 的连续 base64/hex 形态字符（字母数字加 +/=_-）。
fn has_long_secret_run(text: &str, threshold: usize) -> bool {
    let mut run = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=' | '_' | '-') {
            run += 1;
            if run >= threshold {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// 13–19 位纯数字（允许空格/连字符分隔，形如银行卡号）。
fn looks_like_card_number(text: &str) -> bool {
    let stripped: String = text.chars().filter(|c| !c.is_whitespace() && *c != '-').collect();
    stripped.len() >= 13
        && stripped.len() <= 19
        && stripped.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skipped(text: &str) -> bool {
        matches!(SensitiveFilter::classify(text), SensitiveVerdict::Skipped(_))
    }

    #[test]
    fn jwt_and_api_key_prefixes_are_skipped() {
        assert!(skipped("eyJhbGciOiJIUzI1NiJ9.payload.sig"));
        assert!(skipped("sk-abc123def456"));
        assert!(skipped("ghp_16C7e42F292c6912E7710c838347Ae178B4a"));
        assert!(skipped("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn credential_assignments_are_skipped_regardless_of_case() {
        assert!(skipped("数据库配置 password=hunter2 请查收"));
        assert!(skipped("Token: abc123"));
        assert!(skipped("API_KEY=abcd"));
        assert!(skipped("Authorization: Bearer eyJ..."));
    }

    #[test]
    fn long_secret_runs_are_skipped_but_git_hashes_pass() {
        assert!(skipped(&"aX9_=".repeat(12))); // 60 字符密钥形态
        assert!(!skipped("修复合并冲突，提交 9ae2096bd88ff655510d84e1155e6d5b188a68b5 之后的改动"));
    }

    #[test]
    fn card_number_shapes_are_skipped() {
        assert!(skipped("6222 0212 3456 7890 123"));
        assert!(skipped("4111111111111111"));
    }

    #[test]
    fn normal_text_is_allowed() {
        assert!(matches!(SensitiveFilter::classify("你好世界"), SensitiveVerdict::Allowed));
        assert!(matches!(SensitiveFilter::classify("Hello world, today is nice."), SensitiveVerdict::Allowed));
        assert!(matches!(SensitiveFilter::classify("我的手机号是 13800138000"), SensitiveVerdict::Allowed));
    }

    #[test]
    fn oversized_content_is_skipped() {
        assert!(skipped(&"很长".repeat(1500)));
    }
}
