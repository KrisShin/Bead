//! 初始落卡的语言检测。
//!
//! 只回答「这段剪贴板文本最像池里的哪种语言」，给不出答案就返回 None，由上层回落主语言卡。
//! 启发式按字符系判别：谚文 → ko，假名 → ja，汉字 → zh（假名优先于汉字，假名是日语的排他特征），
//! 其余一律 None——拉丁系语言（en/fr/de…）彼此无法用字符系区分，宁可落主语言也不瞎猜。
//! 日语纯汉字句会被判成 zh，这是已知边界，够工单 03 的演示与测试；更准的检测属于后续优化。

use crate::document::Lang;

pub trait LanguageDetector: Send {
    fn detect(&self, text: &str) -> Option<Lang>;
}

pub struct HeuristicDetector;

impl LanguageDetector for HeuristicDetector {
    fn detect(&self, text: &str) -> Option<Lang> {
        let mut has_han = false;
        for ch in text.chars() {
            if is_kana(ch) {
                return Some(Lang::new("ja"));
            }
            if is_hangul(ch) {
                return Some(Lang::new("ko"));
            }
            if is_han(ch) {
                has_han = true;
            }
        }
        if has_han {
            return Some(Lang::new("zh"));
        }
        None
    }
}

fn is_kana(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x309F // 平假名
        | 0x30A0..=0x30FF // 片假名
        | 0x31F0..=0x31FF // 片假名音标扩展
    )
}

fn is_hangul(ch: char) -> bool {
    matches!(ch as u32, 0xAC00..=0xD7AF | 0x1100..=0x11FF)
}

fn is_han(ch: char) -> bool {
    matches!(ch as u32,
        0x4E00..=0x9FFF // CJK 统一表意文字
        | 0x3400..=0x4DBF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kana_anywhere_in_the_text_means_japanese() {
        assert_eq!(HeuristicDetector.detect("テストです"), Some(Lang::new("ja")));
        assert_eq!(HeuristicDetector.detect("[ja] テスト"), Some(Lang::new("ja")));
    }

    #[test]
    fn hangul_means_korean_and_han_means_chinese() {
        assert_eq!(HeuristicDetector.detect("안녕하세요"), Some(Lang::new("ko")));
        assert_eq!(HeuristicDetector.detect("你好世界"), Some(Lang::new("zh")));
    }

    #[test]
    fn latin_and_mixed_content_yields_no_answer() {
        assert_eq!(HeuristicDetector.detect("Hello world"), None);
        assert_eq!(HeuristicDetector.detect("D:\\Devices\\rime_sync"), None);
        assert_eq!(HeuristicDetector.detect(""), None);
    }
}
