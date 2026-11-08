//! 选区锚点图标窗口：一个不激活、不抢焦点、不进任务栏的置顶小窗。
//!
//! 图标只在鼠标松手后出现；出现与点击本窗口都不调用 `set_focus`，点击时原应用仍持有前台。
//! 点击成功消费文本后，由 `main.rs` 的 `anchor_click` 把焦点交给主面板（面板据此不再
//! 受「窗口失焦」收起影响）。窗口生命周期由 `bead-core` 的纯逻辑决定，
//! 这里只执行显示/隐藏与坐标提交。

use bead_core::selection::{place_anchor, Point, Rect, Size, ANCHOR_GAP_PX};

use crate::uia::SelectionSnapshot;

pub const ANCHOR_LABEL: &str = "anchor";
/// 图标逻辑尺寸；实际窗口按选区所在显示器的 scale_factor 放大。
const ANCHOR_LOGICAL_SIZE: f64 = 24.0;

/// 图标点击前的纯门禁：顺序固定，便于在诊断里判定「到底被谁拦下」。
///
/// 顺序：开关关闭 → 面板锁定 → 卡片编辑中 → 没有待消费的选区。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorClickBlock {
    Disabled,
    Locked,
    Editing,
    NoPending,
}

impl AnchorClickBlock {
    pub fn code(self) -> &'static str {
        match self {
            Self::Disabled => "anchor_disabled",
            Self::Locked => "panel_locked",
            Self::Editing => "card_editing",
            Self::NoPending => "pending_missing",
        }
    }
}

pub fn anchor_click_block(
    pending: bool,
    enabled: bool,
    locked: bool,
    editing: bool,
) -> Option<AnchorClickBlock> {
    if !enabled {
        return Some(AnchorClickBlock::Disabled);
    }
    if locked {
        return Some(AnchorClickBlock::Locked);
    }
    if editing {
        return Some(AnchorClickBlock::Editing);
    }
    if !pending {
        return Some(AnchorClickBlock::NoPending);
    }
    None
}

/// 图标窗口显示失败的阶段，用于诊断「图标为什么没出现」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowFailure {
    WindowMissing,
    MonitorMissing,
    PlacementFailed,
    ShowFailed,
}

impl ShowFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::WindowMissing => "window_missing",
            Self::MonitorMissing => "monitor_missing",
            Self::PlacementFailed => "placement_failed",
            Self::ShowFailed => "show_failed",
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, IsWindow, IsWindowVisible, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
    };

    const WS_EX_NOACTIVATE: isize = 0x0800_0000;
    const WS_EX_TOOLWINDOW: isize = 0x0000_0080;

    pub fn create(app: &AppHandle) -> Option<WebviewWindow> {
        let window = WebviewWindowBuilder::new(app, ANCHOR_LABEL, WebviewUrl::App("anchor.html".into()))
            .title("Bead selection anchor")
            .inner_size(ANCHOR_LOGICAL_SIZE, ANCHOR_LOGICAL_SIZE)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false)
            .resizable(false)
            .shadow(false)
            .build()
            .ok()?;
        let hwnd = window.hwnd().ok()?;
        let raw = HWND(hwnd.0 as *mut core::ffi::c_void);
        unsafe {
            let ex = GetWindowLongPtrW(raw, GWL_EXSTYLE);
            SetWindowLongPtrW(raw, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW);
        }
        Some(window)
    }

    /// 按物理像素提交位置与尺寸；返回是否成功显示。
    pub fn show_at(app: &AppHandle, point: Point, physical_size: f64) -> bool {
        let Some(window) = app.get_webview_window(ANCHOR_LABEL) else {
            return false;
        };
        let Ok(hwnd) = window.hwnd() else {
            return false;
        };
        let raw = HWND(hwnd.0 as *mut core::ffi::c_void);
        let size = physical_size.max(1.0).round() as i32;
        unsafe {
            let placed = SetWindowPos(
                raw,
                Some(HWND_TOPMOST),
                point.x.round() as i32,
                point.y.round() as i32,
                size,
                size,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .is_ok();
            let _ = ShowWindow(raw, SW_SHOWNOACTIVATE);
            // 位置提交失败或窗口仍不可见（被合成器/桌面策略拦下）都算失败，
            // 调用方据此记录 stage=anchor 的具体原因，而不是留下一个看不见的“已显示”。
            placed && IsWindowVisible(raw).as_bool()
        }
    }

    pub fn hide(app: &AppHandle) {
        let Some(window) = app.get_webview_window(ANCHOR_LABEL) else {
            return;
        };
        let Ok(hwnd) = window.hwnd() else {
            return;
        };
        let raw = HWND(hwnd.0 as *mut core::ffi::c_void);
        unsafe {
            let _ = ShowWindow(raw, SW_HIDE);
        }
    }

    pub fn position_for(
        window: &WebviewWindow,
        snapshot: &SelectionSnapshot,
        release: Point,
    ) -> Result<(Point, f64), ShowFailure> {
        let anchor_rect = snapshot.rects.last().copied().unwrap_or(Rect {
            left: release.x,
            top: release.y,
            right: release.x + 1.0,
            bottom: release.y + 1.0,
        });
        let probe = window
            .monitor_from_point(anchor_rect.left, anchor_rect.top)
            .ok()
            .flatten()
            .or_else(|| window.current_monitor().ok().flatten())
            .ok_or(ShowFailure::MonitorMissing)?;
        let scale = probe.scale_factor();
        let area = probe.work_area();
        let area = Rect {
            left: area.position.x as f64,
            top: area.position.y as f64,
            right: (area.position.x + area.size.width as i32) as f64,
            bottom: (area.position.y + area.size.height as i32) as f64,
        };
        let icon = Size {
            width: ANCHOR_LOGICAL_SIZE * scale,
            height: ANCHOR_LOGICAL_SIZE * scale,
        };
        let point = place_anchor(
            std::slice::from_ref(&anchor_rect),
            icon,
            area,
            ANCHOR_GAP_PX * scale,
        )
        .ok_or(ShowFailure::PlacementFailed)?;
        Ok((point, icon.width))
    }

    pub fn show_selection(
        app: &AppHandle,
        snapshot: &SelectionSnapshot,
        release: Point,
    ) -> Result<(), ShowFailure> {
        let window = app
            .get_webview_window(ANCHOR_LABEL)
            .ok_or(ShowFailure::WindowMissing)?;
        let (point, physical_size) = position_for(&window, snapshot, release)?;
        if show_at(app, point, physical_size) {
            Ok(())
        } else {
            Err(ShowFailure::ShowFailed)
        }
    }

    /// 图标窗口当前是否存在/可见，只读，供 `selection_debug_state` 排障。
    pub fn window_state(app: &AppHandle) -> (bool, bool) {
        let Some(window) = app.get_webview_window(ANCHOR_LABEL) else {
            return (false, false);
        };
        let Ok(hwnd) = window.hwnd() else {
            return (false, false);
        };
        let raw = HWND(hwnd.0 as *mut core::ffi::c_void);
        unsafe { (IsWindow(Some(raw)).as_bool(), IsWindowVisible(raw).as_bool()) }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use tauri::{AppHandle, WebviewWindow};

    pub fn create(_app: &AppHandle) -> Option<WebviewWindow> {
        None
    }

    pub fn show_at(_app: &AppHandle, _point: Point, _physical_size: f64) -> bool {
        false
    }

    pub fn hide(_app: &AppHandle) {}

    pub fn show_selection(
        _app: &AppHandle,
        _snapshot: &SelectionSnapshot,
        _release: Point,
    ) -> Result<(), ShowFailure> {
        Err(ShowFailure::WindowMissing)
    }

    pub fn window_state(_app: &AppHandle) -> (bool, bool) {
        (false, false)
    }
}

#[cfg(windows)]
pub use imp::{create, hide, show_selection, window_state};

#[cfg(not(windows))]
pub use imp::{create, hide, show_selection, window_state};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_gate_order_is_deterministic() {
        assert_eq!(
            anchor_click_block(false, false, false, false),
            Some(AnchorClickBlock::Disabled)
        );
        // 开关开着时，锁定优先于编辑中，编辑中优先于没有待消费选区。
        assert_eq!(
            anchor_click_block(true, true, true, true),
            Some(AnchorClickBlock::Locked)
        );
        assert_eq!(
            anchor_click_block(true, true, false, true),
            Some(AnchorClickBlock::Editing)
        );
        assert_eq!(
            anchor_click_block(false, true, false, false),
            Some(AnchorClickBlock::NoPending)
        );
        assert_eq!(anchor_click_block(true, true, false, false), None);
    }

    #[test]
    fn gate_and_show_failure_codes_are_stable() {
        assert_eq!(AnchorClickBlock::Disabled.code(), "anchor_disabled");
        assert_eq!(AnchorClickBlock::Locked.code(), "panel_locked");
        assert_eq!(AnchorClickBlock::Editing.code(), "card_editing");
        assert_eq!(AnchorClickBlock::NoPending.code(), "pending_missing");
        assert_eq!(ShowFailure::WindowMissing.code(), "window_missing");
        assert_eq!(ShowFailure::MonitorMissing.code(), "monitor_missing");
        assert_eq!(ShowFailure::PlacementFailed.code(), "placement_failed");
        assert_eq!(ShowFailure::ShowFailed.code(), "show_failed");
    }
}
