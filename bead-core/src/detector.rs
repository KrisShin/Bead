//! 初始落卡的语言检测。
//!
//! 只回答「这段剪贴板文本最像池里的哪种语言」，给不出答案就返回 None，由上层回落主语言卡。
//! 启发式按字符系判别：谚文 → ko，假名 → ja，汉字 → zh（假名优先于汉字，假名是日语的排他特征），
//! 拉丁字母占多数的文本 → en（en/fr/de… 无法只用字符系区分，按拉丁系统一猜 en，
//! 若池里没有 en 卡则上层不粘贴——这是刻意的取舍，避免把英文塞进中文卡）。
//! 日语纯汉字句会被判成 zh，拉丁系非英文文本会落到 en，都是已知边界。

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
        // 拉丁系：字母过半是拉丁字母即判 en（纯英文句子/单词能落到 en 卡）
        let mut latin = 0usize;
        let mut other_letters = 0usize;
        for ch in text.chars() {
            if is_latin(ch) {
                latin += 1;
            } else if ch.is_alphabetic() {
                other_letters += 1;
            }
        }
        if latin >= 2 && latin * 2 >= latin + other_letters {
            return Some(Lang::new("en"));
        }
        None
    }
}

fn is_latin(ch: char) -> bool {
    matches!(ch as u32,
        0x0041..=0x005A // A-Z
        | 0x0061..=0x007A // a-z
        | 0x00C0..=0x024F // 带附加符号的拉丁字母
    )
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
    fn latin_text_lands_on_english() {
        assert_eq!(HeuristicDetector.detect("Hello world"), Some(Lang::new("en")));
        assert_eq!(HeuristicDetector.detect("translate this"), Some(Lang::new("en")));
    }

    #[test]
    fn non_latin_without_han_kana_hangul_yields_no_answer() {
        // 西里尔字母：不是 en，池里通常也没有 ru 以外的判断依据，交给上层回落主语言
        assert_eq!(HeuristicDetector.detect("Привет мир"), None);
        assert_eq!(HeuristicDetector.detect(""), None);
    }
}
