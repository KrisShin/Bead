//! 划词热键文本解析：`Ctrl+Alt+Shift+B` → 可注册的组合键。
//!
//! 解析放在纯逻辑层，是为了让「改键」在保存前就能明确失败（`Ctrl+Shift` 缺主键、
//! `Ctrl+B+C` 多个主键、`Ctrl+F25` 越界），而不是把坏配置写进 settings.json，
//! 等到下次启动 RegisterHotKey 才静默失效。系统层只负责把解析结果翻译成 VK 码。

use std::fmt;

/// 默认划词热键。比原来的 Ctrl+Alt+B 多一个 Shift，避开编辑器与 IM 的常用组合。
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Shift+B";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: HotkeyKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyKey {
    /// 大写 A–Z。
    Letter(char),
    /// 0–9。
    Digit(u8),
    /// F1–F24。
    Function(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    Empty,
    UnknownPart,
    MissingKey,
    MultipleKeys,
    MissingModifier,
    UnsupportedKey,
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            HotkeyError::Empty => "快捷键不能为空",
            HotkeyError::UnknownPart => "含有无法识别的键名",
            HotkeyError::MissingKey => "缺少主键（如 B）",
            HotkeyError::MultipleKeys => "只能有一个主键",
            HotkeyError::MissingModifier => "至少需要一个修饰键（Ctrl / Alt / Shift / Win）",
            HotkeyError::UnsupportedKey => "只支持 A–Z、0–9 与 F1–F24",
        };
        f.write_str(message)
    }
}

impl std::error::Error for HotkeyError {}

/// 解析形如 `Ctrl+Alt+Shift+B` 的文本；大小写不敏感，允许 `+` 两侧有空格。
pub fn parse_hotkey(text: &str) -> Result<Hotkey, HotkeyError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(HotkeyError::Empty);
    }
    let mut hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        key: HotkeyKey::Letter('A'),
    };
    let mut key = None;
    for part in trimmed.split('+').map(str::trim) {
        if part.is_empty() {
            return Err(HotkeyError::UnknownPart);
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => hotkey.ctrl = true,
            "alt" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "win" | "windows" => hotkey.win = true,
            _ => {
                if key.is_some() {
                    return Err(HotkeyError::MultipleKeys);
                }
                key = Some(parse_key(part)?);
            }
        }
    }
    let Some(key) = key else {
        return Err(HotkeyError::MissingKey);
    };
    if !(hotkey.ctrl || hotkey.alt || hotkey.shift || hotkey.win) {
        return Err(HotkeyError::MissingModifier);
    }
    hotkey.key = key;
    Ok(hotkey)
}

fn parse_key(part: &str) -> Result<HotkeyKey, HotkeyError> {
    let upper = part.to_ascii_uppercase();
    if upper.len() == 1 {
        let ch = upper.chars().next().expect("length checked");
        if ch.is_ascii_alphabetic() {
            return Ok(HotkeyKey::Letter(ch));
        }
        if let Some(digit) = ch.to_digit(10) {
            return Ok(HotkeyKey::Digit(digit as u8));
        }
        return Err(HotkeyError::UnsupportedKey);
    }
    if let Some(number) = upper.strip_prefix('F') {
        if let Ok(index) = number.parse::<u8>() {
            if (1..=24).contains(&index) {
                return Ok(HotkeyKey::Function(index));
            }
        }
    }
    Err(HotkeyError::UnsupportedKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default_hotkey() {
        assert_eq!(
            parse_hotkey(DEFAULT_HOTKEY),
            Ok(Hotkey {
                ctrl: true,
                alt: true,
                shift: true,
                win: false,
                key: HotkeyKey::Letter('B'),
            })
        );
    }

    #[test]
    fn modifiers_and_letters_are_case_insensitive_and_space_tolerant() {
        assert_eq!(
            parse_hotkey(" ctrl + ALT + shift + b "),
            Ok(Hotkey {
                ctrl: true,
                alt: true,
                shift: true,
                win: false,
                key: HotkeyKey::Letter('B'),
            })
        );
    }

    #[test]
    fn supports_digits_function_keys_and_the_windows_modifier() {
        assert_eq!(
            parse_hotkey("Win+Ctrl+1"),
            Ok(Hotkey {
                ctrl: true,
                alt: false,
                shift: false,
                win: true,
                key: HotkeyKey::Digit(1),
            })
        );
        assert_eq!(
            parse_hotkey("Shift+F5"),
            Ok(Hotkey {
                ctrl: false,
                alt: false,
                shift: true,
                win: false,
                key: HotkeyKey::Function(5),
            })
        );
    }

    #[test]
    fn rejects_incomplete_or_ambiguous_specs_before_they_reach_the_registry() {
        assert_eq!(parse_hotkey("   "), Err(HotkeyError::Empty));
        assert_eq!(parse_hotkey("Ctrl+Shift"), Err(HotkeyError::MissingKey));
        assert_eq!(parse_hotkey("Ctrl+Alt+B+C"), Err(HotkeyError::MultipleKeys));
        assert_eq!(parse_hotkey("Ctrl+Alt+B?"), Err(HotkeyError::UnsupportedKey));
        assert_eq!(parse_hotkey("Ctrl+F25"), Err(HotkeyError::UnsupportedKey));
        assert_eq!(parse_hotkey("B"), Err(HotkeyError::MissingModifier));
        assert_eq!(parse_hotkey("Ctrl++B"), Err(HotkeyError::UnknownPart));
    }
}
