//! UI Automation（UIA）只读选区：确认「确实有非空文本选区」，并给出文本与屏幕矩形。
//!
//! 这一段只在 Windows 上存在。所有 COM 调用都发生在调用方的 STA 线程里（当前是
//! `selection_watch` 的消息循环线程），查询前会设置连接/事务超时，避免目标应用卡住时
//! 无限等待。查询本身不写剪贴板、不改选区、不激活任何窗口。
//!
//! 读取按「最可靠 → 最兜底」逐级尝试，任何一级拿到非空选区就立即返回：
//! 1. 当前焦点元素的 TextPattern（传统输入场景；Chromium 把 TextPattern 挂在文档根）。
//! 2. 松手点下的元素 TextPattern（选区可能位于未获得焦点的内嵌文档）。
//! 3. 松手点元素的祖先链（浏览器/编辑器把 TextPattern 挂在文档根，点中的是深层叶子）。
//! 4. 传统 Win32 编辑控件（Edit/RichEdit/WinForms）：UIA 常没有 TextPattern，
//!    改用 `EM_GETSEL` + `WM_GETTEXT` 读取选区；密码控件与超高偏移一律跳过。
//!
//! 失败原因会被结构化返回（不含任何文本内容），由监听线程按 stage/reason 记录，
//! 便于判定「是应用不支持、还是没选中、还是查询超时」。

use bead_core::selection::{Point, Rect, MAX_SELECTION_CHARS};

use windows::core::Interface;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationElement, IUIAutomationTextPattern,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, UIA_TextPatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetParent, GetWindowLongPtrW, SendMessageTimeoutW, WindowFromPoint, ES_PASSWORD,
    GWL_STYLE, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_GETTEXT, WM_GETTEXTLENGTH,
};

/// `EM_GETSEL`：windows 0.62 的元数据没有生成这个常量。
const EM_GETSEL: u32 = 0x00B0;
/// 单次向目标控件发消息的最长等待；目标无响应时立即放弃，不挂住监听线程。
const SEND_MESSAGE_TIMEOUT_MS: u32 = 50;
/// 选区偏移上限：超过这个位置既超出 `accept_selection` 的准入，也不值得跨进程搬运。
const MAX_EDIT_SELECTION_OFFSET: usize = 20_000;
/// 从松手点元素向上爬的最大层数。浏览器文档根通常在 3–6 层内。
const MAX_ANCESTOR_WALK: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionSnapshot {
    pub text: String,
    pub rects: Vec<Rect>,
    pub covers_document: bool,
}

/// 一次读取失败的结构化原因。只描述「哪一步失败」，不含任何选区内容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionFailure {
    /// 拿不到当前焦点元素（可能无焦点或目标无响应）。
    NoFocusedElement,
    /// 松手点下取不到元素（点到了桌面/不可达区域）。
    NoPointElement,
    /// 元素及其祖先链都不支持 TextPattern。
    NoTextPattern,
    /// 支持 TextPattern 但 `GetSelection` 为空或查询失败。
    NoSelection,
    /// 有选区但文本为空/全空白。
    EmptySelection,
}

impl SelectionFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoFocusedElement => "no_focused_element",
            Self::NoPointElement => "no_point_element",
            Self::NoTextPattern => "no_text_pattern",
            Self::NoSelection => "no_selection",
            Self::EmptySelection => "empty_selection",
        }
    }

    /// 是否值得提示「这个应用不支持自动划词」。空选区只说明没选中，不需要提示。
    pub fn means_app_unsupported(self) -> bool {
        matches!(self, Self::NoTextPattern)
    }
}

/// 记录候选取值的失败原因：`saw_text_pattern` 决定最终报「不支持」还是「没选中」。
#[derive(Default)]
struct FailureTracker {
    saw_text_pattern: bool,
    last: Option<SelectionFailure>,
}

impl FailureTracker {
    /// 只有「拿到 TextPattern 之后才可能报出」的原因才算「应用支持」。
    /// `NoFocusedElement` / `NoPointElement` 说明查询没走到控件，不能据此判断支持性。
    fn record(&mut self, reason: SelectionFailure) {
        if matches!(
            reason,
            SelectionFailure::NoSelection | SelectionFailure::EmptySelection
        ) {
            self.saw_text_pattern = true;
        }
        self.last = Some(reason);
    }

    /// 最终失败码：元素支持 TextPattern 但这次没有可翻译选区时，必须报 `NoSelection`
    /// （不是「应用不支持」），否则会误提示用户换用热键。
    fn outcome(&self) -> SelectionFailure {
        if self.saw_text_pattern {
            SelectionFailure::NoSelection
        } else {
            self.last.unwrap_or(SelectionFailure::NoTextPattern)
        }
    }
}

pub struct UiaClient {
    automation: IUIAutomation,
}

impl UiaClient {
    /// 必须在已经初始化 COM 的线程上调用。重复初始化是安全的。
    pub fn new() -> Option<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
                // 目标应用无响应时尽早失败，绝不把划词线程挂死。
                let _ = automation2.SetConnectionTimeout(200);
                let _ = automation2.SetTransactionTimeout(200);
            }
            Some(Self { automation })
        }
    }

    /// 鼠标路径：焦点元素 → 松手点元素 → 点选元素的祖先链 → 传统 Edit 控件。
    ///
    /// 注意不能「焦点元素取不到就整体失败」：Chromium 里焦点可能在地址栏/外框，
    /// 而真正的选区在松手点下的文档里；反过来，内嵌文档也可能没有焦点。
    pub fn read_selection(&self, release: Point) -> Result<SelectionSnapshot, SelectionFailure> {
        let mut tracker = FailureTracker::default();

        // 1) 当前焦点元素。
        match unsafe { self.automation.GetFocusedElement() } {
            Ok(element) => match unsafe { self.snapshot_of(&element) } {
                Ok(snapshot) => return Ok(snapshot),
                Err(reason) => tracker.record(reason),
            },
            Err(_) => tracker.record(SelectionFailure::NoFocusedElement),
        }

        // 2) 松手点下的元素。
        let point = POINT {
            x: release.x as i32,
            y: release.y as i32,
        };
        match unsafe { self.automation.ElementFromPoint(point) } {
            Ok(element) => {
                match unsafe { self.snapshot_of(&element) } {
                    Ok(snapshot) => return Ok(snapshot),
                    Err(reason) => tracker.record(reason),
                }
                // 3) 祖先链：Chromium/多数编辑器把 TextPattern 挂在文档根节点上。
                match unsafe { self.walk_ancestors_for_selection(&element) } {
                    Ok(snapshot) => return Ok(snapshot),
                    Err(reason) => tracker.record(reason),
                }
            }
            Err(_) => tracker.record(SelectionFailure::NoPointElement),
        }

        // 4) 传统 Win32 编辑控件：UIA 无 TextPattern 时仍可从控件消息读到选区。
        if let Some(snapshot) = unsafe { legacy_edit_selection(point) } {
            return Ok(snapshot);
        }

        Err(tracker.outcome())
    }

    /// 键盘选区（Shift+方向键、Ctrl+A）没有松手点，只认当前焦点元素及其祖先。
    ///
    /// 不退回鼠标位置取元素：键盘操作时鼠标可能停在别处，按位置取会读到无关选区。
    pub fn read_focused_selection(&self) -> Result<SelectionSnapshot, SelectionFailure> {
        let element = unsafe { self.automation.GetFocusedElement() }
            .map_err(|_| SelectionFailure::NoFocusedElement)?;
        match unsafe { self.snapshot_of(&element) } {
            Ok(snapshot) => Ok(snapshot),
            Err(reason) => {
                if let Ok(snapshot) = unsafe { self.walk_ancestors_for_selection(&element) } {
                    return Ok(snapshot);
                }
                Err(reason)
            }
        }
    }

    unsafe fn snapshot_of(
        &self,
        element: &IUIAutomationElement,
    ) -> Result<SelectionSnapshot, SelectionFailure> {
        let pattern = element
            .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .map_err(|_| SelectionFailure::NoTextPattern)?;
        let array = pattern
            .GetSelection()
            .map_err(|_| SelectionFailure::NoSelection)?;
        if array.Length().unwrap_or(0) <= 0 {
            return Err(SelectionFailure::NoSelection);
        }
        let range = array
            .GetElement(0)
            .map_err(|_| SelectionFailure::NoSelection)?;
        let text = range
            .GetText((MAX_SELECTION_CHARS + 1) as i32)
            .map_err(|_| SelectionFailure::NoSelection)?;
        let text = text.to_string();
        if text.trim().is_empty() {
            return Err(SelectionFailure::EmptySelection);
        }
        let covers_document = document_is_selected(&pattern, &range);
        let rects = range
            .GetBoundingRectangles()
            .ok()
            .map(|array| rects_from_safe_array(array))
            .unwrap_or_default();
        Ok(SelectionSnapshot {
            text,
            rects,
            covers_document,
        })
    }

    /// 从元素向上找第一个「支持 TextPattern 且选区非空」的祖先。
    ///
    /// 只在当前元素没有 TextPattern/选区时才调用，避免在正常路径上多做 UIA 往返。
    unsafe fn walk_ancestors_for_selection(
        &self,
        element: &IUIAutomationElement,
    ) -> Result<SelectionSnapshot, SelectionFailure> {
        let walker = self
            .automation
            .ControlViewWalker()
            .map_err(|_| SelectionFailure::NoTextPattern)?;
        let mut current = element.clone();
        let mut saw_text_pattern = false;
        for _ in 0..MAX_ANCESTOR_WALK {
            let parent = match walker.GetParentElement(&current) {
                Ok(parent) => parent,
                Err(_) => break,
            };
            match self.snapshot_of(&parent) {
                Ok(snapshot) => return Ok(snapshot),
                Err(reason) => {
                    saw_text_pattern |= !matches!(reason, SelectionFailure::NoTextPattern);
                    current = parent;
                }
            }
        }
        if saw_text_pattern {
            Err(SelectionFailure::NoSelection)
        } else {
            Err(SelectionFailure::NoTextPattern)
        }
    }
}

impl Drop for UiaClient {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

unsafe fn document_is_selected(
    pattern: &IUIAutomationTextPattern,
    selection: &windows::Win32::UI::Accessibility::IUIAutomationTextRange,
) -> bool {
    let Ok(document) = pattern.DocumentRange() else {
        return false;
    };
    let start = selection.CompareEndpoints(
        TextPatternRangeEndpoint_Start,
        &document,
        TextPatternRangeEndpoint_Start,
    );
    let end = selection.CompareEndpoints(
        TextPatternRangeEndpoint_End,
        &document,
        TextPatternRangeEndpoint_End,
    );
    matches!((start, end), (Ok(0), Ok(0)))
}

/// SAFEARRAY 里是若干组 (left, top, width, height) 的 f64，按屏幕物理像素给出。
unsafe fn rects_from_safe_array(array: *mut SAFEARRAY) -> Vec<Rect> {
    if array.is_null() {
        return Vec::new();
    }
    let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
    if SafeArrayAccessData(array, &mut data).is_err() || data.is_null() {
        let _ = SafeArrayDestroy(array);
        return Vec::new();
    }
    let lower = SafeArrayGetLBound(array, 1).unwrap_or(0);
    let upper = SafeArrayGetUBound(array, 1).unwrap_or(-1);
    let count = (upper - lower + 1).max(0) as usize;
    let values = data as *const f64;
    let slice = std::slice::from_raw_parts(values, count);
    let rects = rects_from_doubles(slice);
    let _ = SafeArrayUnaccessData(array);
    let _ = SafeArrayDestroy(array);
    rects
}

fn rects_from_doubles(values: &[f64]) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(values.len() / 4);
    for chunk in (0..values.len() / 4).map(|index| index * 4) {
        let left = values[chunk];
        let top = values[chunk + 1];
        let width = values[chunk + 2];
        let height = values[chunk + 3];
        if width > 0.0 && height > 0.0 {
            rects.push(Rect {
                left,
                top,
                right: left + width,
                bottom: top + height,
            });
        }
    }
    rects
}

// --- 传统 Win32 编辑控件兜底（UIA 无 TextPattern 时的最后一层） ---

/// 类名是否可能承载「可读选区」的输入控件。
///
/// 只收窄到有 `EM_GETSEL`/`WM_GETTEXT` 语义的标准控件；自绘控件（DirectUI/CEF/Canvas）
/// 类名不匹配，会直接跳过，不会误读窗口标题或静态文本。
fn is_text_edit_class(class: &str) -> bool {
    let lower = class.to_ascii_lowercase();
    lower == "edit"
        || lower.starts_with("richedit")
        || lower.starts_with("windowsforms10.edit")
        || lower == "trichedit"
}

/// 密码控件不读：即使能取到选区文本，也不该为它弹翻译图标。
fn is_password_style(style: isize) -> bool {
    (style & ES_PASSWORD as isize) != 0
}

/// 把 UTF-16 代码单元区间切成字符串；越界会被夹到缓冲区长度，空选区返回 None。
///
/// 索引是 UTF-16 代码单元而不是字符数：编辑控件的 `EM_GETSEL` 就是这么计的。
fn slice_utf16_selection(
    buffer: &[u16],
    start: usize,
    end: usize,
    total_length: usize,
) -> Option<(String, bool)> {
    let start = start.min(buffer.len());
    let end = end.min(buffer.len());
    if start >= end {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[start..end]);
    if text.trim().is_empty() {
        return None;
    }
    let covers_document = start == 0 && total_length > 0 && end >= total_length;
    Some((text, covers_document))
}

unsafe fn legacy_edit_selection(point: POINT) -> Option<SelectionSnapshot> {
    let mut hwnd = WindowFromPoint(point);
    for _ in 0..4 {
        if hwnd.0.is_null() {
            return None;
        }
        if let Some(snapshot) = read_edit_control(hwnd) {
            return Some(snapshot);
        }
        hwnd = match GetParent(hwnd) {
            Ok(parent) => parent,
            Err(_) => return None,
        };
    }
    None
}

unsafe fn read_edit_control(hwnd: HWND) -> Option<SelectionSnapshot> {
    let class = window_class(hwnd)?;
    if !is_text_edit_class(&class) {
        return None;
    }
    let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
    if is_password_style(style) {
        return None;
    }

    // EM_GETSEL 的低/高 16 位分别给出起点与终点；位置超过上限直接放弃。
    let mut result = 0usize;
    let sent = SendMessageTimeoutW(
        hwnd,
        EM_GETSEL,
        WPARAM(0),
        LPARAM(0),
        SMTO_BLOCK | SMTO_ABORTIFHUNG,
        SEND_MESSAGE_TIMEOUT_MS,
        Some(&mut result),
    );
    if sent.0 == 0 {
        return None;
    }
    let start = (result & 0xFFFF) as usize;
    let end = ((result >> 16) & 0xFFFF) as usize;
    if start >= end || end > MAX_EDIT_SELECTION_OFFSET {
        return None;
    }

    let mut length = 0usize;
    let sent = SendMessageTimeoutW(
        hwnd,
        WM_GETTEXTLENGTH,
        WPARAM(0),
        LPARAM(0),
        SMTO_BLOCK | SMTO_ABORTIFHUNG,
        SEND_MESSAGE_TIMEOUT_MS,
        Some(&mut length),
    );
    if sent.0 == 0 {
        return None;
    }

    // 只搬运到选区终点为止，避免把长文档整段拉进本进程。
    let capacity = end + 1;
    let mut buffer = vec![0u16; capacity];
    let mut copied = 0usize;
    let sent = SendMessageTimeoutW(
        hwnd,
        WM_GETTEXT,
        WPARAM(capacity),
        LPARAM(buffer.as_mut_ptr() as isize),
        SMTO_BLOCK | SMTO_ABORTIFHUNG,
        SEND_MESSAGE_TIMEOUT_MS,
        Some(&mut copied),
    );
    if sent.0 == 0 {
        return None;
    }
    if copied == 0 {
        copied = capacity;
    }
    let (text, covers_document) = slice_utf16_selection(&buffer, start, end.min(copied), length)?;
    Some(SelectionSnapshot {
        text,
        // 传统控件拿不到屏幕矩形：由调用方按松手点兜底定位。
        rects: Vec::new(),
        covers_document,
    })
}

unsafe fn window_class(hwnd: HWND) -> Option<String> {
    let mut buffer = [0u16; 128];
    let length = GetClassNameW(hwnd, &mut buffer);
    if length <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_array_values_are_grouped_into_rects_and_zero_sized_rects_are_dropped() {
        let values = [10.0, 20.0, 30.0, 40.0, 5.0, 6.0, 0.0, 8.0, 1.0, 2.0, 3.0, 4.0];
        assert_eq!(
            rects_from_doubles(&values),
            vec![
                Rect { left: 10.0, top: 20.0, right: 40.0, bottom: 60.0 },
                Rect { left: 1.0, top: 2.0, right: 4.0, bottom: 6.0 },
            ]
        );
    }

    #[test]
    fn only_standard_text_controls_are_treated_as_legacy_edits() {
        assert!(is_text_edit_class("Edit"));
        assert!(is_text_edit_class("RICHEDIT50W"));
        assert!(is_text_edit_class("RichEdit20W"));
        assert!(is_text_edit_class("WindowsForms10.EDIT.app.0.141b42a_r6_ad1"));
        assert!(!is_text_edit_class("Chrome_WidgetWin_1"));
        assert!(!is_text_edit_class("WeChatMainWndForPC"));
        assert!(!is_text_edit_class("Static"));
        assert!(!is_text_edit_class("Button"));
    }

    #[test]
    fn password_style_is_never_read() {
        assert!(is_password_style(ES_PASSWORD as isize));
        assert!(is_password_style(0x0000_0004 | ES_PASSWORD as isize));
        assert!(!is_password_style(0x0000_0004));
    }

    #[test]
    fn utf16_selection_slices_include_surrogate_pairs_and_clamp_out_of_range() {
        // "A😀B"：A=1 个代码单元，😀=2 个，B=1 个，共 4 个。
        let buffer: Vec<u16> = "A😀B".encode_utf16().collect();
        assert_eq!(slice_utf16_selection(&buffer, 0, 4, 4), Some(("A😀B".into(), true)));
        assert_eq!(slice_utf16_selection(&buffer, 1, 3, 4), Some(("😀".into(), false)));
        // 起止点越界时夹到缓冲区；起止相同则没有选区。
        assert_eq!(slice_utf16_selection(&buffer, 0, 99, 4), Some(("A😀B".into(), true)));
        assert_eq!(slice_utf16_selection(&buffer, 2, 2, 4), None);
        assert_eq!(slice_utf16_selection(&buffer, 4, 5, 4), None);
        // 空白选区不进入翻译链路。
        assert_eq!(slice_utf16_selection(&[b' '.into(), b'\t'.into()], 0, 2, 2), None);
    }

    #[test]
    fn failure_codes_are_stable_and_unsupported_apps_are_flagged() {
        assert_eq!(SelectionFailure::NoFocusedElement.code(), "no_focused_element");
        assert_eq!(SelectionFailure::NoPointElement.code(), "no_point_element");
        assert_eq!(SelectionFailure::NoTextPattern.code(), "no_text_pattern");
        assert_eq!(SelectionFailure::NoSelection.code(), "no_selection");
        assert_eq!(SelectionFailure::EmptySelection.code(), "empty_selection");
        assert!(SelectionFailure::NoTextPattern.means_app_unsupported());
        assert!(!SelectionFailure::EmptySelection.means_app_unsupported());
        assert!(!SelectionFailure::NoSelection.means_app_unsupported());
    }

    #[test]
    fn a_text_pattern_with_no_selection_is_reported_as_missing_selection_not_unsupported() {
        let mut tracker = FailureTracker::default();
        tracker.record(SelectionFailure::NoSelection);
        tracker.record(SelectionFailure::NoTextPattern);
        assert_eq!(tracker.outcome(), SelectionFailure::NoSelection);
        assert!(!tracker.outcome().means_app_unsupported());
    }

    #[test]
    fn a_failed_point_lookup_does_not_claim_the_app_lacks_a_text_pattern() {
        let mut tracker = FailureTracker::default();
        tracker.record(SelectionFailure::NoFocusedElement);
        tracker.record(SelectionFailure::NoPointElement);
        assert_eq!(tracker.outcome(), SelectionFailure::NoPointElement);
        assert!(!tracker.outcome().means_app_unsupported());
    }

    #[test]
    fn a_tracker_without_any_pattern_falls_back_to_unsupported() {
        let mut tracker = FailureTracker::default();
        tracker.record(SelectionFailure::NoTextPattern);
        assert_eq!(tracker.outcome(), SelectionFailure::NoTextPattern);
        assert!(tracker.outcome().means_app_unsupported());
        assert_eq!(FailureTracker::default().outcome(), SelectionFailure::NoTextPattern);
    }
}
