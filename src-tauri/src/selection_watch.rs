//! 划词监听：Raw Input + 隐藏窗口 + 同线程 UIA 终审。
//!
//! 为什么不用全局鼠标钩子：`LowLevelMouseProc` 会给全系统输入加一段同步回调，
//! 系统输入延迟取决于我们的处理速度。Raw Input 只是把事件复制一份到我们的窗口，
//! 即使用户拖窗口/拖文件/拉滑块，输入仍直达目标应用，我们只是旁观者。
//!
//! 为什么把 UIA 也放在这个线程：UIA 客户端是 STA 对象，必须在创建它的线程上调用。
//! 这个线程本来就有消息循环（Raw Input 需要一个窗口），因此直接复用；查询前已设置
//! 200ms 连接/事务超时，最坏只阻塞我们自己的消息队列，不阻塞系统输入。
//!
//! 三类信号（鼠标松手、键盘输入、系统文本选区变化）统一进一个尾随防抖窗口：
//! 拖动选区时系统会连发事件、键盘会持续输入，等安静 300ms 再查一次 UIA，
//! 一次划词只查一次、只弹一个图标。
//!
//! 误触防线：鼠标手势先过廉价预筛（左键、非标题栏、非自身窗口、有位移或双击），
//! 「确实存在非空文本选区」由 UIA 终审；拖窗口（HTCAPTION）在预筛就被拒绝，
//! 拖文件、拉滑块、Explorer 双击等走不到 TextPattern，同样不会出现图标。
//!
//! 键盘兜底固定开启，随划词开关启停：监听线程运行时注册键盘 Raw Input 与
//! `EVENT_OBJECT_TEXTSELECTION_CHANGED` 钩子。键盘 Raw Input 只记录「发生过一次键击」
//! 与时间，不读取、不保存任何键值内容；划词关闭时线程结束，两者一起反注册，
//! 不留任何键盘监听。鼠标 Raw Input 同样随划词开关启停。
//!
//! 隐私：UIA 路径不读剪贴板、不记录文本内容。仅当应用明确不支持 `TextPattern`
//! （Trae/微信这类 Electron/自绘界面）时，才走 `selection_clipboard` 的兜底：
//! 先快照、再模拟 Ctrl+C、读文本、恢复快照，**会短暂写剪贴板**；文本只在内存里
//! 传给图标点击路径，图标超时或关闭开关即丢弃，任何日志都不含文本与按键内容。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bead_core::selection::{
    accept_selection, attributed_to_keyboard, classify_mouse_release, GestureVerdict, MouseButton,
    MouseRelease, Point, QuietDebounce, SelectionPolicy, SelectionRejection, KEYBOARD_ATTRIBUTION_MS,
    SELECTION_QUIET_MS,
};
use tauri::{Emitter, Manager};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetAncestor,
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetSystemMetrics,
    GetWindowThreadProcessId, KillTimer, RegisterClassW, SendMessageW, SetTimer,
    TranslateMessage, WindowFromPoint, WNDCLASSW, GA_ROOT, RI_MOUSE_LEFT_BUTTON_DOWN,
    RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_WHEEL, SM_CXDOUBLECLK, SM_CYDOUBLECLK, WM_INPUT,
    WM_NCHITTEST, WM_QUIT, WM_TIMER, WS_POPUP, WINEVENT_OUTOFCONTEXT,
};

use crate::uia::{SelectionFailure, SelectionSnapshot, UiaClient};

extern "system" {
    fn GetDoubleClickTime() -> u32;
}

/// `EVENT_OBJECT_TEXTSELECTION_CHANGED`：windows 0.62 的元数据没有生成这个常量。
const EVENT_OBJECT_TEXTSELECTION_CHANGED: u32 = 0x8014;
const TIMER_QUERY: usize = 1;
/// 剪贴板兜底会注入 Ctrl+C；这段时间内忽略我们自己的按键/选区信号，避免自激。
const FALLBACK_INPUT_SUPPRESS_MS: u64 = 900;

thread_local! {
    /// wndproc 只做解析与入队，UIA 查询在主循环里执行：UIA 调用可能泵消息，
    /// 若在 wndproc 内直接查询，嵌套的 WM_INPUT 会重入同一份可变状态。
    static RAW_INPUTS: RefCell<VecDeque<RawInputEvent>> = RefCell::new(VecDeque::new());
    /// 文本选区变化事件只记窗口句柄，不带任何文本内容。
    static TEXT_SELECTION_EVENTS: RefCell<VecDeque<HWND>> = RefCell::new(VecDeque::new());
}

/// 划词链路的结构化诊断：计数 + 最近一次失败位置。
///
/// 只记录「哪个 stage、什么 reason、前台窗口类名与 pid」，**不记录选中文本内容，
/// 不记录任何按键内容**。`selection_debug_state` 命令把它原样暴露给排障方。
#[derive(Default)]
pub struct SelectionDiagnostics {
    pub gesture_candidates: AtomicU64,
    pub uia_ok: AtomicU64,
    pub uia_failed: AtomicU64,
    pub gate_rejections: AtomicU64,
    pub anchor_shown: AtomicU64,
    pub anchor_failed: AtomicU64,
    pub click_total: AtomicU64,
    pub click_consumed: AtomicU64,
    pub click_missed: AtomicU64,
    pub click_blocked: AtomicU64,
    /// 剪贴板兜底：尝试次数 / 成功次数 / 失败次数（失败原因进 `last`）。
    pub fallback_attempts: AtomicU64,
    pub fallback_ok: AtomicU64,
    pub fallback_failed: AtomicU64,
    last: Mutex<Option<SelectionDiagnostic>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionDiagnostic {
    pub stage: &'static str,
    pub reason: &'static str,
    pub class: String,
    pub pid: u32,
}

impl SelectionDiagnostics {
    /// 记录一次失败并打一行结构化日志。stage 取值：
    /// `query`/`uia`/`gate`/`anchor`/`click`/`watcher`/`hotkey`。
    pub fn note(&self, stage: &'static str, reason: &'static str, class: &str, pid: u32) {
        eprintln!("{}", diagnostic_line(stage, reason, class, pid));
        if let Ok(mut last) = self.last.lock() {
            *last = Some(SelectionDiagnostic {
                stage,
                reason,
                class: normalize_window_class(class),
                pid,
            });
        }
    }

    pub fn last(&self) -> Option<SelectionDiagnostic> {
        self.last.lock().ok().and_then(|last| last.clone())
    }
}

/// 日志行格式固定，测试只校验结构，不涉及任何用户内容。
pub fn diagnostic_line(stage: &str, reason: &str, class: &str, pid: u32) -> String {
    format!(
        "[bead][selection] stage={stage} reason={reason} app_class={} pid={pid}",
        normalize_window_class(class)
    )
}

/// 窗口类名可能包含任意字符；只保留可判定的 ASCII 片段，避免日志被异常内容污染。
pub fn normalize_window_class(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .take(48)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if filtered.is_empty() {
        "unknown".into()
    } else {
        filtered
    }
}

#[derive(Clone, Copy)]
struct RawMouseEvent {
    buttons: u32,
    point: POINT,
    at: Instant,
}

#[derive(Clone, Copy)]
enum RawInputEvent {
    Mouse(RawMouseEvent),
    Key,
}

/// 待查询的 UIA 入口：鼠标路径用松手点兜底找元素，键盘路径只认焦点元素。
#[derive(Clone, Copy)]
enum QueryOrigin {
    Pointer(Point),
    Keyboard,
}

struct Tracking {
    start: Point,
    start_at: Instant,
    double_click: bool,
    started_on_chrome: bool,
    foreground_is_self: bool,
}

struct WatchState {
    app: tauri::AppHandle,
    uia: UiaClient,
    hwnd: HWND,
    keyboard_fallback: Arc<AtomicBool>,
    diagnostics: Arc<SelectionDiagnostics>,
    tracking: Option<Tracking>,
    last_down: Option<(POINT, Instant)>,
    debounce: QuietDebounce,
    origin: Option<QueryOrigin>,
    /// 最近一次键盘输入的时间（只记时间，不记键值）。
    last_key_at: Option<u64>,
    /// 松手瞬间的前台进程；剪贴板兜底前复核，用户切走就绝不注入 Ctrl+C。
    release_pid: Option<u32>,
    /// 键盘设备与事件钩子当前是否注册成功，避免重复注册。
    keyboard_on: bool,
    hook: HWINEVENTHOOK,
    /// 连续「应用不支持 TextPattern」的前台 pid 与次数；达到 2 次才给设置页提示，
    /// 避免在桌面/资源管理器里拖选一次就误报。
    no_pattern_pid: Option<u32>,
    no_pattern_streak: u32,
    /// 到此刻为止忽略「键盘输入/文本选区变化」信号（本进程注入的 Ctrl+C 引起）。
    suppress_until_ms: u64,
    started: Instant,
}

impl WatchState {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn handle_mouse(&mut self, event: RawMouseEvent) {
        if event.buttons & RI_MOUSE_WHEEL != 0 {
            // 滚轮既可能发生在拖选途中，也可能是用户不想翻译；取消当前手势与待查。
            self.tracking = None;
            self.debounce.cancel();
            return;
        }
        if event.buttons & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
            let double_click_ms = unsafe { GetDoubleClickTime() } as u64;
            let (slop_x, slop_y) = unsafe {
                (
                    GetSystemMetrics(SM_CXDOUBLECLK).max(2),
                    GetSystemMetrics(SM_CYDOUBLECLK).max(2),
                )
            };
            let double_click = self
                .last_down
                .map(|(last, at)| {
                    event.at.duration_since(at) <= Duration::from_millis(double_click_ms)
                        && (event.point.x - last.x).abs() <= slop_x
                        && (event.point.y - last.y).abs() <= slop_y
                })
                .unwrap_or(false);
            self.last_down = Some((event.point, event.at));
            let start = Point {
                x: event.point.x as f64,
                y: event.point.y as f64,
            };
            self.tracking = Some(Tracking {
                start,
                start_at: event.at,
                double_click,
                started_on_chrome: unsafe { started_on_chrome(event.point) },
                // 按下点落在我们自己的覆盖窗（图标窗是 NOACTIVATE，前台仍是源应用）时，
                // 这次点击不属于源应用的划词手势，后续查询一律不弹。
                // WebView2 的子窗口可能属于运行时进程，必须按顶层窗口的 pid 判归属。
                foreground_is_self: unsafe {
                    let hit = WindowFromPoint(event.point);
                    let root = GetAncestor(hit, GA_ROOT);
                    foreground_is_self() || process_is_self(root) || process_is_self(hit)
                },
            });
            return;
        }
        if event.buttons & RI_MOUSE_LEFT_BUTTON_UP != 0 {
            let Some(tracking) = self.tracking.take() else {
                return;
            };
            let end = Point {
                x: event.point.x as f64,
                y: event.point.y as f64,
            };
            let release = MouseRelease {
                button: MouseButton::Left,
                start: tracking.start,
                end,
                duration_ms: event.at.duration_since(tracking.start_at).as_millis() as u64,
                double_click: tracking.double_click,
                started_on_chrome: tracking.started_on_chrome,
                foreground_is_self: tracking.foreground_is_self,
            };
            if classify_mouse_release(release) != GestureVerdict::Candidate {
                return;
            }
            // 关掉开关后可能还有一两个在途事件：以设置现值再判一次，避免关不掉。
            if !crate::anchor_enabled(&self.app) {
                return;
            }
            self.diagnostics
                .gesture_candidates
                .fetch_add(1, Ordering::Relaxed);
            // 记下松手时的前台进程：剪贴板兜底会注入 Ctrl+C，必须先确认前台没变，
            // 否则会把按键送进用户刚切过去的另一个应用。
            self.release_pid = unsafe { Some(window_process_id(GetForegroundWindow())) };
            self.signal(QueryOrigin::Pointer(end));
        }
    }

    fn handle_key(&mut self) {
        let now = self.now_ms();
        self.last_key_at = Some(now);
        if self.input_suppressed() {
            return;
        }
        if !self.keyboard_fallback.load(Ordering::SeqCst) {
            return;
        }
        self.signal(QueryOrigin::Keyboard);
    }

    /// 本进程刚注入过 Ctrl+C：这段时间的按键与选区变化都是我们自己制造的。
    fn input_suppressed(&self) -> bool {
        self.now_ms() < self.suppress_until_ms
    }

    /// 系统报告文本选区变化。只有「刚刚发生键盘输入」且变化来自前台窗口时才处理：
    /// 鼠标选区由松手手势路径负责，这里重复处理只会多查一次。
    fn handle_text_selection(&mut self, hwnd: HWND) {
        if self.input_suppressed() {
            return;
        }
        if !self.keyboard_fallback.load(Ordering::SeqCst) {
            return;
        }
        let now = self.now_ms();
        if !attributed_to_keyboard(self.last_key_at, now, KEYBOARD_ATTRIBUTION_MS) {
            return;
        }
        if !unsafe { same_as_foreground(hwnd) } || unsafe { process_is_self(hwnd) } {
            return;
        }
        self.signal(QueryOrigin::Keyboard);
    }

    /// 进入尾随防抖：每次信号都把静默期整体后移，并重新挂一次窗口计时器。
    fn signal(&mut self, origin: QueryOrigin) {
        let now = self.now_ms();
        self.debounce.signal(now);
        self.origin = Some(origin);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_QUERY, SELECTION_QUIET_MS as u32, None);
        }
    }

    /// 计时器到点：安静满窗口才真正查询；提前唤醒则按剩余时间重新挂表。
    fn poll_query(&mut self) {
        let now = self.now_ms();
        if !self.debounce.poll(now, SELECTION_QUIET_MS) {
            if let Some(remaining) = self.debounce.remaining_ms(now, SELECTION_QUIET_MS) {
                unsafe {
                    SetTimer(Some(self.hwnd), TIMER_QUERY, remaining.max(1) as u32, None);
                }
            }
            return;
        }
        let origin = self.origin.take().unwrap_or(QueryOrigin::Keyboard);
        let release_pid = self.release_pid.take();
        let (class, pid) = unsafe { foreground_identity() };
        if !crate::anchor_enabled(&self.app) {
            self.diagnostics.note("query", "disabled", &class, pid);
            return;
        }
        // 在我们自己的面板里打字/选字不该弹图标。
        if unsafe { foreground_is_self() } {
            self.diagnostics.note("query", "self_foreground", &class, pid);
            return;
        }
        let mut anchor_point = unsafe { cursor_point() };
        let snapshot = match origin {
            QueryOrigin::Pointer(point) => match self.uia.read_selection(point) {
                Ok(snapshot) => {
                    self.on_uia_success();
                    snapshot
                }
                Err(failure) => {
                    self.note_uia_failure(failure, &class, pid);
                    if !uia_failure_allows_clipboard_fallback(failure) {
                        return;
                    }
                    // 只有鼠标手势才走剪贴板兜底：键盘路径刚结束时用户可能还按着修饰键，
                    // 注入 Ctrl+C 会叠加成别的快捷键。
                    anchor_point = point;
                    match self.clipboard_fallback(&class, pid, release_pid) {
                        Some(snapshot) => snapshot,
                        None => return,
                    }
                }
            },
            QueryOrigin::Keyboard => match self.uia.read_focused_selection() {
                Ok(snapshot) => {
                    self.on_uia_success();
                    snapshot
                }
                Err(failure) => {
                    self.note_uia_failure(failure, &class, pid);
                    return;
                }
            },
        };
        if let Err(rejection) = accept_selection(
            &snapshot.text,
            snapshot.covers_document,
            SelectionPolicy::default(),
        ) {
            self.diagnostics
                .gate_rejections
                .fetch_add(1, Ordering::Relaxed);
            self.diagnostics
                .note("gate", rejection_code(rejection), &class, pid);
            return;
        }
        crate::show_selection_anchor(&self.app, &snapshot, anchor_point);
    }

    fn note_uia_failure(&mut self, failure: SelectionFailure, class: &str, pid: u32) {
        self.diagnostics
            .uia_failed
            .fetch_add(1, Ordering::Relaxed);
        self.diagnostics.note("uia", failure.code(), class, pid);
        if failure.means_app_unsupported() {
            self.note_no_text_pattern(pid);
        }
    }

    fn on_uia_success(&mut self) {
        self.diagnostics.uia_ok.fetch_add(1, Ordering::Relaxed);
        self.no_pattern_pid = None;
        self.no_pattern_streak = 0;
        crate::clear_selection_unsupported_hint(&self.app);
    }

    /// UIA 失败后的剪贴板兜底。成功时同样清掉热键提示：这次确实取到了。
    ///
    /// 切换前台窗口/外部改写剪贴板时 `capture_selection` 会返回失败；
    /// 终端类窗口在这里就拦下，绝不注入 Ctrl+C。
    fn clipboard_fallback(
        &mut self,
        class: &str,
        pid: u32,
        release_pid: Option<u32>,
    ) -> Option<SelectionSnapshot> {
        // 松手后用户可能已经 Alt+Tab 切走：此时注入 Ctrl+C 会落进别的应用，
        // 必须在按键盘之前就拦下。
        if release_pid != Some(pid) {
            self.diagnostics
                .fallback_failed
                .fetch_add(1, Ordering::Relaxed);
            self.diagnostics.note(
                "fallback",
                crate::selection_clipboard::FallbackError::ForegroundChanged.code(),
                class,
                pid,
            );
            return None;
        }
        if crate::selection_clipboard::is_terminal_window_class(class) {
            self.diagnostics
                .fallback_failed
                .fetch_add(1, Ordering::Relaxed);
            self.diagnostics.note(
                "fallback",
                crate::selection_clipboard::FallbackError::Terminal.code(),
                class,
                pid,
            );
            return None;
        }
        self.diagnostics
            .fallback_attempts
            .fetch_add(1, Ordering::Relaxed);
        // 先设抑制窗口再注入，保证我们自己的 Ctrl+C 不会触发键盘/选区信号。
        self.suppress_until_ms = self.now_ms() + FALLBACK_INPUT_SUPPRESS_MS;
        match crate::selection_clipboard::capture_selection() {
            Ok(captured) => {
                self.diagnostics
                    .fallback_ok
                    .fetch_add(1, Ordering::Relaxed);
                if captured.restore_failed {
                    // 文本已到手，剪贴板恢复失败单独记一笔，但不影响出图标。
                    self.diagnostics.note("fallback", "restore_failed", class, pid);
                }
                self.no_pattern_pid = None;
                self.no_pattern_streak = 0;
                crate::clear_selection_unsupported_hint(&self.app);
                Some(SelectionSnapshot {
                    text: captured.text,
                    rects: Vec::new(),
                    covers_document: false,
                })
            }
            Err(error) => {
                self.diagnostics
                    .fallback_failed
                    .fetch_add(1, Ordering::Relaxed);
                self.diagnostics.note("fallback", error.code(), class, pid);
                None
            }
        }
    }

    /// UIA 明确报告「该应用没有 TextPattern」时，连续两次同一前台进程才提示热键兜底。
    fn note_no_text_pattern(&mut self, pid: u32) {
        if self.no_pattern_pid == Some(pid) {
            self.no_pattern_streak = self.no_pattern_streak.saturating_add(1);
        } else {
            self.no_pattern_pid = Some(pid);
            self.no_pattern_streak = 1;
        }
        if self.no_pattern_streak >= 2 {
            crate::publish_selection_unsupported_hint(&self.app);
        }
    }

    /// 按开关注册/反注册键盘 Raw Input 与文本选区事件钩子；关闭时彻底断开。
    fn apply_keyboard_inputs(&mut self) {
        let enabled = self.keyboard_fallback.load(Ordering::SeqCst);
        if enabled == self.keyboard_on {
            return;
        }
        if enabled {
            let hook = if self.hook.is_invalid() {
                unsafe {
                    SetWinEventHook(
                        EVENT_OBJECT_TEXTSELECTION_CHANGED,
                        EVENT_OBJECT_TEXTSELECTION_CHANGED,
                        None,
                        Some(win_event_proc),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    )
                }
            } else {
                self.hook
            };
            if hook.is_invalid() {
                self.revert_keyboard();
                return;
            }
            if !unsafe { set_keyboard_device(self.hwnd, true) } {
                if self.hook.is_invalid() {
                    let _ = unsafe { UnhookWinEvent(hook) };
                }
                self.revert_keyboard();
                return;
            }
            self.hook = hook;
            self.keyboard_on = true;
            crate::clear_selection_watch_error(&self.app);
        } else {
            if !self.hook.is_invalid() {
                let _ = unsafe { UnhookWinEvent(self.hook) };
            }
            self.hook = HWINEVENTHOOK(std::ptr::null_mut());
            let _ = unsafe { set_keyboard_device(self.hwnd, false) };
            self.keyboard_on = false;
            self.debounce.cancel();
            self.origin = None;
            self.last_key_at = None;
        }
    }

    /// 键盘兜底注册失败：停用本次运行的键盘监听并推一次状态。
    ///
    /// 开关已固定开启、不再回写设置字段：改为在设置页写一条明确提示，
    /// 不保留「已开启」的假状态（规范：能力探测失败不得静默 no-op）。
    fn revert_keyboard(&mut self) {
        self.keyboard_on = false;
        self.keyboard_fallback.store(false, Ordering::SeqCst);
        crate::set_selection_watch_error(
            &self.app,
            "键盘选区兜底注册失败，本次运行已停用（不记录按键内容，重启后重试）。",
        );
        let state = self.app.state::<crate::AppState>();
        let view = crate::snapshot(state.inner());
        let _ = self.app.emit("state", view);
    }
}

/// 选区监听句柄：关闭开关时用它停线程并反注册 Raw Input。
pub struct WatcherHandle {
    thread_id: u32,
    stopped: Arc<AtomicBool>,
}

impl WatcherHandle {
    pub fn stop(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                self.thread_id,
                WM_QUIT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    /// 监听线程是否仍在跑（Raw Input 注册是否仍然有效）。
    pub fn is_running(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst)
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 启动监听线程；启动失败返回 None（调用方据此让设置项如实标为不可用）。
pub fn start(app: tauri::AppHandle) -> Option<WatcherHandle> {
    let stopped = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Option<(isize, u32)>>();
    let thread_stopped = stopped.clone();
    std::thread::Builder::new()
        .name("bead-selection-watch".into())
        .spawn(move || {
            // 失败原因已通过 ready 通道回报给 `start`；线程在这里只负责退出。
            let _ = unsafe { run(app, thread_stopped, ready_tx) };
        })
        .ok()?;
    let (_, thread_id) = ready_rx.recv_timeout(Duration::from_secs(2)).ok()??;
    Some(WatcherHandle {
        thread_id,
        stopped,
    })
}

unsafe fn run(
    app: tauri::AppHandle,
    stopped: Arc<AtomicBool>,
    ready: Sender<Option<(isize, u32)>>,
) -> Result<(), String> {
    let module = GetModuleHandleW(None).map_err(|error| error.to_string())?;
    let instance = windows::Win32::Foundation::HINSTANCE(module.0);
    let class_name = w!("BeadSelectionWatcher");
    let class = WNDCLASSW {
        style: Default::default(),
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: Default::default(),
        hCursor: Default::default(),
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: class_name,
    };
    if RegisterClassW(&class) == 0 {
        let _ = ready.send(None);
        return Err("RegisterClassW 失败".into());
    }
    // 普通 WS_POPUP 隐藏窗口，不用 HWND_MESSAGE：Raw Input 需要真实顶层窗口来接收
    // WM_INPUT，消息专用窗口在部分系统版本上收不到。
    let hwnd = match CreateWindowExW(
        Default::default(),
        class_name,
        class_name,
        WS_POPUP,
        0,
        0,
        0,
        0,
        None,
        None,
        Some(instance),
        None,
    ) {
        Ok(hwnd) => hwnd,
        Err(error) => {
            let _ = ready.send(None);
            return Err(format!("CreateWindowExW 失败：{error}"));
        }
    };
    let Some(uia) = UiaClient::new() else {
        let _ = DestroyWindow(hwnd);
        let _ = ready.send(None);
        return Err("UIA 客户端创建失败".into());
    };

    let device = [RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    }];
    if let Err(error) =
        RegisterRawInputDevices(&device, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
    {
        let _ = DestroyWindow(hwnd);
        let _ = ready.send(None);
        return Err(format!("注册 Raw Input 失败：{error}"));
    }

    let keyboard_fallback = app
        .state::<crate::AppState>()
        .keyboard_fallback
        .clone();
    let diagnostics = app.state::<crate::AppState>().selection_diag.clone();
    let mut state = WatchState {
        app,
        uia,
        hwnd,
        keyboard_fallback,
        diagnostics,
        tracking: None,
        last_down: None,
        debounce: QuietDebounce::default(),
        origin: None,
        last_key_at: None,
        release_pid: None,
        keyboard_on: false,
        hook: HWINEVENTHOOK(std::ptr::null_mut()),
        no_pattern_pid: None,
        no_pattern_streak: 0,
        suppress_until_ms: 0,
        started: Instant::now(),
    };
    // 键盘兜底若已开启，钩子必须在当前线程安装（事件只会投递到安装它的线程）。
    state.apply_keyboard_inputs();
    let _ = ready.send(Some((hwnd.0 as isize, GetWindowThreadProcessId(hwnd, None))));

    let mut message = Default::default();
    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
        if message.message == WM_TIMER && message.wParam.0 == TIMER_QUERY {
            state.poll_query();
            continue;
        }
        let _ = TranslateMessage(&message);
        DispatchMessageW(&message);
        // UIA 查询放到消息分发之后：这里不会重入 wndproc 的可变状态。
        let events: Vec<RawInputEvent> =
            RAW_INPUTS.with(|queue| queue.borrow_mut().drain(..).collect());
        for event in events {
            match event {
                RawInputEvent::Mouse(mouse) => state.handle_mouse(mouse),
                RawInputEvent::Key => state.handle_key(),
            }
        }
        let selection_events: Vec<HWND> =
            TEXT_SELECTION_EVENTS.with(|queue| queue.borrow_mut().drain(..).collect());
        for changed in selection_events {
            state.handle_text_selection(changed);
        }
    }

    // 关闭开关：先断开键盘兜底与鼠标 Raw Input，再销毁窗口，保证不再收到任何全局输入。
    if !state.hook.is_invalid() {
        let _ = UnhookWinEvent(state.hook);
        state.hook = HWINEVENTHOOK(std::ptr::null_mut());
    }
    let _ = set_keyboard_device(hwnd, false);
    let device = [RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: HWND::default(),
    }];
    let _ = RegisterRawInputDevices(&device, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
    let _ = KillTimer(Some(hwnd), TIMER_QUERY);
    let _ = DestroyWindow(hwnd);
    // UiaClient 的 Drop 负责 CoUninitialize。
    drop(state);
    stopped.store(true, Ordering::SeqCst);
    Ok(())
}

unsafe fn set_keyboard_device(hwnd: HWND, enabled: bool) -> bool {
    let device = [RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x06,
        dwFlags: if enabled { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
        hwndTarget: if enabled { hwnd } else { HWND::default() },
    }];
    RegisterRawInputDevices(&device, std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_ok()
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // 只记窗口句柄，不碰任何文本内容；过滤放在主循环里做，回调保持最短。
    TEXT_SELECTION_EVENTS.with(|queue| queue.borrow_mut().push_back(hwnd));
}

unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_INPUT => {
            if let Some(event) = read_raw_input(lparam) {
                RAW_INPUTS.with(|queue| queue.borrow_mut().push_back(event));
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

unsafe fn read_raw_input(lparam: LPARAM) -> Option<RawInputEvent> {
    let raw_handle = HRAWINPUT(lparam.0 as *mut core::ffi::c_void);
    let mut size = 0u32;
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    if GetRawInputData(raw_handle, RID_INPUT, None, &mut size, header_size) == u32::MAX
        || size == 0
    {
        return None;
    }
    let mut buffer = vec![0u8; size as usize];
    if GetRawInputData(
        raw_handle,
        RID_INPUT,
        Some(buffer.as_mut_ptr().cast()),
        &mut size,
        header_size,
    ) == u32::MAX
    {
        return None;
    }
    let raw = &*(buffer.as_ptr() as *const RAWINPUT);
    if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
        // 键盘兜底只关心「发生过一次键击」；不读取 raw.data.keyboard 的任何字段，
        // 键值随缓冲区在本函数结束时丢弃，不写日志、不入状态。
        return Some(RawInputEvent::Key);
    }
    if raw.header.dwType != RIM_TYPEMOUSE.0 {
        return None;
    }
    let buttons = raw.data.mouse.Anonymous.Anonymous.usButtonFlags as u32;
    if buttons & (RI_MOUSE_LEFT_BUTTON_DOWN | RI_MOUSE_LEFT_BUTTON_UP | RI_MOUSE_WHEEL) == 0 {
        return None;
    }
    let mut point = POINT::default();
    if GetCursorPos(&mut point).is_err() {
        return None;
    }
    Some(RawInputEvent::Mouse(RawMouseEvent {
        buttons,
        point,
        at: Instant::now(),
    }))
}

unsafe fn cursor_point() -> Point {
    let mut point = POINT::default();
    if GetCursorPos(&mut point).is_err() {
        return Point { x: 0.0, y: 0.0 };
    }
    Point {
        x: point.x as f64,
        y: point.y as f64,
    }
}

/// 按下点是否落在标题栏/边框/系统按钮等非客户区。
///
/// 拖窗口必须在这里被拒绝：拖完松手时 UIA 仍可能读到文档里的旧选区，
/// 只看「松手点下有没有选区」会把拖窗口误判成划词。
unsafe fn started_on_chrome(point: POINT) -> bool {
    let window = WindowFromPoint(point);
    if window.0.is_null() {
        return false;
    }
    let root = GetAncestor(window, GA_ROOT);
    if root.0.is_null() {
        return false;
    }
    let packed = ((point.y as i16 as isize) << 16) | (point.x as i16 as isize);
    let hit = SendMessageW(root, WM_NCHITTEST, None, Some(LPARAM(packed)));
    is_chrome_hit(hit.0 as i32)
}

fn is_chrome_hit(code: i32) -> bool {
    // HTCAPTION=2、HTSYSMENU=3、HTMINBUTTON..HTBORDER=8..=18、
    // HTCLOSE=20、HTHELP=21；HTCLIENT=1 与 HTNOWHERE=0 不算。
    matches!(code, 2 | 3 | 8..=18 | 20 | 21)
}

unsafe fn foreground_is_self() -> bool {
    let foreground = GetForegroundWindow();
    process_is_self(foreground)
}

/// 前台窗口的类名与 pid，只用于结构化诊断，不携带任何文本内容。
pub unsafe fn foreground_identity() -> (String, u32) {
    let foreground = GetForegroundWindow();
    if foreground.0.is_null() {
        return ("unknown".into(), 0);
    }
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(foreground, Some(&mut pid));
    let mut buffer = [0u16; 128];
    let length = GetClassNameW(foreground, &mut buffer);
    let class = if length > 0 {
        String::from_utf16_lossy(&buffer[..length as usize])
    } else {
        "unknown".into()
    };
    (class, pid)
}

fn rejection_code(rejection: SelectionRejection) -> &'static str {
    match rejection {
        SelectionRejection::Empty => "empty",
        SelectionRejection::TooLong => "too_long",
        SelectionRejection::CoversDocument => "covers_document",
    }
}

/// 哪些 UIA 失败值得再试一次剪贴板兜底。
///
/// `NoSelection` / `EmptySelection` 说明控件本身支持 TextPattern，只是这次没有选区：
/// 注入 Ctrl+C 只会白改一次剪贴板，不该兜底。其余失败都说明「根本没读到选区」。
fn uia_failure_allows_clipboard_fallback(failure: SelectionFailure) -> bool {
    !matches!(
        failure,
        SelectionFailure::NoSelection | SelectionFailure::EmptySelection
    )
}

/// 窗口所属进程 id。剪贴板兜底用它复核「松手时的前台进程是否仍是现在的」。
unsafe fn window_process_id(window: HWND) -> u32 {
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(window, Some(&mut pid));
    pid
}

unsafe fn process_is_self(window: HWND) -> bool {
    window_process_id(window) == std::process::id()
}

/// 选区变化窗口是否属于当前前台窗口（同一顶层窗口），避免后台应用的选区变化误弹图标。
unsafe fn same_as_foreground(window: HWND) -> bool {
    let foreground = GetForegroundWindow();
    let root = GetAncestor(window, GA_ROOT);
    let foreground_root = GetAncestor(foreground, GA_ROOT);
    !root.0.is_null() && !foreground_root.0.is_null() && root == foreground_root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_chrome_hits_are_rejected_but_client_area_is_allowed() {
        assert!(is_chrome_hit(2)); // HTCAPTION：拖窗口
        assert!(is_chrome_hit(3)); // HTSYSMENU
        assert!(is_chrome_hit(10)); // HTLEFT：拉边框
        assert!(is_chrome_hit(17)); // HTBOTTOMRIGHT
        assert!(is_chrome_hit(20)); // HTCLOSE
        assert!(!is_chrome_hit(0)); // HTNOWHERE
        assert!(!is_chrome_hit(1)); // HTCLIENT：正文区
    }

    #[test]
    fn diagnostic_lines_carry_stage_and_reason_but_never_free_text() {
        let line = diagnostic_line("uia", "no_text_pattern", "WeChatMainWndForPC", 1234);
        assert_eq!(
            line,
            "[bead][selection] stage=uia reason=no_text_pattern app_class=WeChatMainWndForPC pid=1234"
        );
        // 异常类名（含空白/引号/非 ASCII）被压成可判定的 ASCII 片段。
        let class = normalize_window_class("怪 类\"名\n");
        assert_eq!(class, "______");
        let line = diagnostic_line("anchor", "window_missing", "怪 类\"名\n", 7);
        assert!(line.contains("app_class=______"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn window_class_normalization_is_bounded_and_stable() {
        assert_eq!(normalize_window_class(""), "unknown");
        assert_eq!(normalize_window_class("Chrome_WidgetWin_1"), "Chrome_WidgetWin_1");
        assert_eq!(normalize_window_class("a b\tc"), "a_b_c");
        let long = "x".repeat(100);
        assert_eq!(normalize_window_class(&long).len(), 48);
    }

    #[test]
    fn gate_rejection_codes_are_stable() {
        assert_eq!(rejection_code(SelectionRejection::Empty), "empty");
        assert_eq!(rejection_code(SelectionRejection::TooLong), "too_long");
        assert_eq!(
            rejection_code(SelectionRejection::CoversDocument),
            "covers_document"
        );
    }

    #[test]
    fn only_missing_or_unreachable_selections_trigger_clipboard_fallback() {
        assert!(uia_failure_allows_clipboard_fallback(
            SelectionFailure::NoTextPattern
        ));
        assert!(uia_failure_allows_clipboard_fallback(
            SelectionFailure::NoFocusedElement
        ));
        assert!(uia_failure_allows_clipboard_fallback(
            SelectionFailure::NoPointElement
        ));
        // 控件支持 TextPattern，只是没选中：不该为了兜底去改剪贴板。
        assert!(!uia_failure_allows_clipboard_fallback(
            SelectionFailure::NoSelection
        ));
        assert!(!uia_failure_allows_clipboard_fallback(
            SelectionFailure::EmptySelection
        ));
    }

    #[test]
    fn diagnostics_keep_only_the_last_failure() {
        let diagnostics = SelectionDiagnostics::default();
        diagnostics.note("uia", "no_text_pattern", "WeChatMainWndForPC", 9);
        diagnostics.note("click", "pending_missing", "Chrome_WidgetWin_1", 8);
        let last = diagnostics.last().expect("last failure");
        assert_eq!(last.stage, "click");
        assert_eq!(last.reason, "pending_missing");
        assert_eq!(last.class, "Chrome_WidgetWin_1");
        assert_eq!(last.pid, 8);
    }
}
