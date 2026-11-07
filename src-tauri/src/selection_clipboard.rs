//! 剪贴板模拟取词：UIA 拿不到 `TextPattern` 时的兜底路径。
//!
//! 依据 `demo/translatewell.exe` 的静态证据（只导入 user32 的
//! `SendInput` + `OpenClipboard/GetClipboardData/SetClipboardData/EmptyClipboard` +
//! `GetClipboardSequenceNumber`，完全不导入 UIAutomationCore/oleacc），这类应用
//! （Electron/自绘 DirectUI）不需要 UIA：它们自己响应 Ctrl+C，把选区写进剪贴板。
//!
//! 本模块只做「取一次文本」这一件事，不翻译、不弹窗、不记录文本：
//! 1. 先把当前剪贴板完整快照到本进程内存；**快照失败或遇到无法完整恢复的格式
//!    （图片/文件/OLE/延迟渲染）就整次放弃，绝不先按 Ctrl+C**；
//! 2. 用 `SendInput` 注入 Ctrl+C；
//! 3. 用 `GetClipboardSequenceNumber` 判定「复制是否真的发生」；
//! 4. 读文本后按序列号复核：中途被外部改写就放弃恢复，以用户内容为准；
//! 5. 恢复快照（文本类 HGLOBAL 格式可逐字节还原）。
//!
//! 隐私边界：不写日志、不持久化任何文本；短暂占用系统剪贴板是这条兜底路径
//! 不可消除的代价（Windows 剪贴板历史也可能记录这次复制）。

use std::time::{Duration, Instant};

/// UIA 失败后最多等多久出现一次剪贴板序列号变化。
const COPY_WAIT_MS: u64 = 400;
/// 轮询间隔：约 30 次，足够覆盖异步复制又不至于长时间空转。
const COPY_POLL_MS: u64 = 12;
/// 快照总量上限；超过就放弃，避免一张大图把内存与耗时放大。
const MAX_BACKUP_BYTES: usize = 8 * 1024 * 1024;
/// 整个兜底流程的硬超时。剪贴板所有者（延迟渲染）可能挂起，
/// 放到独立线程里执行，监听线程最多等这么久。
const CAPTURE_TIMEOUT_MS: u64 = 1500;

pub const CF_TEXT: u32 = 1;
pub const CF_BITMAP: u32 = 2;
pub const CF_METAFILEPICT: u32 = 3;
pub const CF_TIFF: u32 = 6;
pub const CF_OEMTEXT: u32 = 7;
pub const CF_DIB: u32 = 8;
pub const CF_PALETTE: u32 = 9;
pub const CF_RIFF: u32 = 11;
pub const CF_WAVE: u32 = 12;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_ENHMETAFILE: u32 = 14;
pub const CF_HDROP: u32 = 15;
pub const CF_LOCALE: u32 = 16;
pub const CF_DIBV5: u32 = 17;
pub const CF_OWNERDISPLAY: u32 = 0x0080;

/// 兜底失败原因：全部是稳定代码，可直接进结构化日志与 `selection_debug_state`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackError {
    /// 终端类窗口：Ctrl+C 可能是 SIGINT，必须跳过。
    Terminal,
    /// 剪贴板被别的进程占着，连快照都做不了（因此也没有按 Ctrl+C）。
    ClipboardBusy,
    /// 剪贴板里有图片/文件/OLE 等无法逐字节恢复的格式：整次放弃。
    UnsupportedFormat,
    /// 快照体积超过上限。
    TooLarge,
    /// 注入 Ctrl+C 失败（被 UIPI 拦截或输入队列不可用）。
    SendFailed,
    /// 序列号没变：没有选区，或应用不响应 Ctrl+C。
    NoCopy,
    /// 复制出来的文本为空。
    Empty,
    /// 抓取过程中前台窗口变了（用户切走了）：恢复剪贴板但不出图标。
    ForegroundChanged,
    /// 复制后又检测到外部改写：不恢复，以用户内容为准。
    ExternalRewrite,
    /// 兜底整体超时（剪贴板所有者挂起等）：不再等，也不代表剪贴板已被改坏。
    Timeout,
}

impl FallbackError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::ClipboardBusy => "clipboard_busy",
            Self::UnsupportedFormat => "unsupported_format",
            Self::TooLarge => "oversized",
            Self::SendFailed => "send_failed",
            Self::NoCopy => "no_copy",
            Self::Empty => "empty",
            Self::ForegroundChanged => "foreground_changed",
            Self::ExternalRewrite => "external_rewrite",
            Self::Timeout => "timeout",
        }
    }
}

/// 成功抓到的选区：文本 + 恢复是否成功（恢复失败不影响返回文本，但要进诊断）。
pub struct FallbackCapture {
    pub text: String,
    pub restore_failed: bool,
}

/// 终端窗口类名：这类窗口里 Ctrl+C 等于中断进程，必须跳过。
pub fn is_terminal_window_class(class: &str) -> bool {
    let class = class.trim().to_ascii_lowercase();
    matches!(
        class.as_str(),
        "consolewindowclass"
            | "cascadia_hosting_window_class"
            | "pseudoconsolewindow"
            | "mintty"
            | "alacritty"
            | "org.wezfurlong.wezterm"
            | "virtualconsole"
    )
}

/// 该剪贴板格式能否通过「复制 HGLOBAL 字节」完整恢复。
///
/// 文本类标准格式与自定义注册格式（`>= 0xC000`，惯例是 HGLOBAL）可以；
/// 图片/文件/OLE/owner-display/GDI 句柄一律拒绝——这些无法安全还原，
/// 宁可这次不取词，也不能把用户的剪贴板内容弄丢。
pub fn can_backup_format(format: u32, name: Option<&str>) -> bool {
    match format {
        CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT | CF_LOCALE => true,
        // 图片、文件列表、图元文件、调色板、音频及 GDI 句柄。
        CF_BITMAP | CF_METAFILEPICT | CF_TIFF | CF_DIB | CF_PALETTE | CF_RIFF | CF_WAVE
        | CF_ENHMETAFILE | CF_HDROP | CF_DIBV5 => false,
        // owner-display（延迟投递）、私有段、GDI 对象段。
        CF_OWNERDISPLAY..=0x008F => false,
        0x0200..=0x03FF => false,
        registered if registered >= 0xC000 => {
            name.map(|name| !is_unrestorable_registered_name(name))
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// OLE/文件/图片类注册格式：字节能读到，但还原后对方未必能正确解释，
/// 或本身就是「文件复制」的语义，按不可恢复处理。
pub fn is_unrestorable_registered_name(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if name.starts_with("image/") {
        return true;
    }
    matches!(
        name.as_str(),
        "embed source"
            | "embedded object"
            | "object descriptor"
            | "link source"
            | "link source descriptor"
            | "ole private data"
            | "dataobjectattributes"
            | "dataobjectattributesrequest"
            | "native"
            | "ownerlink"
            | "filename"
            | "filenamew"
            | "png"
            | "gif"
            | "tiff"
    )
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
        GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_C, VK_CONTROL,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    /// 剪贴板快照：格式 id + 原始 HGLOBAL 字节，以及拍快照时的序列号。
    struct ClipboardBackup {
        entries: Vec<(u32, Vec<u8>)>,
        seq: u32,
    }

    impl ClipboardBackup {
        fn capture() -> Result<Self, FallbackError> {
            open_clipboard()?;
            let entries = unsafe { read_all_formats() };
            let seq = unsafe { GetClipboardSequenceNumber() };
            // 无论枚举成功与否都必须关掉，否则后续所有剪贴板操作都会失败。
            let _ = unsafe { CloseClipboard() };
            Ok(Self {
                entries: entries?,
                seq,
            })
        }

        fn restore(&self) -> bool {
            // 剪贴板可能被别的程序短暂占用，重试几次；仍失败就如实上报。
            for _ in 0..4 {
                if unsafe { self.restore_once() } {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            false
        }

        unsafe fn restore_once(&self) -> bool {
            if open_clipboard().is_err() {
                return false;
            }
            let mut ok = unsafe { EmptyClipboard() }.is_ok();
            for (format, bytes) in &self.entries {
                let Ok(memory) = (unsafe {
                    GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))
                }) else {
                    ok = false;
                    continue;
                };
                let pointer = unsafe { GlobalLock(memory) };
                if pointer.is_null() {
                    let _ = unsafe { GlobalFree(Some(memory)) };
                    ok = false;
                    continue;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        pointer.cast::<u8>(),
                        bytes.len(),
                    );
                    let _ = GlobalUnlock(memory);
                }
                // 成功后内存归系统所有，绝不能再 GlobalFree。
                if unsafe { SetClipboardData(*format, Some(HANDLE(memory.0))) }.is_err() {
                    let _ = unsafe { GlobalFree(Some(memory)) };
                    ok = false;
                }
            }
            let _ = unsafe { CloseClipboard() };
            ok
        }
    }

    fn open_clipboard() -> Result<(), FallbackError> {
        for _ in 0..8 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(6));
        }
        Err(FallbackError::ClipboardBusy)
    }

    unsafe fn read_all_formats() -> Result<Vec<(u32, Vec<u8>)>, FallbackError> {
        let mut entries = Vec::new();
        let mut total = 0usize;
        let mut format = 0u32;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            let name = format_name(format);
            if !can_backup_format(format, name.as_deref()) {
                return Err(FallbackError::UnsupportedFormat);
            }
            let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
                continue;
            };
            if handle.is_invalid() {
                continue;
            }
            let memory = HGLOBAL(handle.0);
            let size = unsafe { GlobalSize(memory) };
            if size == 0 {
                continue;
            }
            total = total.saturating_add(size);
            if total > MAX_BACKUP_BYTES {
                return Err(FallbackError::TooLarge);
            }
            let pointer = unsafe { GlobalLock(memory) };
            if pointer.is_null() {
                return Err(FallbackError::ClipboardBusy);
            }
            let bytes =
                unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
            let _ = unsafe { GlobalUnlock(memory) };
            entries.push((format, bytes));
        }
        Ok(entries)
    }

    fn format_name(format: u32) -> Option<String> {
        if format < 0xC000 {
            return None;
        }
        let mut buffer = [0u16; 128];
        let length = unsafe { GetClipboardFormatNameW(format, &mut buffer) };
        if length <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..length as usize]))
    }

    fn key_event(key: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    unsafe fn send_ctrl_c() -> bool {
        let inputs = [
            key_event(VK_CONTROL, false),
            key_event(VK_C, false),
            key_event(VK_C, true),
            key_event(VK_CONTROL, true),
        ];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        sent == inputs.len() as u32
    }

    /// 读剪贴板文本：优先 Unicode，其次 CF_TEXT（按当前 ANSI 代码页解码）。
    fn read_text() -> Option<(String, u32)> {
        let seq = unsafe { GetClipboardSequenceNumber() };
        open_clipboard().ok()?;
        let text = unsafe {
            read_format_text(CF_UNICODETEXT).or_else(|| read_format_text(CF_TEXT))
        };
        let _ = unsafe { CloseClipboard() };
        text.map(|text| (text, seq))
    }

    unsafe fn read_format_text(format: u32) -> Option<String> {
        let handle = unsafe { GetClipboardData(format) }.ok()?;
        if handle.is_invalid() {
            return None;
        }
        let memory = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(memory) };
        if size == 0 {
            return None;
        }
        let pointer = unsafe { GlobalLock(memory) };
        if pointer.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) };
        let text = if format == CF_UNICODETEXT {
            let units = unsafe {
                std::slice::from_raw_parts(pointer.cast::<u16>(), size / 2)
            };
            let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
            Some(String::from_utf16_lossy(&units[..end]))
        } else {
            let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
            let ansi = &bytes[..end];
            let capacity = unsafe {
                MultiByteToWideChar(CP_ACP, Default::default(), ansi, None)
            };
            if capacity <= 0 {
                return None;
            }
            let mut wide = vec![0u16; capacity as usize];
            let written = unsafe {
                MultiByteToWideChar(CP_ACP, Default::default(), ansi, Some(&mut wide))
            };
            if written <= 0 {
                return None;
            }
            wide.truncate(written as usize);
            Some(String::from_utf16_lossy(&wide))
        };
        let _ = unsafe { GlobalUnlock(memory) };
        text
    }

    /// 一次完整的兜底取词。调用方必须先确认窗口不是终端类。
    fn capture_selection_inner() -> Result<FallbackCapture, FallbackError> {
        let backup = ClipboardBackup::capture()?;
        // 快照与这一刻之间若剪贴板已被别人改写，快照就作废；此时绝不能再按 Ctrl+C，
        // 否则恢复时会把用户新复制的内容覆盖掉。
        let seq_before = unsafe { GetClipboardSequenceNumber() };
        if seq_before != backup.seq {
            return Err(FallbackError::ExternalRewrite);
        }
        let foreground_before = unsafe { GetForegroundWindow() };
        if !unsafe { send_ctrl_c() } {
            return Err(FallbackError::SendFailed);
        }

        let deadline = Instant::now() + Duration::from_millis(COPY_WAIT_MS);
        let mut seq_after = seq_before;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(COPY_POLL_MS));
            let now = unsafe { GetClipboardSequenceNumber() };
            if now != seq_before {
                seq_after = now;
                break;
            }
        }
        if seq_after == seq_before {
            // 序列号没变：没有选区（或应用不响应 Ctrl+C）。剪贴板仍是原内容。
            return Err(FallbackError::NoCopy);
        }

        let foreground_after = unsafe { GetForegroundWindow() };
        let foreground_changed = foreground_after.0 != foreground_before.0;

        let Some((text, text_seq)) = read_text() else {
            let _ = backup.restore();
            return Err(FallbackError::Empty);
        };
        // 读完之后再确认一次：复制与读取之间被外部改写，以用户内容为准，不恢复。
        let seq_now = unsafe { GetClipboardSequenceNumber() };
        if seq_now != seq_after || text_seq != seq_after {
            return Err(FallbackError::ExternalRewrite);
        }
        if text.trim().is_empty() {
            let _ = backup.restore();
            return Err(FallbackError::Empty);
        }

        let restore_failed = !backup.restore();
        if foreground_changed {
            return Err(FallbackError::ForegroundChanged);
        }
        Ok(FallbackCapture {
            text,
            restore_failed,
        })
    }

    /// 在独立线程里跑完整流程：剪贴板所有者（延迟渲染）可能挂起，
    /// 监听线程只等一个上限，超时后不再阻塞事件循环。
    pub fn capture_selection() -> Result<FallbackCapture, FallbackError> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(capture_selection_inner());
        });
        receiver
            .recv_timeout(Duration::from_millis(CAPTURE_TIMEOUT_MS))
            .unwrap_or(Err(FallbackError::Timeout))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn capture_selection() -> Result<FallbackCapture, FallbackError> {
        Err(FallbackError::SendFailed)
    }
}

#[cfg(windows)]
pub use imp::capture_selection;

#[cfg(not(windows))]
pub use imp::capture_selection;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_formats_are_restorable_and_bitmaps_and_files_are_not() {
        assert!(can_backup_format(CF_TEXT, None));
        assert!(can_backup_format(CF_OEMTEXT, None));
        assert!(can_backup_format(CF_UNICODETEXT, None));
        assert!(can_backup_format(CF_LOCALE, None));
        assert!(!can_backup_format(CF_BITMAP, None));
        assert!(!can_backup_format(CF_DIB, None));
        assert!(!can_backup_format(CF_DIBV5, None));
        assert!(!can_backup_format(CF_HDROP, None));
        assert!(!can_backup_format(CF_METAFILEPICT, None));
        assert!(!can_backup_format(CF_OWNERDISPLAY, None));
        assert!(!can_backup_format(0x0200, None));
        assert!(!can_backup_format(0x0300, None));
    }

    #[test]
    fn registered_text_formats_are_kept_but_ole_and_files_are_dropped() {
        assert!(can_backup_format(0xC000, Some("HTML Format")));
        assert!(can_backup_format(0xC001, Some("Rich Text Format")));
        assert!(can_backup_format(0xC002, Some("text/html")));
        // 读不到名字的注册格式无法判断，按不可恢复处理。
        assert!(!can_backup_format(0xC003, None));
        assert!(!can_backup_format(0xC004, Some("Embed Source")));
        assert!(!can_backup_format(0xC005, Some("Object Descriptor")));
        assert!(!can_backup_format(0xC006, Some("FileNameW")));
        assert!(!can_backup_format(0xC007, Some("PNG")));
        assert!(!can_backup_format(0xC008, Some("image/png")));
    }

    #[test]
    fn terminal_window_classes_are_skipped_case_insensitively() {
        assert!(is_terminal_window_class("ConsoleWindowClass"));
        assert!(is_terminal_window_class("CASCADIA_HOSTING_WINDOW_CLASS"));
        assert!(is_terminal_window_class("mintty"));
        assert!(is_terminal_window_class(" PseudoConsoleWindow "));
        assert!(!is_terminal_window_class("Chrome_WidgetWin_1"));
        assert!(!is_terminal_window_class("WeChatMainWndForPC"));
        assert!(!is_terminal_window_class(""));
    }

    #[test]
    fn fallback_error_codes_are_stable() {
        assert_eq!(FallbackError::Terminal.code(), "terminal");
        assert_eq!(FallbackError::ClipboardBusy.code(), "clipboard_busy");
        assert_eq!(FallbackError::UnsupportedFormat.code(), "unsupported_format");
        assert_eq!(FallbackError::TooLarge.code(), "oversized");
        assert_eq!(FallbackError::SendFailed.code(), "send_failed");
        assert_eq!(FallbackError::NoCopy.code(), "no_copy");
        assert_eq!(FallbackError::Empty.code(), "empty");
        assert_eq!(
            FallbackError::ForegroundChanged.code(),
            "foreground_changed"
        );
        assert_eq!(FallbackError::ExternalRewrite.code(), "external_rewrite");
        assert_eq!(FallbackError::Timeout.code(), "timeout");
    }
}
