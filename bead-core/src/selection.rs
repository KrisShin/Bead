//! 划词锚点的纯逻辑：鼠标手势判定、选区准入、图标位置与生命周期。
//!
//! 这里刻意不碰 Win32、UIA 或窗口系统：所有输入都是普通数值与字符串，
//! 因此误触规则、屏幕边缘翻转和超时行为可以在没有桌面的环境里被完整断言。
//! 系统层只负责把真实事件翻译成这些类型，并按返回值执行动作。

/// 判定为「拖选」的最小位移（物理像素）。低于它的单击留给 UIA 判断是否为双击选词。
pub const MIN_DRAG_DISTANCE_PX: f64 = 4.0;
/// 自动图标允许的最大选区长度。超长选区（含 Ctrl+A 全文）不弹图标，显式热键仍可用。
pub const MAX_SELECTION_CHARS: usize = 4000;
/// 图标贴选区外沿的间距（物理像素）。
pub const ANCHOR_GAP_PX: f64 = 10.0;
/// 图标在屏幕上停留的时长，之后自动隐藏。
pub const ANCHOR_VISIBLE_MS: u64 = 2500;
/// 选区变化事件/键盘输入的尾随防抖窗口（毫秒）。
///
/// 拖动选区时系统会连发多条 `TEXTSELECTION_CHANGED`，键盘连按也会持续输入；
/// 统一等安静满这么久再发起一次 UIA 查询，既合并重复查询，也不打断选区过程。
pub const SELECTION_QUIET_MS: u64 = 300;
/// 选区变化事件可以归因到键盘的时间窗口（毫秒）：键击后这么久内的选区变化才算键盘选区。
pub const KEYBOARD_ATTRIBUTION_MS: u64 = 1500;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Rect {
    pub fn width(&self) -> f64 {
        (self.right - self.left).max(0.0)
    }

    pub fn height(&self) -> f64 {
        (self.bottom - self.top).max(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseRelease {
    pub button: MouseButton,
    pub start: Point,
    pub end: Point,
    pub duration_ms: u64,
    pub double_click: bool,
    /// 按下点落在窗口标题栏/边框等非客户区。
    pub started_on_chrome: bool,
    /// 按下时前台窗口属于 Bead 自己。
    pub foreground_is_self: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureIgnoreReason {
    NotLeftButton,
    StartedOnChrome,
    OwnWindow,
    NotASelectionGesture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureVerdict {
    Candidate,
    Ignore(GestureIgnoreReason),
}

/// 是否值得为这次松手做一次 UIA 校验。
///
/// 这里只做廉价预筛；「确实存在非空文本选区」由 UIA 终审，拖窗口/拖文件/滑块
/// 最终都因为没有 TextPattern 而被拒绝。
pub fn classify_mouse_release(release: MouseRelease) -> GestureVerdict {
    if release.button != MouseButton::Left {
        return GestureVerdict::Ignore(GestureIgnoreReason::NotLeftButton);
    }
    if release.started_on_chrome {
        return GestureVerdict::Ignore(GestureIgnoreReason::StartedOnChrome);
    }
    if release.foreground_is_self {
        return GestureVerdict::Ignore(GestureIgnoreReason::OwnWindow);
    }
    let dx = release.end.x - release.start.x;
    let dy = release.end.y - release.start.y;
    let distance = (dx * dx + dy * dy).sqrt();
    // 双击选词位移可能为 0，必须放行；纯长按不移动则不是选区手势。
    if distance >= MIN_DRAG_DISTANCE_PX || release.double_click {
        GestureVerdict::Candidate
    } else {
        let _ = release.duration_ms;
        GestureVerdict::Ignore(GestureIgnoreReason::NotASelectionGesture)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionPolicy {
    pub max_chars: usize,
}

impl Default for SelectionPolicy {
    fn default() -> Self {
        Self {
            max_chars: MAX_SELECTION_CHARS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionRejection {
    Empty,
    TooLong,
    CoversDocument,
}

/// 选区准入：空、超长、覆盖全文（Ctrl+A）都不弹自动图标。
///
/// 显式热键不走这里，所以「全选后翻译」仍由用户主动触发。
pub fn accept_selection<'a>(
    text: &'a str,
    covers_document: bool,
    policy: SelectionPolicy,
) -> Result<&'a str, SelectionRejection> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(SelectionRejection::Empty);
    }
    if covers_document {
        return Err(SelectionRejection::CoversDocument);
    }
    if trimmed.chars().count() > policy.max_chars {
        return Err(SelectionRejection::TooLong);
    }
    Ok(trimmed)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// 把图标放在选区末端外侧；右侧/下方放不下就翻到左侧/上方，最后夹进可用区。
///
/// 输入全部是物理像素；调用方负责按选区所在显示器取可用区，避免混合 DPI 用错比例。
pub fn place_anchor(
    rects: &[Rect],
    icon: Size,
    area: Rect,
    gap: f64,
) -> Option<Point> {
    let rect = rects.last()?;
    if area.width() <= 0.0 || area.height() <= 0.0 || icon.width <= 0.0 || icon.height <= 0.0 {
        return None;
    }
    let mut x = rect.right + gap;
    let mut y = rect.bottom + gap;
    if x + icon.width > area.right {
        x = rect.left - gap - icon.width;
    }
    if y + icon.height > area.bottom {
        y = rect.top - gap - icon.height;
    }
    let max_x = area.right - icon.width;
    let max_y = area.bottom - icon.height;
    Some(Point {
        x: x.clamp(area.left, max_x.max(area.left)),
        y: y.clamp(area.top, max_y.max(area.top)),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorPhase {
    Hidden,
    Visible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorEvent {
    Show { at_ms: u64 },
    Dismiss,
    Timeout { at_ms: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorEffect {
    Show,
    Hide,
    None,
}

/// 极简生命周期：只允许 Hidden→Visible→Hidden，重复事件不产生副作用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorLifecycle {
    phase: AnchorPhase,
    shown_at_ms: u64,
    visible_ms: u64,
}

impl AnchorLifecycle {
    pub fn new(visible_ms: u64) -> Self {
        Self {
            phase: AnchorPhase::Hidden,
            shown_at_ms: 0,
            visible_ms,
        }
    }

    pub fn phase(&self) -> AnchorPhase {
        self.phase
    }

    pub fn reduce(&mut self, event: AnchorEvent) -> AnchorEffect {
        match (self.phase, event) {
            (AnchorPhase::Hidden, AnchorEvent::Show { at_ms }) => {
                self.phase = AnchorPhase::Visible;
                self.shown_at_ms = at_ms;
                AnchorEffect::Show
            }
            (AnchorPhase::Visible, AnchorEvent::Show { .. }) => AnchorEffect::None,
            (AnchorPhase::Visible, AnchorEvent::Dismiss) => {
                self.phase = AnchorPhase::Hidden;
                AnchorEffect::Hide
            }
            (AnchorPhase::Hidden, AnchorEvent::Dismiss) => AnchorEffect::None,
            (AnchorPhase::Visible, AnchorEvent::Timeout { at_ms }) => {
                if at_ms.saturating_sub(self.shown_at_ms) >= self.visible_ms {
                    self.phase = AnchorPhase::Hidden;
                    AnchorEffect::Hide
                } else {
                    AnchorEffect::None
                }
            }
            (AnchorPhase::Hidden, AnchorEvent::Timeout { .. }) => AnchorEffect::None,
        }
    }
}

/// 尾随防抖：每次 `signal` 都推迟静默期，只有安静满 `quiet_ms` 才产出一次查询机会。
///
/// 鼠标松手、键盘输入、系统选区变化事件全部汇入同一个实例，因此同一次划词只会
/// 触发一次 UIA 查询；`poll` 返回 true 后会清空待查状态，重复轮询不会重复查询。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuietDebounce {
    last_signal_ms: Option<u64>,
}

impl QuietDebounce {
    /// 记录一次「选区可能变化」的信号；已有待查窗口会被整体推迟。
    pub fn signal(&mut self, at_ms: u64) {
        self.last_signal_ms = Some(at_ms);
    }

    /// 静默期已满则返回 true 并清空待查；未满或没有信号返回 false。
    pub fn poll(&mut self, now_ms: u64, quiet_ms: u64) -> bool {
        match self.last_signal_ms {
            Some(at) if now_ms.saturating_sub(at) >= quiet_ms => {
                self.last_signal_ms = None;
                true
            }
            _ => false,
        }
    }

    /// 距静默期结束还需等待的毫秒数；没有待查时返回 None。
    pub fn remaining_ms(&self, now_ms: u64, quiet_ms: u64) -> Option<u64> {
        self.last_signal_ms
            .map(|at| quiet_ms.saturating_sub(now_ms.saturating_sub(at)))
    }

    /// 取消待查（关闭开关、鼠标滚轮打断等）；返回此前是否有待查。
    pub fn cancel(&mut self) -> bool {
        self.last_signal_ms.take().is_some()
    }

    pub fn is_pending(&self) -> bool {
        self.last_signal_ms.is_some()
    }
}

/// 选区变化事件是否可以归因到键盘：只有最近一次键击仍在窗口期内才认为来自键盘。
///
/// 鼠标划词由松手手势路径负责，事件列表里的鼠标选区变化不再重复处理。
pub fn attributed_to_keyboard(
    last_key_at_ms: Option<u64>,
    event_at_ms: u64,
    window_ms: u64,
) -> bool {
    last_key_at_ms.is_some_and(|key| event_at_ms.saturating_sub(key) <= window_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(distance: f64, double_click: bool) -> MouseRelease {
        MouseRelease {
            button: MouseButton::Left,
            start: Point { x: 100.0, y: 100.0 },
            end: Point {
                x: 100.0 + distance,
                y: 100.0,
            },
            duration_ms: 200,
            double_click,
            started_on_chrome: false,
            foreground_is_self: false,
        }
    }

    #[test]
    fn a_tiny_click_is_not_a_selection_gesture_but_double_click_is() {
        assert_eq!(
            classify_mouse_release(release(1.0, false)),
            GestureVerdict::Ignore(GestureIgnoreReason::NotASelectionGesture)
        );
        assert_eq!(
            classify_mouse_release(release(0.0, true)),
            GestureVerdict::Candidate
        );
        assert_eq!(
            classify_mouse_release(release(20.0, false)),
            GestureVerdict::Candidate
        );
    }

    #[test]
    fn chrome_drag_and_own_window_are_rejected_before_uia() {
        let mut on_chrome = release(30.0, false);
        on_chrome.started_on_chrome = true;
        assert_eq!(
            classify_mouse_release(on_chrome),
            GestureVerdict::Ignore(GestureIgnoreReason::StartedOnChrome)
        );

        let mut own = release(30.0, false);
        own.foreground_is_self = true;
        assert_eq!(
            classify_mouse_release(own),
            GestureVerdict::Ignore(GestureIgnoreReason::OwnWindow)
        );

        let mut right = release(30.0, false);
        right.button = MouseButton::Other;
        assert_eq!(
            classify_mouse_release(right),
            GestureVerdict::Ignore(GestureIgnoreReason::NotLeftButton)
        );
    }

    #[test]
    fn selection_gate_rejects_empty_select_all_and_overlong_text() {
        let policy = SelectionPolicy::default();
        assert_eq!(accept_selection("   ", false, policy), Err(SelectionRejection::Empty));
        assert_eq!(
            accept_selection("hello", true, policy),
            Err(SelectionRejection::CoversDocument)
        );
        let long = "a".repeat(MAX_SELECTION_CHARS + 1);
        assert_eq!(
            accept_selection(&long, false, policy),
            Err(SelectionRejection::TooLong)
        );
        assert_eq!(accept_selection("  hello  ", false, policy), Ok("hello"));
    }

    #[test]
    fn anchor_prefers_below_right_then_flips_and_clamps() {
        let area = Rect {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1040.0,
        };
        let icon = Size {
            width: 24.0,
            height: 24.0,
        };
        let center = Rect {
            left: 400.0,
            top: 300.0,
            right: 500.0,
            bottom: 320.0,
        };
        assert_eq!(
            place_anchor(&[center], icon, area, 10.0),
            Some(Point { x: 510.0, y: 330.0 })
        );

        let right_edge = Rect {
            left: 1880.0,
            top: 300.0,
            right: 1900.0,
            bottom: 320.0,
        };
        assert_eq!(
            place_anchor(&[right_edge], icon, area, 10.0),
            Some(Point { x: 1846.0, y: 330.0 })
        );

        let bottom_edge = Rect {
            left: 400.0,
            top: 1000.0,
            right: 500.0,
            bottom: 1020.0,
        };
        assert_eq!(
            place_anchor(&[bottom_edge], icon, area, 10.0),
            Some(Point { x: 510.0, y: 966.0 })
        );

        let corner = Rect {
            left: 1880.0,
            top: 1000.0,
            right: 1900.0,
            bottom: 1020.0,
        };
        assert_eq!(
            place_anchor(&[corner], icon, area, 10.0),
            Some(Point { x: 1846.0, y: 966.0 })
        );
    }

    #[test]
    fn placement_uses_the_last_rect_of_a_multiline_selection() {
        let area = Rect {
            left: 0.0,
            top: 0.0,
            right: 1000.0,
            bottom: 800.0,
        };
        let icon = Size { width: 24.0, height: 24.0 };
        let first = Rect { left: 10.0, top: 10.0, right: 500.0, bottom: 30.0 };
        let last = Rect { left: 10.0, top: 40.0, right: 200.0, bottom: 60.0 };
        assert_eq!(
            place_anchor(&[first, last], icon, area, 10.0),
            Some(Point { x: 210.0, y: 70.0 })
        );
    }

    #[test]
    fn anchor_lifecycle_ignores_duplicate_show_and_hides_once() {
        let mut lifecycle = AnchorLifecycle::new(2500);
        assert_eq!(lifecycle.phase(), AnchorPhase::Hidden);
        assert_eq!(
            lifecycle.reduce(AnchorEvent::Show { at_ms: 100 }),
            AnchorEffect::Show
        );
        assert_eq!(
            lifecycle.reduce(AnchorEvent::Show { at_ms: 200 }),
            AnchorEffect::None
        );
        assert_eq!(
            lifecycle.reduce(AnchorEvent::Timeout { at_ms: 2000 }),
            AnchorEffect::None
        );
        assert_eq!(
            lifecycle.reduce(AnchorEvent::Timeout { at_ms: 2601 }),
            AnchorEffect::Hide
        );
        assert_eq!(lifecycle.phase(), AnchorPhase::Hidden);
        assert_eq!(lifecycle.reduce(AnchorEvent::Dismiss), AnchorEffect::None);
    }

    #[test]
    fn quiet_debounce_merges_rapid_signals_into_one_query() {
        let mut debounce = QuietDebounce::default();
        assert!(!debounce.is_pending());
        debounce.signal(100);
        debounce.signal(220); // 拖动选区仍在继续：窗口整体后移
        assert_eq!(debounce.remaining_ms(220, SELECTION_QUIET_MS), Some(300));
        assert!(!debounce.poll(400, SELECTION_QUIET_MS));
        assert!(debounce.poll(520, SELECTION_QUIET_MS));
        // 已经产出过一次查询：再轮询不会重复查询。
        assert!(!debounce.poll(2000, SELECTION_QUIET_MS));
        assert!(!debounce.is_pending());
    }

    #[test]
    fn quiet_debounce_cancel_drops_pending_query() {
        let mut debounce = QuietDebounce::default();
        debounce.signal(100);
        assert!(debounce.cancel());
        assert!(!debounce.cancel());
        assert!(!debounce.poll(1000, SELECTION_QUIET_MS));
    }

    #[test]
    fn key_selection_is_only_attributed_within_the_keyboard_window() {
        assert!(!attributed_to_keyboard(None, 1000, KEYBOARD_ATTRIBUTION_MS));
        assert!(attributed_to_keyboard(Some(900), 1000, KEYBOARD_ATTRIBUTION_MS));
        assert!(attributed_to_keyboard(
            Some(0),
            KEYBOARD_ATTRIBUTION_MS,
            KEYBOARD_ATTRIBUTION_MS
        ));
        assert!(!attributed_to_keyboard(
            Some(0),
            KEYBOARD_ATTRIBUTION_MS + 1,
            KEYBOARD_ATTRIBUTION_MS
        ));
    }
}
