#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri 壳层：持有全部系统能力与业务状态，WebView 只发 intent、只收 state 快照。
//!
//! 剪贴板读取只出现在 `consume` 的 `ReadClipboardOnce` 分支里，且每个 intent 最多读一次；
//! 这里没有监听器、没有轮询、没有后台刷新。默认被动因此是结构性的，不靠设置开关去维持。
//! 翻译经 bead-core 的 ProviderChain（回退/重试/熔断）落到 bead-providers 的真实引擎；
//! key 只存在 AppState 内存与用户配置目录里，不写日志、不明文回显。
//! 设置（自动读剪贴板、悬停延迟、引擎、语言池）持久化到系统配置目录（工单 05）。

mod history;
mod settings;
#[cfg(windows)]
mod speech_local;
#[cfg(not(windows))]
mod speech_local;
// 选区锚点图标三件套目前只在 Windows 实现（UIA 与 Raw Input 均是 Win32 能力）。
// macOS 走辅助功能 API、Linux 走 PRIMARY 选区，属于后续平台适配，不在本阶段伪造桩。
#[cfg(windows)]
mod selection_anchor;
#[cfg(windows)]
mod selection_clipboard;
#[cfg(windows)]
mod selection_watch;
#[cfg(windows)]
mod uia;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bead_core::{
    check_for_update, choose_voice, complete_translations, ApplyOutcome, ChainConfig, Clock,
    CommitOutcome, HeuristicDetector, HoverAction, HoverEvent, HoverState, HoverStateMachine, Lang,
    LanguagePool, OnlineTts, ProviderChain, SensitiveFilter, SensitiveVerdict, SlotStatus,
    SpeechError, SpeechManager, SpeechProfile, SyncCoordinator, SyncPlan, UpdateStatus, VoiceInfo,
};
use bead_providers::{build_translator, list_models, Engine, GitHubReleaseFetcher, ProviderConfig, UreqTransport};
use serde::Serialize;
use tauri::{Emitter, LogicalPosition, LogicalSize, Manager, WebviewWindow};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_clipboard_manager::ClipboardExt;

struct SystemClock;

// --- Windows 划词取词：全局热键 + 模拟 Ctrl+C（保存/恢复剪贴板） ---
#[cfg(windows)]
mod win {
    #[repr(C)]
    pub struct Msg {
        pub hwnd: isize,
        pub message: u32,
        pub wparam: usize,
        pub lparam: isize,
        pub time: u32,
        pub pt_x: i32,
        pub pt_y: i32,
    }

    extern "system" {
        pub fn RegisterHotKey(hwnd: isize, id: u32, modifiers: u32, vk: u32) -> i32;
        pub fn UnregisterHotKey(hwnd: isize, id: u32) -> i32;
        pub fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
        pub fn PostThreadMessageW(id_thread: u32, msg: u32, wparam: usize, lparam: isize) -> i32;
        pub fn GetCurrentThreadId() -> u32;
        pub fn GetLastError() -> u32;
        pub fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
        // 划词图标点开面板时把真实窗口焦点交给主窗口，并在收起时还回原应用。
        pub fn GetForegroundWindow() -> isize;
        pub fn SetForegroundWindow(hwnd: isize) -> i32;
        pub fn IsWindow(hwnd: isize) -> i32;
    }

    pub const WM_HOTKEY: u32 = 0x0312;
    pub const WM_QUIT: u32 = 0x0012;
    pub const MOD_ALT: u32 = 0x0001;
    pub const MOD_CONTROL: u32 = 0x0002;
    pub const MOD_SHIFT: u32 = 0x0004;
    pub const MOD_WIN: u32 = 0x0008;
    pub const KEYEVENTF_KEYUP: u32 = 0x0002;
    /// RegisterHotKey 返回失败且 GetLastError 为该值时表示快捷键已被其他程序占用。
    pub const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;
}

/// 模拟 Ctrl+C 让前台程序把选中文本复制进剪贴板。
#[cfg(windows)]
fn simulate_copy() {
    unsafe {
        win::keybd_event(0x11, 0, 0, 0); // Ctrl down
        win::keybd_event(0x43, 0, 0, 0); // C down
        std::thread::sleep(std::time::Duration::from_millis(30));
        win::keybd_event(0x43, 0, win::KEYEVENTF_KEYUP, 0);
        win::keybd_event(0x11, 0, win::KEYEVENTF_KEYUP, 0);
    }
}

/// 划词热键句柄：改键/退出时先反注册再结束线程，保证旧键不会占住新键。
#[cfg(windows)]
struct HotkeyHandle {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl HotkeyHandle {
    fn stop(&mut self) {
        let Some(join) = self.join.take() else {
            return;
        };
        unsafe {
            let _ = win::PostThreadMessageW(self.thread_id, win::WM_QUIT, 0, 0);
        }
        let _ = join.join();
    }
}

#[cfg(windows)]
impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
const HOTKEY_ID: u32 = 1;

#[cfg(windows)]
fn hotkey_vk(key: bead_core::HotkeyKey) -> u32 {
    use bead_core::HotkeyKey;
    match key {
        HotkeyKey::Letter(ch) => 0x41 + (ch as u32 - 'A' as u32),
        HotkeyKey::Digit(digit) => 0x30 + digit as u32,
        HotkeyKey::Function(index) => 0x70 + (index as u32 - 1),
    }
}

#[cfg(windows)]
fn hotkey_modifiers(hotkey: &bead_core::Hotkey) -> u32 {
    let mut modifiers = 0;
    if hotkey.ctrl {
        modifiers |= win::MOD_CONTROL;
    }
    if hotkey.alt {
        modifiers |= win::MOD_ALT;
    }
    if hotkey.shift {
        modifiers |= win::MOD_SHIFT;
    }
    if hotkey.win {
        modifiers |= win::MOD_WIN;
    }
    modifiers
}

/// 启动热键线程。注册失败时线程立即退出并回报原因，由设置页明确提示，不静默。
#[cfg(windows)]
fn start_hotkey(app: tauri::AppHandle, hotkey: bead_core::Hotkey) -> Result<HotkeyHandle, String> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    let join = std::thread::Builder::new()
        .name("bead-hotkey".into())
        .spawn(move || unsafe {
            let thread_id = win::GetCurrentThreadId();
            let modifiers = hotkey_modifiers(&hotkey);
            let vk = hotkey_vk(hotkey.key);
            if win::RegisterHotKey(0, HOTKEY_ID, modifiers, vk) == 0 {
                let code = win::GetLastError();
                let message = if code == win::ERROR_HOTKEY_ALREADY_REGISTERED {
                    format!("快捷键被其他程序占用（错误 {code}），请改键或改用划词图标")
                } else {
                    format!("快捷键注册失败（错误 {code}），请改键或改用划词图标")
                };
                let _ = ready_tx.send(Err(message));
                return;
            }
            let _ = ready_tx.send(Ok(thread_id));
            let mut msg = win::Msg {
                hwnd: 0,
                message: 0,
                wparam: 0,
                lparam: 0,
                time: 0,
                pt_x: 0,
                pt_y: 0,
            };
            while win::GetMessageW(&mut msg, 0, 0, 0) > 0 {
                if msg.message == win::WM_HOTKEY && msg.wparam == HOTKEY_ID as usize {
                    let state = app.state::<AppState>();
                    if !state.settings.lock().expect("settings poisoned").selection_enabled {
                        continue;
                    }
                    // 显式热键优先：选区图标先收起，且按热键路径的剪贴板取词走。
                    dismiss_anchor(&app);
                    capture_and_translate(&app);
                }
            }
            win::UnregisterHotKey(0, HOTKEY_ID);
        });
    let join = match join {
        Ok(join) => join,
        Err(error) => return Err(format!("快捷键线程启动失败：{error}")),
    };
    match ready_rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(Ok(thread_id)) => Ok(HotkeyHandle {
            thread_id,
            join: Some(join),
        }),
        Ok(Err(message)) => {
            let _ = join.join();
            Err(message)
        }
        Err(_) => Err("快捷键注册超时，请改键或改用划词图标".into()),
    }
}

/// 应用热键文本：先停旧线程（反注册旧键），再解析并注册新键。
///
/// 解析失败时保留当前可用热键，只把错误写进状态供设置页提示；
/// 注册失败（例如 1409 已被占用）同样如实上报，绝不静默。
#[cfg(windows)]
fn apply_hotkey(app: &tauri::AppHandle, spec: &str) -> Option<String> {
    let state = app.state::<AppState>();
    let parsed = match bead_core::parse_hotkey(spec) {
        Ok(hotkey) => hotkey,
        Err(error) => {
            let message = format!("快捷键「{spec}」无效：{error}");
            *state.hotkey_error.lock().expect("hotkey error poisoned") = Some(message.clone());
            return Some(message);
        }
    };
    if let Some(mut previous) = state.hotkey.lock().expect("hotkey poisoned").take() {
        previous.stop();
    }
    match start_hotkey(app.clone(), parsed) {
        Ok(handle) => {
            *state.hotkey.lock().expect("hotkey poisoned") = Some(handle);
            *state.hotkey_error.lock().expect("hotkey error poisoned") = None;
            None
        }
        Err(message) => {
            *state.hotkey_error.lock().expect("hotkey error poisoned") = Some(message.clone());
            Some(message)
        }
    }
}

/// 取选中文本并送入：保存剪贴板 → 模拟 Ctrl+C → 读 → 恢复 → 过滤 → 展开并移交焦点 → 翻译。
///
/// 焦点在剪贴板读取与恢复之后、面板确定展开时才移交：提前抢焦点会让源应用丢掉选区，
/// 注入的 Ctrl+C 就复制不到东西。
#[cfg(windows)]
fn capture_and_translate(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let old_clipboard = app.clipboard().read_text().ok();
    simulate_copy();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let text = match app.clipboard().read_text() {
        Ok(text) => text,
        Err(_) => {
return;
        }
    };
    // 恢复用户剪贴板（取词不该留下痕迹）
    if let Some(old) = &old_clipboard {
        let _ = app.clipboard().write_text(old.clone());
    }
    // 锁定/编辑态不冲突：跳过并提示
    match state.hover.lock().expect("hover state poisoned").state() {
        HoverState::Expanded { locked: true, .. } | HoverState::Expanded { editing: true, .. } => {
return;
        }
        _ => {}
    }
    if let SensitiveVerdict::Skipped(_reason) = SensitiveFilter::classify(&text) {
return;
    }
    // 展开并送入（SelectionCaptured 展开无读取 intent，文本走本路径）
    state
        .hover
        .lock()
        .expect("hover state poisoned")
        .transition(HoverEvent::SelectionCaptured);
    // 取词与剪贴板恢复都已完成、面板确定要展开，这时才移交真实窗口焦点：
    // 面板有焦点就不会在 leave_grace_ms 后按「失焦」收起，与图标点击路径一致。
    // 记录必须在切焦点之前做，否则读到的会是本应用自己；收起时再还回源应用。
    record_focus_return(app, state.inner());
    focus_main_panel(app);
    let engine = engine_fingerprint(
        &state.provider_config.lock().expect("provider config poisoned"),
    );
    let outcome = {
        let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator.accept_clipboard_with_cache(&text, false, &engine)
    };
    match outcome {
        Some(CommitOutcome::Translate(plan)) => {
            resolve_translation_with_history(app, state.inner(), plan, engine)
        }
        Some(CommitOutcome::Cached { .. }) => {
            state.translation_epoch.fetch_add(1, Ordering::SeqCst);
            touch_history_for_current(state.inner(), &engine);
        }
        None => {}
    }
    // 窗口伸缩由前端驱动：先把 expanded 快照推过去，前端在内容全透明的那一帧再自己 resize。
    // 这里若抢先 resize，WebView 上一帧的白底面板会被留在胶囊大小的窗口里。
    // 快照在提交之后取：缓存命中的译文已经写回，不会先推一份「同步中」的旧状态。
    let view = snapshot(state.inner());
    let _ = app.emit("state", view);
}

/// 选区图标开关当前值：Raw Input 线程松手时用它复核，避免关闭后还有在途事件弹图标。
fn anchor_enabled(app: &tauri::AppHandle) -> bool {
    app.state::<AppState>()
        .settings
        .lock()
        .expect("settings poisoned")
        .selection_anchor_enabled
}

/// 自动划词连续失败时写一条降级提示，展示在设置页热键输入框下方。
///
/// 只写提示文本（含当前热键），不写任何选区内容；不主动 emit，避免打断用户正在编辑的设置表单。
#[cfg(windows)]
fn publish_selection_unsupported_hint(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let (hotkey, hotkey_enabled) = {
        let settings = state.settings.lock().expect("settings poisoned");
        (
            settings.selection_hotkey.clone(),
            settings.selection_enabled,
        )
    };
    let Some(mut hint) = state.selection_unsupported_hint.lock().ok() else {
        return;
    };
    // 热键注册与「划词（快捷键触发取词）」开关是两件事：开关没开时热键按了也不会取词。
    let text = if hotkey_enabled {
        format!("当前应用不支持自动读取选区，翻译图标无法出现。选中文本后按 {hotkey} 即可翻译。")
    } else {
        format!(
            "当前应用不支持自动读取选区，翻译图标无法出现。勾选「划词（快捷键触发取词）」后，选中文本按 {hotkey} 即可翻译。"
        )
    };
    if hint.as_deref() != Some(text.as_str()) {
        *hint = Some(text);
    }
}

#[cfg(windows)]
fn clear_selection_unsupported_hint(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let _ = state
        .selection_unsupported_hint
        .lock()
        .map(|mut hint| hint.take());
}

fn selection_unsupported_hint(state: &AppState) -> Option<String> {
    #[cfg(windows)]
    {
        return state
            .selection_unsupported_hint
            .lock()
            .ok()
            .and_then(|hint| hint.clone());
    }
    #[cfg(not(windows))]
    {
        let _ = state;
        None
    }
}

/// 收起图标并丢弃待确认文本。图标超时、切换开关、热键取词、悬停展开都走这里。
///
/// `anchor_epoch` 递增让正在 sleep 的超时线程作废：否则旧定时器会把新图标提前隐藏。
fn dismiss_anchor(app: &tauri::AppHandle) {
    #[cfg(windows)]
    selection_anchor::hide(app);
    let state = app.state::<AppState>();
    state.anchor_epoch.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut pending) = state.pending_selection.lock() {
        pending.take();
    };
}

/// 松手后 UIA 确认了选区：把文本暂存并显示图标，2.5s 后自动隐藏。
///
/// UIA 路径不写剪贴板；应用不支持 TextPattern 时快照可能来自剪贴板兜底，
/// 那条路径在到达这里之前已经尽力恢复原剪贴板。图标窗口本身不激活任何窗口。
#[cfg(windows)]
fn show_selection_anchor(
    app: &tauri::AppHandle,
    snapshot: &uia::SelectionSnapshot,
    release: bead_core::selection::Point,
) {
    let state = app.state::<AppState>();
    let diag = state.selection_diag.clone();
    let (class, pid) = unsafe { selection_watch::foreground_identity() };
    if !state.anchor_ready.load(Ordering::SeqCst) {
        diag.anchor_failed.fetch_add(1, Ordering::Relaxed);
        diag.note("anchor", "not_ready", &class, pid);
        return;
    }
    // 锁定/编辑态下图标会遮住正在编辑的卡片，且覆盖当前内容违反交互基线。
    let busy_reason = match state.hover.lock().expect("hover state poisoned").state() {
        HoverState::Expanded { locked: true, .. } => Some("panel_locked"),
        HoverState::Expanded { editing: true, .. } => Some("card_editing"),
        _ => None,
    };
    if let Some(reason) = busy_reason {
        diag.anchor_failed.fetch_add(1, Ordering::Relaxed);
        diag.note("anchor", reason, &class, pid);
        return;
    }
    if let Err(failure) = selection_anchor::show_selection(app, snapshot, release) {
        diag.anchor_failed.fetch_add(1, Ordering::Relaxed);
        diag.note("anchor", failure.code(), &class, pid);
        return;
    }
    diag.anchor_shown.fetch_add(1, Ordering::Relaxed);
    *state
        .pending_selection
        .lock()
        .expect("pending selection poisoned") = Some(snapshot.text.clone());
    let epoch = state.anchor_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(
            bead_core::selection::ANCHOR_VISIBLE_MS,
        ));
        let state = handle.state::<AppState>();
        if state.anchor_epoch.load(Ordering::SeqCst) == epoch {
            #[cfg(windows)]
            selection_anchor::hide(&handle);
            if let Ok(mut pending) = state.pending_selection.lock() {
                pending.take();
            };
        }
    });
}

/// 按开关启停 Raw Input 监听。关闭时停线程并反注册设备，此后不注册任何全局监听。
///
/// 监听线程启动失败或图标窗口建不出来时，把内存里的开关与能力位一起落回不可用，
/// 让设置页如实置灰（规范：能力探测失败不得静默 no-op）。
fn apply_anchor_watcher(app: &tauri::AppHandle, enabled: bool) {
    #[cfg(windows)]
    {
        let state = app.state::<AppState>();
        let mut watcher = state
            .selection_watch
            .lock()
            .expect("selection watcher poisoned");
        if !enabled {
            if let Some(handle) = watcher.take() {
                handle.stop();
            }
            clear_selection_unsupported_hint(app);
        } else if !state.anchor_ready.load(Ordering::SeqCst) {
            // 图标窗口不可用：开关不能在界面上保持「已开启」。
            state
                .settings
                .lock()
                .expect("settings poisoned")
                .selection_anchor_enabled = false;
        } else if watcher.is_none() {
            match selection_watch::start(app.clone()) {
                Some(handle) => *watcher = Some(handle),
                None => {
                    state.anchor_ready.store(false, Ordering::SeqCst);
                    state.selection_diag.note("watcher", "start_failed", "unknown", 0);
                    state
                        .settings
                        .lock()
                        .expect("settings poisoned")
                        .selection_anchor_enabled = false;
                }
            }
        }
    }
    // 关闭、或刚才因能力不可用被落回关闭：都要把图标与待确认文本一起清掉。
    if !enabled || !anchor_enabled(app) {
        dismiss_anchor(app);
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the unix epoch")
            .as_millis() as u64
    }
}

/// 在线 TTS：Google translate_tts（无 key）。只在「该语言没有本地音色」且用户允许
/// 在线兜底时才调用；有本地音色时绝不把文本发往网络。
struct GoogleTtsOnline {
    transport: bead_providers::UreqByteTransport,
}

impl OnlineTts for GoogleTtsOnline {
    fn fetch(&self, language: &Lang, text: &str) -> Result<Vec<u8>, SpeechError> {
        bead_providers::GoogleTts::fetch(&self.transport, language, text)
    }
}

/// 一次朗读请求的结果。`kind`：local = 本地引擎已开口；file = 前端用 `<audio>` 播；
/// error / unavailable / cancelled = 明确失败或已被新请求取代，前端据此出轻提示。
#[derive(Serialize, Clone)]
struct SpeakResult {
    kind: String,
    ticket: u64,
    lang: String,
    backend: String,
    url: String,
    error: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize, Clone, Default)]
struct SpeechView {
    phase: String,
    lang: String,
    backend: String,
    ticket: u64,
    error: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize, Clone)]
struct SpeechCapabilityView {
    lang: String,
    enabled: bool,
    backend: String,
    reason: Option<String>,
}

#[derive(Serialize, Clone)]
struct SpeechCapabilitiesView {
    online_fallback: bool,
    rate: f64,
    volume: u8,
    languages: Vec<SpeechCapabilityView>,
    /// 枚举系统语音失败时如实回传；此时「本地/在线」判断只能标成未知或在线。
    voice_error: Option<String>,
}

struct VoiceCache {
    voices: Vec<VoiceInfo>,
    fetched_at: Option<Instant>,
    error: Option<String>,
}

impl Default for VoiceCache {
    fn default() -> Self {
        Self {
            voices: Vec::new(),
            fetched_at: None,
            error: None,
        }
    }
}

struct ActiveSpeech {
    ticket: u64,
    lang: String,
    text: String,
    backend: String,
    playback: Option<Arc<Mutex<speech_local::LocalPlayback>>>,
}

/// 全进程唯一的朗读控制器。文档/翻译同步链完全不知道它存在：
/// 只读一次卡片文本，之后所有状态都在这里，不写回 `Document`、不递增翻译代次、
/// 不参与面板动画。`latest-wins` 由 ticket 保证；晚到的在线结果按 ticket 丢弃。
struct SpeechService {
    ticket: AtomicU64,
    active: Mutex<Option<ActiveSpeech>>,
    voices: Mutex<VoiceCache>,
    status: Mutex<SpeechView>,
    manager: SpeechManager<GoogleTtsOnline>,
}

impl SpeechService {
    fn new(manager: SpeechManager<GoogleTtsOnline>) -> Self {
        Self {
            ticket: AtomicU64::new(0),
            active: Mutex::new(None),
            voices: Mutex::new(VoiceCache::default()),
            status: Mutex::new(SpeechView {
                phase: "idle".into(),
                ..Default::default()
            }),
            manager,
        }
    }

    fn current_ticket(&self) -> u64 {
        self.ticket.load(Ordering::SeqCst)
    }

    fn is_current(&self, ticket: u64) -> bool {
        self.current_ticket() == ticket
    }

    fn begin(&self, app: &tauri::AppHandle, lang: &str, text: &str) -> u64 {
        let ticket = self.ticket.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(mut old) = self.active.lock().expect("speech active poisoned").take() {
            if let Some(playback) = old.playback.take() {
                playback.lock().expect("speech playback poisoned").stop();
            }
            let view = SpeechView {
                phase: "stopped".into(),
                lang: old.lang,
                backend: old.backend,
                ticket: old.ticket,
                error: None,
                reason: Some("superseded".into()),
            };
            emit_speech(app, &view);
        }
        *self.active.lock().expect("speech active poisoned") = Some(ActiveSpeech {
            ticket,
            lang: lang.to_string(),
            text: text.to_string(),
            backend: "pending".into(),
            playback: None,
        });
        ticket
    }

    fn set_backend(&self, ticket: u64, backend: &str) {
        if let Some(active) = self
            .active
            .lock()
            .expect("speech active poisoned")
            .as_mut()
            .filter(|active| active.ticket == ticket)
        {
            active.backend = backend.to_string();
        }
    }

    fn playing(&self, app: &tauri::AppHandle, ticket: u64, lang: &str, backend: &str) {
        if !self.is_current(ticket) {
            return;
        }
        self.set_backend(ticket, backend);
        self.set_status(
            app,
            SpeechView {
                phase: "playing".into(),
                lang: lang.to_string(),
                backend: backend.to_string(),
                ticket,
                error: None,
                reason: None,
            },
        );
    }

    fn resolving(&self, app: &tauri::AppHandle, ticket: u64, lang: &str) {
        if !self.is_current(ticket) {
            return;
        }
        self.set_status(
            app,
            SpeechView {
                phase: "resolving".into(),
                lang: lang.to_string(),
                backend: "pending".into(),
                ticket,
                error: None,
                reason: None,
            },
        );
    }

    fn finish(&self, app: &tauri::AppHandle, ticket: u64) {
        let removed = {
            let mut active = self.active.lock().expect("speech active poisoned");
            match active.as_ref() {
                Some(current) if current.ticket == ticket => active.take(),
                _ => None,
            }
        };
        if let Some(active) = removed {
            let view = SpeechView {
                phase: "finished".into(),
                lang: active.lang,
                backend: active.backend,
                ticket,
                error: None,
                reason: None,
            };
            self.set_status(app, view);
        }
    }

    fn fail(&self, app: &tauri::AppHandle, ticket: u64, lang: &str, backend: &str, error: String) {
        // 停止/抢占之后才回来的失败必须丢弃：否则会把「已停止」的界面又推成错误提示。
        if !self.is_current(ticket) {
            return;
        }
        let removed = {
            let mut active = self.active.lock().expect("speech active poisoned");
            match active.as_ref() {
                Some(current) if current.ticket == ticket => active.take(),
                _ => None,
            }
        };
        let lang = removed
            .map(|active| active.lang)
            .unwrap_or_else(|| lang.to_string());
        self.set_status(
            app,
            SpeechView {
                phase: "error".into(),
                lang,
                backend: backend.to_string(),
                ticket,
                error: Some(error),
                reason: None,
            },
        );
    }

    fn stop(&self, app: &tauri::AppHandle, reason: &str) -> SpeechView {
        let ticket = self.ticket.fetch_add(1, Ordering::SeqCst) + 1;
        let removed = self.active.lock().expect("speech active poisoned").take();
        let had_active = removed.is_some();
        if let Some(mut old) = removed {
            if let Some(playback) = old.playback.take() {
                playback.lock().expect("speech playback poisoned").stop();
            }
            let view = SpeechView {
                phase: "stopped".into(),
                lang: old.lang,
                backend: old.backend,
                ticket: old.ticket,
                error: None,
                reason: Some(reason.to_string()),
            };
            emit_speech(app, &view);
        }
        let idle = SpeechView {
            phase: "idle".into(),
            lang: String::new(),
            backend: String::new(),
            ticket,
            error: None,
            reason: had_active.then(|| reason.to_string()),
        };
        *self.status.lock().expect("speech status poisoned") = idle.clone();
        idle
    }

    fn set_status(&self, app: &tauri::AppHandle, view: SpeechView) {
        *self.status.lock().expect("speech status poisoned") = view.clone();
        emit_speech(app, &view);
    }

    fn status_view(&self) -> SpeechView {
        self.status.lock().expect("speech status poisoned").clone()
    }

    fn active_lang_and_text(&self) -> Option<(String, String)> {
        self.active
            .lock()
            .expect("speech active poisoned")
            .as_ref()
            .map(|active| (active.lang.clone(), active.text.clone()))
    }

    fn refresh_voices(&self, force: bool) -> Result<Vec<VoiceInfo>, String> {
        {
            let cache = self.voices.lock().expect("speech voices poisoned");
            if !force {
                if let Some(fetched_at) = cache.fetched_at {
                    if fetched_at.elapsed() < Duration::from_secs(60) {
                        return match &cache.error {
                            Some(error) if cache.voices.is_empty() => Err(error.clone()),
                            _ => Ok(cache.voices.clone()),
                        };
                    }
                }
            }
        }

        let result = speech_local::list_voices();
        let mut cache = self.voices.lock().expect("speech voices poisoned");
        cache.fetched_at = Some(Instant::now());
        match result {
            Ok(voices) => {
                cache.voices = voices.clone();
                cache.error = None;
                Ok(voices)
            }
            Err(error) => {
                cache.error = Some(error.clone());
                if cache.voices.is_empty() {
                    Err(error)
                } else {
                    // 刷新失败但手里有旧列表：继续用旧列表，同时把错误留给能力查询展示。
                    Ok(cache.voices.clone())
                }
            }
        }
    }

    fn voice_snapshot(&self) -> (Vec<VoiceInfo>, Option<String>) {
        let cache = self.voices.lock().expect("speech voices poisoned");
        (cache.voices.clone(), cache.error.clone())
    }
}

fn emit_speech(app: &tauri::AppHandle, view: &SpeechView) {
    let _ = app.emit("speech", view.clone());
}

fn speak_error_result(
    ticket: u64,
    lang: &str,
    backend: &str,
    kind: &str,
    message: String,
    reason: Option<&str>,
) -> SpeakResult {
    SpeakResult {
        kind: kind.into(),
        ticket,
        lang: lang.into(),
        backend: backend.into(),
        url: String::new(),
        error: Some(message),
        reason: reason.map(|value| value.to_string()),
    }
}

struct AppState {
    coordinator: Mutex<SyncCoordinator<SystemClock>>,
    hover: Mutex<HoverStateMachine>,

    chain: Mutex<ProviderChain<SystemClock>>,
    provider_config: Mutex<ProviderConfig>,
    settings: Mutex<settings::AppSettings>,
    settings_dir: Mutex<Option<PathBuf>>,
    /// 落盘的翻译历史（MRU、最多 10 条）；启动早期为空，setup 拿到配置目录后加载。
    history: Mutex<history::TranslationHistory>,
    speech: SpeechService,
    update_latest: Mutex<Option<String>>,
    /// 翻译任务代次：只有最新一代可以推送 state，防止慢请求回来覆盖新内容。
    translation_epoch: AtomicU64,
    /// 选区图标待确认的文本：点击图标才消费；超时/关开关/热键取词即丢弃。
    pending_selection: Mutex<Option<String>>,
    /// 图标显示代次：定时隐藏据此放弃旧定时器，避免把新图标提前收掉。
    anchor_epoch: AtomicU64,
    /// 图标窗口是否创建成功；失败时设置项如实标为不可用（不静默 no-op）。
    anchor_ready: AtomicBool,
    /// 键盘选区兜底开关（默认关闭）：监听线程据此注册/反注册键盘设备与事件钩子。
    keyboard_fallback: Arc<AtomicBool>,
    /// 划词热键线程句柄；改键与退出时先反注册旧键再结束线程。
    #[cfg(windows)]
    hotkey: Mutex<Option<HotkeyHandle>>,
    /// 热键注册失败/文本无效的明确提示；设置页据此展示原因（不静默降级）。
    hotkey_error: Mutex<Option<String>>,
    /// Raw Input 监听句柄；关闭开关时 take 并 stop，线程内反注册设备。
    #[cfg(windows)]
    selection_watch: Mutex<Option<selection_watch::WatcherHandle>>,
    /// 划词链路结构化诊断：计数 + 最近一次失败 stage/reason。
    /// 只记录窗口类名/pid，绝不记录选中文本或按键内容。
    #[cfg(windows)]
    selection_diag: Arc<selection_watch::SelectionDiagnostics>,
    /// 自动划词连续失败时给设置页的降级提示（热键兜底）；自动识别成功后清除。
    #[cfg(windows)]
    selection_unsupported_hint: Mutex<Option<String>>,
    /// 点开划词面板前的原前台窗口句柄：面板收起时若焦点仍在本应用，还给这个窗口。
    /// 用户如果已经点了别的应用，前台不再是本窗口，收起时不会抢回焦点（见 restore_focus）。
    #[cfg(windows)]
    focus_return: Mutex<Option<isize>>,
}

#[derive(Serialize, Clone)]
struct CardView {
    lang: String,
    text: String,
    status: String,
    authoritative: bool,
}

/// provider 设置视图：key/secret 绝不回传前端，只回传「是否已配置」。
#[derive(Serialize, Clone)]
struct ProviderView {
    engine: String,
    has_key: bool,
    has_secret: bool,
    needs_secret: bool,
    model: String,
    base_url: String,
}

/// 设置视图（全量回显——没有机密）。
#[derive(Serialize, Clone)]
struct SettingsView {
    auto_read_clipboard: bool,
    hover_delay_ms: u64,
    /// 前端据此排 grace tick；必须从 Rust 读，写死常量会让自定义延迟永远不生效。
    leave_grace_ms: u64,
    selection_enabled: bool,
    selection_hotkey: String,
    /// 选区锚点图标开关：Windows 才可用，不可用时前端把开关置灰。
    selection_anchor_enabled: bool,
    selection_anchor_supported: bool,
    /// 键盘选区兜底开关（注册键盘设备与文本选区事件，不记录键值）。默认关闭。
    selection_keyboard_fallback: bool,
    /// 热键注册失败或文本无效的明确原因；为空表示当前热键可用。
    selection_hotkey_error: Option<String>,
    autostart_enabled: bool,
    auto_check_updates: bool,
    /// 朗读语速倍率（0.5–2.0）。本地引擎直接生效，在线音频由前端倍速播放。
    tts_rate: f64,
    /// 朗读音量（0–100）。
    tts_volume: u8,
    /// 该语言没有本地音色时，是否允许把文本发往在线朗读服务。默认开启。
    tts_online_fallback: bool,
}

#[derive(Serialize, Clone)]
struct StateView {
    view: String,
    locked: bool,
    cards: Vec<CardView>,

    provider: ProviderView,
    settings: SettingsView,
    update_latest: Option<String>,
}

fn status_name(status: SlotStatus) -> &'static str {
    match status {
        SlotStatus::Idle => "idle",
        SlotStatus::Syncing => "syncing",
        SlotStatus::Error => "error",
    }
}

fn snapshot(state: &AppState) -> StateView {
    let hover = state.hover.lock().expect("hover state poisoned");
    let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
    let hover_state = hover.state();
    let (view, locked) = match hover_state {
        HoverState::Idle => ("capsule", false),
        HoverState::HoverPending { .. } => ("hover_pending", false),
        HoverState::Expanded { locked, .. } => ("expanded", locked),
        HoverState::Collapsing { .. } => ("collapsing", false),
    };
    // 面板展开但还没有新识别结果时，用最近一条历史填充；有在途翻译或已有新结果时不动。
    if view == "expanded" {
        preview_latest_history(&mut coordinator, state);
    }
    let authoritative = coordinator.document().authoritative();
    // 卡片顺序跟着语言池的槽位顺序走，而不是按语言 code 排序：
    // 改某张卡的语言只替换该槽位，卡片不会因为字母顺序变化而换位置。
    let cards = coordinator
        .pool()
        .languages()
        .iter()
        .filter_map(|language| {
            coordinator.document().slot(language).map(|slot| CardView {
                lang: language.code().to_string(),
                text: slot.text.clone(),
                status: status_name(slot.status).to_string(),
                authoritative: Some(language) == authoritative,
            })
        })
        .collect();

    let provider_config = state.provider_config.lock().expect("provider config poisoned");
    let settings = state.settings.lock().expect("settings poisoned");
    StateView {
        view: view.to_string(),
        locked,
        cards,

        provider: ProviderView {
            engine: provider_config.engine.code().to_string(),
            has_key: provider_config.has_key_stored(),
            has_secret: provider_config.has_secret_stored(),
            needs_secret: provider_config.engine_needs_secret(),
            model: provider_config.model.clone(),
            base_url: provider_config.base_url.clone(),
        },
        settings: SettingsView {
            auto_read_clipboard: settings.auto_read_clipboard,
            hover_delay_ms: settings.hover_delay_ms,
            leave_grace_ms: settings.leave_grace_ms,
            selection_enabled: settings.selection_enabled,
            selection_hotkey: settings.selection_hotkey.clone(),
            selection_anchor_enabled: settings.selection_anchor_enabled,
            selection_anchor_supported: cfg!(windows)
                && state.anchor_ready.load(Ordering::SeqCst),
            selection_keyboard_fallback: settings.selection_keyboard_fallback,
            selection_hotkey_error: state
                .hotkey_error
                .lock()
                .expect("hotkey error poisoned")
                .clone()
                .or_else(|| selection_unsupported_hint(state)),
            autostart_enabled: settings.autostart_enabled,
            auto_check_updates: settings.auto_check_updates,
            tts_rate: settings.tts_rate,
            tts_volume: settings.tts_volume,
            tts_online_fallback: settings.tts_online_fallback,
        },
        update_latest: state.update_latest.lock().expect("update poisoned").clone(),
    }
}

/// 面板打开且既没有新识别结果、也没有在途请求时，用最近一条历史填充面板。
///
/// 只在 `snapshot` 内同步执行：不读剪贴板、不发网络请求、不碰 hover 计时；
/// 新内容一旦提交（`authoritative` 出现）或进入翻译（`Syncing`），这里就不再介入。
fn preview_latest_history(coordinator: &mut SyncCoordinator<SystemClock>, state: &AppState) {
    let has_new_content = coordinator.document().authoritative().is_some()
        || coordinator
            .document()
            .slots()
            .any(|(_, slot)| !slot.text.is_empty() || slot.status == SlotStatus::Syncing);
    if has_new_content {
        return;
    }
    let Some(entry) = state
        .history
        .lock()
        .expect("history poisoned")
        .entries()
        .first()
        .cloned()
    else {
        return;
    };
    let translations: BTreeMap<Lang, String> = entry
        .translations
        .iter()
        .map(|(code, text)| (Lang::new(code.as_str()), text.clone()))
        .collect();
    coordinator.preview_history(
        &Lang::new(entry.source_lang.as_str()),
        &entry.source_text,
        &translations,
    );
}

/// 启动时异步检查（不阻塞启动）；手动检查同步返回结果。失败一律静默。
fn run_update_check(state: &AppState) {
    let settings = state.settings.lock().expect("settings poisoned");
    if !settings.auto_check_updates {
        return;
    }
    drop(settings);
    let status = check_for_update(&GitHubReleaseFetcher::default(), env!("CARGO_PKG_VERSION"));
    if let UpdateStatus::UpdateAvailable(_latest) = status {
}
}

/// 前往下载：只打开浏览器跳 Release 页，绝不下载/替换自身（产品红线）。
#[tauri::command]
fn open_release_page(app: tauri::AppHandle) {
    let url = "https://github.com/KrisShin/Bead/releases/latest";
    let _ = match std::env::consts::OS {
        "windows" => std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn(),
        "macos" => std::process::Command::new("open").arg(url).spawn(),
        _ => std::process::Command::new("xdg-open").arg(url).spawn(),
    };
    let _ = app;
}

/// 朗读一张卡：本地有音色就本机发声；只有缺音色且允许在线兜底时才联网。
/// 返回 `SpeakResult` 而不是整份 `StateView`——点朗读不得触发整板重渲染、不能打断编辑。
/// 网络/进程工作都在 blocking 线程，命令本身不占 IPC。
#[tauri::command]
async fn speak_card(lang: String, app: tauri::AppHandle) -> SpeakResult {
    let text = {
        let state = app.state::<AppState>();
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator
            .document()
            .slot(&Lang::new(&lang))
            .map(|slot| slot.text.clone())
            .unwrap_or_default()
    };
    run_speech(text, lang, app).await
}

/// 划词 / 小窗等上层入口统一走这里：与卡片朗读共用同一个 `SpeechService`、
/// 音色匹配、抢占与在线缓存；**不得另建引擎、队列或语音缓存**（设计 §5.1）。
#[tauri::command]
async fn speak_text(text: String, lang: String, app: tauri::AppHandle) -> SpeakResult {
    run_speech(text, lang, app).await
}

/// 朗读的实际执行路径。空文本在 `begin` 之前返回，避免无意义地抢占正在播放的卡片。
async fn run_speech(text: String, lang: String, app: tauri::AppHandle) -> SpeakResult {
    let language = Lang::new(&lang);
    let (rate, volume, online_fallback) = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().expect("settings poisoned");
        (
            settings.tts_rate,
            settings.tts_volume,
            settings.tts_online_fallback,
        )
    };
    if text.trim().is_empty() {
        return speak_error_result(
            0,
            &lang,
            "",
            "empty",
            "没有可朗读的文本".into(),
            Some("empty"),
        );
    }

    let ticket = {
        let state = app.state::<AppState>();
        state.speech.begin(&app, &lang, &text)
    };

    // 枚举系统语音可能要几百毫秒；放进 blocking 线程，避免占住异步运行时。
    let handle = app.clone();
    let voices_result = tauri::async_runtime::spawn_blocking(move || {
        handle.state::<AppState>().speech.refresh_voices(false)
    })
    .await;
    let voices = match voices_result {
        Ok(Ok(voices)) => voices,
        other => {
            if !online_fallback {
                let message = match other {
                    Ok(Err(error)) => format!("无法读取系统语音：{error}"),
                    Err(error) => format!("系统语音查询任务失败：{error}"),
                    _ => "无法读取系统语音".into(),
                };
                let state = app.state::<AppState>();
                state
                    .speech
                    .fail(&app, ticket, &lang, "none", message.clone());
                return speak_error_result(
                    ticket,
                    &lang,
                    "none",
                    "error",
                    message,
                    Some("voice_query_failed"),
                );
            }
            Vec::new()
        }
    };

    match choose_voice(&language, &voices) {
        Some(voice) => {
            let state = app.state::<AppState>();
            state.speech.resolving(&app, ticket, &lang);
            let sapi_rate = speech_local::rate_to_sapi(rate);
            match speech_local::speak(&text, &voice, sapi_rate, volume) {
                Ok(playback) => {
                    let playback = Arc::new(Mutex::new(playback));
                    let stored = {
                        let mut active = state.speech.active.lock().expect("speech active poisoned");
                        match active.as_mut().filter(|active| active.ticket == ticket) {
                            Some(active) => {
                                active.backend = "local".into();
                                active.playback = Some(playback.clone());
                                true
                            }
                            None => false,
                        }
                    };
                    if !stored {
                        playback.lock().expect("speech playback poisoned").stop();
                        return speak_error_result(
                            ticket,
                            &lang,
                            "local",
                            "cancelled",
                            String::new(),
                            Some("superseded"),
                        );
                    }
                    state.speech.playing(&app, ticket, &lang, "local");
                    spawn_local_monitor(app.clone(), ticket, lang.clone());
                    SpeakResult {
                        kind: "local".into(),
                        ticket,
                        lang,
                        backend: "local".into(),
                        url: String::new(),
                        error: None,
                        reason: None,
                    }
                }
                Err(error) => {
                    state
                        .speech
                        .fail(&app, ticket, &lang, "local", error.clone());
                    speak_error_result(
                        ticket,
                        &lang,
                        "local",
                        "error",
                        format!("本机朗读失败：{error}"),
                        Some("offline_failed"),
                    )
                }
            }
        }
        None => {
            if !online_fallback {
                let state = app.state::<AppState>();
                let message = format!(
                    "未安装 {lang} 语音，且在线兜底已关闭；可在 Windows 设置 → 时间和语言 → 语音 添加"
                );
                state
                    .speech
                    .fail(&app, ticket, &lang, "none", message.clone());
                return speak_error_result(
                    ticket,
                    &lang,
                    "none",
                    "unavailable",
                    message,
                    Some("no_voice"),
                );
            }

            {
                let state = app.state::<AppState>();
                state.speech.resolving(&app, ticket, &lang);
            }
            let profile = SpeechProfile::online(speech_local::rate_to_sapi(rate));
            let handle = app.clone();
            let request_lang = lang.clone();
            let request_text = text.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                let state = handle.state::<AppState>();
                state
                    .speech
                    .manager
                    .request(&Lang::new(&request_lang), &request_text, &profile)
            })
            .await;

            let state = app.state::<AppState>();
            match result {
                Ok(Ok(path)) => {
                    if !state.speech.is_current(ticket) {
                        return speak_error_result(
                            ticket,
                            &lang,
                            "google",
                            "cancelled",
                            String::new(),
                            Some("superseded"),
                        );
                    }
                    state.speech.playing(&app, ticket, &lang, "google");
                    SpeakResult {
                        kind: "file".into(),
                        ticket,
                        lang,
                        backend: "google".into(),
                        url: path.to_string_lossy().to_string(),
                        error: None,
                        reason: None,
                    }
                }
                Ok(Err(error)) => {
                    let message = speech_error_message(&error);
                    state
                        .speech
                        .fail(&app, ticket, &lang, "google", message.clone());
                    speak_error_result(
                        ticket,
                        &lang,
                        "google",
                        "error",
                        message,
                        Some("online_failed"),
                    )
                }
                Err(error) => {
                    let message = format!("在线朗读任务失败：{error}");
                    state
                        .speech
                        .fail(&app, ticket, &lang, "google", message.clone());
                    speak_error_result(
                        ticket,
                        &lang,
                        "google",
                        "error",
                        message,
                        Some("online_failed"),
                    )
                }
            }
        }
    }
}

fn speech_error_message(error: &SpeechError) -> String {
    match error {
        SpeechError::OnlineFailed(message) => format!("在线朗读失败：{message}"),
        SpeechError::OfflineFailed(message) => format!("本机朗读失败：{message}"),
    }
}

fn spawn_local_monitor(app: tauri::AppHandle, ticket: u64, lang: String) {
    std::thread::spawn(move || loop {
        let state = app.state::<AppState>();
        if !state.speech.is_current(ticket) {
            return;
        }
        let playback = {
            let active = state.speech.active.lock().expect("speech active poisoned");
            match active
                .as_ref()
                .filter(|active| active.ticket == ticket)
                .and_then(|active| active.playback.clone())
            {
                Some(playback) => playback,
                None => return,
            }
        };
        match playback
            .lock()
            .expect("speech playback poisoned")
            .finished()
        {
            Some(0) => {
                state.speech.finish(&app, ticket);
                return;
            }
            Some(code) => {
                state.speech.fail(
                    &app,
                    ticket,
                    &lang,
                    "local",
                    format!("本机朗读进程异常退出（代码 {code}）"),
                );
                return;
            }
            None => {}
        }
        std::thread::sleep(Duration::from_millis(120));
    });
}

/// 各语言朗读能力（只读）。前端在挂载与设置变化后调用，据此 disable 按钮、标「在线」。
#[tauri::command]
async fn speech_capabilities(app: tauri::AppHandle) -> SpeechCapabilitiesView {
    let (langs, rate, volume, online_fallback) = {
        let state = app.state::<AppState>();
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        let langs = coordinator
            .pool()
            .languages()
            .iter()
            .map(|language| language.code().to_string())
            .collect::<Vec<_>>();
        drop(coordinator);
        let settings = state.settings.lock().expect("settings poisoned");
        (
            langs,
            settings.tts_rate,
            settings.tts_volume,
            settings.tts_online_fallback,
        )
    };

    let handle = app.clone();
    let refresh = tauri::async_runtime::spawn_blocking(move || {
        handle.state::<AppState>().speech.refresh_voices(false)
    })
    .await;
    let (voices, voice_error) = {
        let state = app.state::<AppState>();
        let (voices, cached_error) = state.speech.voice_snapshot();
        let error = match refresh {
            Ok(Ok(_)) => cached_error,
            Ok(Err(error)) => Some(error),
            Err(error) => Some(format!("系统语音查询任务失败：{error}")),
        };
        (voices, error)
    };

    let languages = langs
        .into_iter()
        .map(|lang| {
            let language = Lang::new(&lang);
            if let Some(voice) = choose_voice(&language, &voices) {
                SpeechCapabilityView {
                    lang,
                    enabled: true,
                    backend: "local".into(),
                    reason: Some(format!("本机音色：{}", voice.id)),
                }
            } else if online_fallback {
                SpeechCapabilityView {
                    lang,
                    enabled: true,
                    backend: "online".into(),
                    reason: Some("未安装该语言语音，将使用在线朗读（文本会发送到在线服务）".into()),
                }
            } else {
                SpeechCapabilityView {
                    lang,
                    enabled: false,
                    backend: "none".into(),
                    reason: Some(
                        "未安装该语言语音；可在 Windows 设置 → 时间和语言 → 语音 添加".into(),
                    ),
                }
            }
        })
        .collect();

    SpeechCapabilitiesView {
        online_fallback,
        rate,
        volume,
        languages,
        voice_error,
    }
}

/// 前端在 `<audio>` 播完时回传 ticket；只有仍是最新请求才收掉「朗读中」状态。
#[tauri::command]
fn speech_finished(ticket: u64, app: tauri::AppHandle) {
    app.state::<AppState>().speech.finish(&app, ticket);
}

/// 在线音频播放失败（`<audio>` 报错）。不得静默吞掉，由前端出轻提示。
#[tauri::command]
fn speech_failed(ticket: u64, lang: String, message: String, app: tauri::AppHandle) {
    app.state::<AppState>()
        .speech
        .fail(&app, ticket, &lang, "file", message);
}

#[tauri::command]
fn speech_state(app: tauri::AppHandle) -> SpeechView {
    app.state::<AppState>().speech.status_view()
}

#[tauri::command]
fn stop_speaking(reason: String, app: tauri::AppHandle) -> SpeechView {
    app.state::<AppState>().speech.stop(&app, &reason)
}

/// 前端在「画面上已经没有任何内容」的那一帧调用，位置与尺寸用一次 SetWindowPos 提交。
///
/// 为什么由前端驱动而不是状态快照到达时自动 resize：Rust 不知道 WebView 何时才把
/// 全透明的一帧交出去。命令返回快照前就 resize，合成器手里还是上一帧的白底面板，
/// 会被留在胶囊大小的窗口里 —— 那就是偶发的白色「Bead」矩形。
#[tauri::command]
fn resize_window(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    expanded: bool,
) -> StateView {
    if let (Some(window), Some(anchor)) =
        (app.get_webview_window("main"), capsule_shape_anchor(&app))
    {
        if expanded {
            // 面板默认以胶囊为锚点向两侧张开：胶囊左缘在面板里固定 272px（整数）。
            // 贴边时整块面板平移到可用区内，收起仍按锚点还原（胶囊态 left:0）。
            let width = {
                let coordinator = state.coordinator.lock().expect("coordinator poisoned");
                board_width(coordinator.pool().languages().len())
            };
            // 边缘 clamp：胶囊贴近屏幕左右边缘时面板整体平移进可用区。胶囊在面板态
            // 不可见（opacity:0 且被 board 盖住），收起仍按锚点还原，用户看不到它动。
            let (left, top) = match monitor_work_area(&window) {
                Some(area) => place_panel(
                    (anchor.0 as f64, anchor.1 as f64),
                    width,
                    BOARD_WINDOW_HEIGHT,
                    area,
                ),
                None => (anchor.0 as f64 - CAPSULE_LEFT_IN_PANEL, anchor.1 as f64),
            };
            set_window_geometry(&window, left, top, width, BOARD_WINDOW_HEIGHT);
        } else {
            // 这一帧内容 opacity 已是 0，但窗口还盖着桌面：先关掉命中测试，
            // 别让它在缩小的几毫秒里吃掉面板区域的点击。
            let _ = window.set_ignore_cursor_events(true);
            set_window_geometry(
                &window,
                anchor.0 as f64,
                anchor.1 as f64,
                CAPSULE_WINDOW_WIDTH,
                CAPSULE_WINDOW_HEIGHT,
            );
            let _ = window.set_ignore_cursor_events(false);
        }
    }
    snapshot(state.inner())
}

/// 原子地提交「位置 + 尺寸」。
///
/// Tauri 的 set_size / set_position 是两次独立的窗口位置消息，中间稳定存在一帧
/// 「新尺寸 + 旧坐标」的错位窗口：收起时 68x28 的胶囊会先在展开位置的左端闪一下，
/// 展开时 613 宽的空窗会先挂在胶囊右缘。用一次 SetWindowPos 把两者一起提交，
/// 尺寸切换就彻底没有中间态。
#[cfg(windows)]
fn set_window_geometry(window: &tauri::WebviewWindow, x: f64, y: f64, width: f64, height: f64) {
    extern "system" {
        fn SetWindowPos(
            hwnd: isize,
            after: isize,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> isize;
    }
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let scale = window_scale(window);
    unsafe {
        SetWindowPos(
            hwnd.0 as isize,
            0,
            (x * scale).round() as i32,
            (y * scale).round() as i32,
            (width * scale).round() as i32,
            (height * scale).round() as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
fn set_window_geometry(window: &tauri::WebviewWindow, x: f64, y: f64, width: f64, height: f64) {
    let _ = window.set_size(LogicalSize::new(width, height));
    let _ = window.set_position(LogicalPosition::new(x, y));
}

/// 把窗口改到目标尺寸，锚点固定为胶囊左缘：「右缘 + 上缘」不变，变宽只向左、向下长，
/// 胶囊在屏幕上的位置始终不动，因此尺寸切换本身看不出位移。
///
/// 位置一律由锚点算出，**不读当前 inner_size 反推锚点**：异步 resize 的中间态几何
/// 会把锚点算歪（曾经把窗口卡在「展开坐标 + 胶囊尺寸」）。
fn resize_window_anchored(
    window: &WebviewWindow,
    anchor: (i32, i32),
    target_width: f64,
    target_height: f64,
) {
    set_window_geometry(
        window,
        anchor.0 as f64 + CAPSULE_WINDOW_WIDTH - target_width,
        anchor.1 as f64,
        target_width,
        target_height,
    );
}

fn window_is_oversized(window: &WebviewWindow) -> bool {
    let Ok(size) = window.inner_size() else {
        return false;
    };
    let scale = window_scale(window);
    size.width as f64 / scale > CAPSULE_WINDOW_WIDTH + 1.0
}

fn capsule_shape_anchor(app: &tauri::AppHandle) -> Option<(i32, i32)> {
    let state = app.state::<AppState>();
    let saved = state.settings.lock().expect("settings poisoned").capsule_anchor;
    saved.or_else(|| {
        app.get_webview_window("main")
            .and_then(|window| capsule_anchor(&window))
    })
}

/// 兜底收起：700ms 后若仍不在展开态、窗口还是面板尺寸，就把窗口缩回胶囊。
/// 正常路径由前端在透明帧之后调用 `resize_window(false)`，这里只防前端页面被重载
/// 后大窗口长期吞掉桌面点击。触发前重新确认尺寸，避免前端已经缩回还多缩一次。
fn schedule_fallback_shrink(app: &tauri::AppHandle) {
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(700));
        let state = handle.state::<AppState>();
        let Some(window) = handle.get_webview_window("main") else {
            return;
        };
        if !window_is_oversized(&window) {
            return; // 前端已经缩回去了
        }
        // hover_pending 也跳过：计时器马上会把窗口撑开，这时收一下只会多一次重建。
        match state.hover.lock().expect("hover state poisoned").state() {
            HoverState::Expanded { .. } | HoverState::HoverPending { .. } => return,
            _ => {}
        }
        if let Some(anchor) = capsule_shape_anchor(&handle) {
            resize_window_anchored(&window, anchor, CAPSULE_WINDOW_WIDTH, CAPSULE_WINDOW_HEIGHT);
        }
    });
}

/// 只在窗口确实还处于面板尺寸时才排兜底任务：正常收起不会为此多起一个线程。
fn maybe_schedule_fallback_shrink(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window_is_oversized(&window) {
            schedule_fallback_shrink(app);
        }
    }
}

/// 光标相对窗口的真实位置。窗口尺寸变化（胶囊↔整板）时 WebView 会补发
/// pointerleave/enter，前端分辨不了真假，由壳层用实际光标位置复核。
/// 平台取不到光标时返回 Unknown，两个方向都按旧行为放行，绝不把功能锁死。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PointerProbe {
    Inside,
    Outside,
    Unknown,
}

fn probe_pointer(window: &WebviewWindow) -> PointerProbe {
    let (Ok(cursor), Ok(outer), Ok(size)) = (
        window.cursor_position(),
        window.outer_position(),
        window.inner_size(),
    ) else {
        return PointerProbe::Unknown;
    };
    let inside = cursor.x >= outer.x as f64
        && cursor.y >= outer.y as f64
        && cursor.x <= outer.x as f64 + size.width as f64
        && cursor.y <= outer.y as f64 + size.height as f64;
    if inside {
        PointerProbe::Inside
    } else {
        PointerProbe::Outside
    }
}

fn window_scale(window: &WebviewWindow) -> f64 {
    window
        .current_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.scale_factor())
        .unwrap_or(1.0)
}

/// 窗口所在显示器的上边缘（逻辑坐标 y）。胶囊永远吸附在这条线上。
fn monitor_top(window: &WebviewWindow) -> f64 {
    window
        .current_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.position().y as f64 / monitor.scale_factor())
        .unwrap_or(0.0)
}

/// 显示器可用区（已扣除任务栏/停靠栏），逻辑坐标；与 `monitor_top` 同一套换算。
#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkArea {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl WorkArea {
    /// 物理像素换算成逻辑坐标：与 `set_position` / `set_window_geometry` 用的是
    /// 同一套「除以当前显示器 scale_factor」约定，DPI 150% 下也按逻辑像素比较。
    fn from_physical(area: PhysicalArea, scale: f64) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        Self {
            left: area.left as f64 / scale,
            top: area.top as f64 / scale,
            right: area.right as f64 / scale,
            bottom: area.bottom as f64 / scale,
        }
    }
}

/// 物理像素矩形，只用于把 Monitor 的 work_area 转成纯数据，方便单测断言。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhysicalArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// 单台显示器的可用区（按它自己的 scale_factor 折算成逻辑坐标）。
fn monitor_area(monitor: &tauri::window::Monitor) -> WorkArea {
    let area = monitor.work_area();
    WorkArea::from_physical(
        PhysicalArea {
            left: area.position.x,
            top: area.position.y,
            right: area.position.x + area.size.width as i32,
            bottom: area.position.y + area.size.height as i32,
        },
        monitor.scale_factor(),
    )
}

/// 当前窗口所在显示器的可用区（面板只允许落在单台显示器内，用它）。
fn monitor_work_area(window: &WebviewWindow) -> Option<WorkArea> {
    window
        .current_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor_area(&monitor))
}

/// 虚拟桌面所有显示器可用区的横向并集（逻辑坐标）。
///
/// 拖动胶囊用它而不是「当前显示器」：否则胶囊会被本屏左右缘夹死，永远拖不到副屏。
/// 单显示器时它就等于该显示器的可用区；多显示器时允许在屏间连续移动。
/// DPI 混合时各屏按自己的 scale_factor 折算，与窗口坐标的现有换算约定一致。
fn virtual_work_span(window: &WebviewWindow) -> Option<(f64, f64)> {
    let monitors = window.available_monitors().ok()?;
    let areas: Vec<WorkArea> = monitors.iter().map(monitor_area).collect();
    union_span(&areas)
}

/// 多块显示器可用区的横向并集；单块时就是它自己。
fn union_span(areas: &[WorkArea]) -> Option<(f64, f64)> {
    areas.iter().fold(None, |span, area| {
        Some(match span {
            Some((left, right)) => (left.min(area.left), right.max(area.right)),
            None => (area.left, area.right),
        })
    })
}

/// 胶囊拖动时的横向夹取：胶囊必须完整落在可用区横向范围内；范围比胶囊还窄时贴左缘。
fn clamp_capsule_left(desired_left: f64, capsule_width: f64, span: (f64, f64)) -> f64 {
    let (left, right) = span;
    let max_left = (right - capsule_width).max(left);
    desired_left.clamp(left, max_left)
}

/// 面板纵向夹取：面板完整落在可用区内；可用区比面板还矮时贴上缘。
fn clamp_panel_top(desired_top: f64, panel_height: f64, area: WorkArea) -> f64 {
    desired_top.clamp(area.top, (area.bottom - panel_height).max(area.top))
}

/// 面板窗口左上角：默认让胶囊落在面板的 272px 槽位（窗口左缘 = 锚点 - 272），
/// 放不下时把整块面板平移到可用区内。
///
/// 胶囊在面板态是 `opacity: 0` 且被 board 完全盖住，平移面板不会让用户看到胶囊移动；
/// 收起时窗口仍按锚点还原，胶囊出现在展开前的位置。因此「胶囊不动」与「面板完整可见」
/// 在本实现里不冲突；唯一取舍是面板比可用区还宽时贴左缘、允许向右溢出。
fn place_panel(
    anchor: (f64, f64),
    panel_width: f64,
    panel_height: f64,
    area: WorkArea,
) -> (f64, f64) {
    let left = (anchor.0 - CAPSULE_LEFT_IN_PANEL)
        .clamp(area.left, (area.right - panel_width).max(area.left));
    let top = clamp_panel_top(anchor.1, panel_height, area);
    (left, top)
}

/// 面板窗口的目标矩形（逻辑坐标）。
///
/// 纯计算，不移动窗口：前端做卡片增删补间时，需要先知道「窗口最后落在哪」，
/// 才能把内容的起点摆回旧屏幕矩形、并把补间终点对齐新窗口。真正的几何提交
/// 仍然只发生在 `resize_window(true)` 与 `sync_panel_width` 里，保证每帧只有一次
/// `SetWindowPos`；这里只是把同一套 `place_panel` 换算以只读形式暴露出去。
#[derive(Serialize, Clone, Debug, PartialEq)]
struct PanelFrame {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

fn panel_frame_at(anchor: (f64, f64), width: f64, area: Option<WorkArea>) -> PanelFrame {
    let (left, top) = match area {
        Some(area) => place_panel(anchor, width, BOARD_WINDOW_HEIGHT, area),
        None => (anchor.0 - CAPSULE_LEFT_IN_PANEL, anchor.1),
    };
    PanelFrame {
        left,
        top,
        width,
        height: BOARD_WINDOW_HEIGHT,
    }
}

/// 当前窗口矩形（逻辑坐标）。只用于 `panel_frame` 拿不到锚点时的退化返回，
/// 让前端看到「宽度未变」而跳过补间 —— 与 `sync_panel_width` 在同一条件下
/// 直接 no-op 的行为保持一致。
fn current_window_frame(window: &WebviewWindow) -> Option<PanelFrame> {
    let scale = window_scale(window);
    let position = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    Some(PanelFrame {
        left: position.x as f64 / scale,
        top: position.y as f64 / scale,
        width: size.width as f64 / scale,
        height: size.height as f64 / scale,
    })
}

/// 去掉窗口的非客户区边框。
///
/// Tauri 的无边框窗口在 Windows 上仍可能带 WS_BORDER/WS_DLGFRAME/WS_THICKFRAME
/// 与 WS_EX_WINDOWEDGE，DWM 会围着窗口画一圈 1px 边框——这就是胶囊外面那圈
/// 用 CSS 怎么去都去不掉的描边。这里直接把边框样式剥掉，改用 WS_POPUP。
#[cfg(windows)]
fn strip_nc_frame(window: &tauri::WebviewWindow) {
    const GWL_STYLE: i32 = -16;
    const GWL_EXSTYLE: i32 = -20;
    const WS_BORDER: isize = 0x0080_0000;
    const WS_DLGFRAME: isize = 0x0040_0000;
    const WS_THICKFRAME: isize = 0x0004_0000;
    const WS_SYSMENU: isize = 0x0008_0000;
    const WS_MINIMIZEBOX: isize = 0x0002_0000;
    const WS_MAXIMIZEBOX: isize = 0x0001_0000;
    const WS_POPUP: isize = 0x8000_0000;
    const WS_EX_WINDOWEDGE: isize = 0x0000_0100;
    const WS_EX_CLIENTEDGE: isize = 0x0000_0200;
    const WS_EX_STATICEDGE: isize = 0x0002_0000;
    const SWP_NOSIZE: u32 = 0x0001;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_FRAMECHANGED: u32 = 0x0020;

    extern "system" {
        fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn SetWindowPos(
            hwnd: isize,
            after: isize,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> isize;
    }

    let Ok(hwnd) = window.hwnd() else { return };
    let raw = hwnd.0 as isize;
    let mut changed = false;
    unsafe {
        let style = GetWindowLongPtrW(raw, GWL_STYLE);
        let stripped = (style
            & !(WS_BORDER
                | WS_DLGFRAME
                | WS_THICKFRAME
                | WS_SYSMENU
                | WS_MINIMIZEBOX
                | WS_MAXIMIZEBOX))
            | WS_POPUP;
        if stripped != style {
            SetWindowLongPtrW(raw, GWL_STYLE, stripped);
            changed = true;
        }
        let ex = GetWindowLongPtrW(raw, GWL_EXSTYLE);
        let ex_stripped = ex & !(WS_EX_WINDOWEDGE | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE);
        if ex_stripped != ex {
            SetWindowLongPtrW(raw, GWL_EXSTYLE, ex_stripped);
            changed = true;
        }
        if changed {
            SetWindowPos(
                raw,
                0,
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
}

#[cfg(not(windows))]
fn strip_nc_frame(_window: &tauri::WebviewWindow) {}

/// 胶囊锚点（左上角逻辑坐标），与展开/收起形态无关。
/// 面板态窗口是按「锚点 - 272」定位的，所以横向基准要按形态取不同值；
/// 纵坐标一律是窗口上缘（胶囊永远吸附在屏幕顶边）。
fn capsule_anchor(window: &WebviewWindow) -> Option<(i32, i32)> {
    let outer = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    let scale = window_scale(window);
    let left = outer.x as f64 / scale;
    let width = size.width as f64 / scale;
    let x = if width > (CAPSULE_WINDOW_WIDTH + BOARD_WINDOW_WIDTH) / 2.0 {
        left + CAPSULE_LEFT_IN_PANEL
    } else {
        left
    };
    Some((x.round() as i32, (outer.y as f64 / scale).round() as i32))
}

/// 拖动悬浮窗（无边框窗口没有系统标题栏）。
///
/// tao 的 `start_dragging` 只是 `PostMessageW(WM_NCLBUTTONDOWN, HTCAPTION)` 后立即返回，
/// **不会**阻塞到松开，因此这里不能「拖完再记位置」：位置与锚点缓存全部由 setup 里的
/// `WindowEvent::Moved` 监听维护（胶囊态夹 x 到可用区、贴显示器上缘并回写锚点）。
#[tauri::command]
fn start_drag(window: WebviewWindow) {
    let _ = window.start_dragging();
}

/// 前端自实现拖拽结束后保存窗口位置。
/// 前端在 mousemove 中实时 setPosition 并 clamp y ≥ 0，这里只做持久化。
#[tauri::command]
fn persist_position(
    x: i32,
    y: i32,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) {
    let clamped_y = y.max(0);
    {
        let mut settings = state.settings.lock().expect("settings poisoned");
        settings.capsule_anchor = Some((x, clamped_y));
        settings.window_position = Some((x, clamped_y));
    }
    persist(state.inner());
    let _ = app;
}

/// 翻译缓存键里的引擎指纹：引擎 + 模型 + 自定义入口，不含任何密钥。
///
/// 三者任一变化都意味着「同一段原文会得到不同译文」，缓存必须落空；密钥轮换不改变引擎
/// 与模型，翻译结果仍可复用，因此不参与指纹，也避免把凭据带进缓存键。
fn engine_fingerprint(config: &ProviderConfig) -> String {
    format!("{}|{}|{}", config.engine.code(), config.model, config.base_url)
}

/// 锁内过期判定：进入线程时的 epoch 比对只是粗筛，旧批次可能正卡在链锁上排队；
/// 拿到锁后再看一次最新代次，认输就直接放弃、不发网络。apply 层的 generation 校验不动。
fn locked_work_is_stale(epoch: u64, latest: u64) -> bool {
    epoch != latest
}

/// 翻译统一走链：回退/重试/熔断都在这里，调用方只看成功或「全部引擎失败」。
/// `None` 表示这一批在等锁期间已被更新的请求取代，本次不做任何网络调用。
fn run_chain(
    state: &AppState,
    plan: &SyncPlan,
    epoch: u64,
) -> Option<Result<bead_core::TranslationSet, bead_core::ProviderError>> {
    let mut chain = state.chain.lock().expect("provider chain poisoned");
    if locked_work_is_stale(epoch, state.translation_epoch.load(Ordering::SeqCst)) {
        return None;
    }
    Some(chain.translate(plan))
}

/// 翻译链放进后台线程：命令立即返回（卡片已由 coordinator 标成 syncing），
/// 完成后 emit 一次 state 让 WebView 收结果。慢请求不会占住 IPC 线程，
/// 用户在这期间仍能打字、拖动、收起面板。
fn spawn_translation(
    app: &tauri::AppHandle,
    plan: SyncPlan,
    engine: String,
    record_in_cache: bool,
    merge_history_id: Option<u64>,
) {
    let handle = app.clone();
    let epoch = {
        let state = handle.state::<AppState>();
        state.translation_epoch.fetch_add(1, Ordering::SeqCst) + 1
    };
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        if epoch != state.translation_epoch.load(Ordering::SeqCst) {
            return; // 排队期间已被更新的请求取代，别再发一次网络调用
        }
        let result = match run_chain(state.inner(), &plan, epoch) {
            Some(result) => result,
            // 等链锁期间被新请求取代：旧批次直接作废，不发网络、不写文档、不推 state。
            None => return,
        };
        // 整包成功且本次允许写缓存时，才准备历史写入。敏感文本由 `history.record`
        // 自己拦下；这里只负责把 (语言 → 译文) 整理成落盘格式。
        let history_write = match (&result, record_in_cache) {
            (Ok(set), true) => complete_translations(&plan, set).map(|items| {
                items
                    .into_iter()
                    .map(|(lang, text)| (lang.code().to_string(), text))
                    .collect::<BTreeMap<String, String>>()
            }),
            _ => None,
        };
        let mut applied = false;
        {
            let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
            match result {
                Ok(result) => {
                    // 只有整包成功才进缓存；部分失败留给用户手动重试走真实请求。
                    if record_in_cache {
                        coordinator.record_translation(&plan, &engine, &result);
                    }
                    // apply 内部按 generation 丢弃过期结果；只有真正写入才值得进历史。
                    applied = matches!(coordinator.apply(result), ApplyOutcome::Applied);
                }
                Err(_) => {}
            }
        }
        if applied {
            if let Some(translations) = history_write {
                let target_codes: Vec<String> =
                    plan.targets.iter().map(|lang| lang.code().to_string()).collect();
                let mut history = state.history.lock().expect("history poisoned");
                match merge_history_id {
                    // 增量补翻：把缺失语言的译文并回原条目并置顶，避免下次点击再补一遍。
                    Some(id) => {
                        history.merge(id, &translations, &engine);
                    }
                    None => {
                        history.record(
                            &plan.source_text,
                            plan.source_lang.code(),
                            &target_codes,
                            &translations,
                            &engine,
                        );
                    }
                }
            }
        }
        if epoch == state.translation_epoch.load(Ordering::SeqCst) {
            let view = snapshot(state.inner());
            let _ = handle.emit("state", view);
        }
    });
}

/// 缓存命中时把对应历史条目置顶：MRU 只调整顺序，不刷新时间戳（本轮没有产生新译文）。
fn touch_history_for_current(state: &AppState, engine: &str) {
    let current = {
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        let Some(source) = coordinator.document().authoritative().cloned() else {
            return;
        };
        let Some(text) = coordinator
            .document()
            .slot(&source)
            .map(|slot| slot.text.clone())
        else {
            return;
        };
        let targets: Vec<String> = coordinator
            .pool()
            .targets_excluding(&source)
            .iter()
            .map(|lang| lang.code().to_string())
            .collect();
        (text, source.code().to_string(), targets)
    };
    state
        .history
        .lock()
        .expect("history poisoned")
        .touch_matching(&current.0, &current.1, &current.2, engine);
}

/// 进程内缓存落空后先查持久化历史。
///
/// 文本、源语言、引擎都一致，且当前至少一个目标语言能从历史取到译文时：直接回填已有语言，
/// 只对缺失的目标语言合成一次增量请求；历史条目多出来的目标语言由 `restore_history` 忽略。
/// 历史与当前引擎不一致时 `find_compatible` 不命中，避免把旧引擎的译文混进新配置。
fn resolve_translation_with_history(
    app: &tauri::AppHandle,
    state: &AppState,
    plan: SyncPlan,
    engine: String,
) {
    let targets: Vec<String> = plan.targets.iter().map(|lang| lang.code().to_string()).collect();
    let entry = {
        let history = state.history.lock().expect("history poisoned");
        history.find_compatible(
            &plan.source_text,
            plan.source_lang.code(),
            &targets,
            &engine,
        )
    };
    let Some(entry) = entry else {
        spawn_translation(app, plan, engine, true, None);
        return;
    };
    {
        let mut history = state.history.lock().expect("history poisoned");
        history.touch(entry.id);
    }
    let translations: BTreeMap<Lang, String> = entry
        .translations
        .iter()
        .map(|(code, text)| (Lang::new(code.as_str()), text.clone()))
        .collect();
    let missing = {
        let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator.restore_history(&plan.source_lang, &plan.source_text, &translations)
    };
    match missing {
        Some(plan) => spawn_translation(app, plan, engine, true, Some(entry.id)),
        None => {
            // 历史已覆盖全部当前目标：没有网络请求，只推进代次作废在途旧批次。
            state.translation_epoch.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// 只有状态机产出的 intent 才能触碰系统能力。force_refresh 对应界面上的显式「读取剪贴板」。
fn consume(actions: Vec<HoverAction>, app: &tauri::AppHandle, state: &AppState, force_refresh: bool) {
    for action in actions {
        match action {
            HoverAction::ReadClipboardOnce => {
                // 关闭自动读取后：除手动粘贴/输入外无任何读取（重读按钮也拦下，规范从严）。
                if !state.settings.lock().expect("settings poisoned").auto_read_clipboard {
continue;
                }
                match app.clipboard().read_text() {
                    Ok(text) => match SensitiveFilter::classify(&text) {
                        // 敏感内容连 provider 链都不进：过滤发生在读取之后、提交之前，静默跳过。
                        SensitiveVerdict::Skipped(_) => {}
                        SensitiveVerdict::Allowed => {
                            let engine = engine_fingerprint(
                                &state.provider_config.lock().expect("provider config poisoned"),
                            );
                            let outcome = state
                                .coordinator
                                .lock()
                                .expect("coordinator poisoned")
                                .accept_clipboard_with_cache(&text, force_refresh, &engine);
                            match outcome {
                                Some(CommitOutcome::Translate(plan)) => {
                                    // 翻译链含 10s 超时 + 重试，绝不能让展开/刷新命令等它跑完。
                                    resolve_translation_with_history(app, state, plan, engine);
                                }
                                Some(CommitOutcome::Cached { .. }) => {
                                    // 命中缓存：译文已同步写回，没有任何网络请求；
                                    // 仍推进 epoch，让可能还在途的旧批次在写回/推送前作废。
                                    state.translation_epoch.fetch_add(1, Ordering::SeqCst);
                                    touch_history_for_current(state, &engine);
                                }
                                None => {}
                            }
                        }
                    },
                    // 读不到文本（空剪贴板/非文本）是常态，静默返回。
                    Err(_) => {}
                }
            }
            HoverAction::ClearDocument => {
                state
                    .coordinator
                    .lock()
                    .expect("coordinator poisoned")
                    .clear();
            }
            _ => {}
        }
    }
}

fn dispatch(
    event: HoverEvent,
    app: &tauri::AppHandle,
    state: &AppState,
    force_refresh: bool,
) -> StateView {
    let actions = state
        .hover
        .lock()
        .expect("hover state poisoned")
        .transition(event);
    consume(actions, app, state, force_refresh);
    let view = snapshot(state);
    // 真正回到胶囊的那一次：把焦点还给点开面板前的应用（仅当焦点仍由本应用持有、
    // 且目标窗口还在；用户已点了别的应用则保持现状，不抢焦点）。
    if view.view == "capsule" {
        restore_focus(app, state);
    }
    // 面板不可见（收起/ESC/点外部/锁定解除）时停止朗读；锁定与拖卡不会走到这里。
    if view.view != "expanded" && view.view != "hover_pending" {
        state.speech.stop(app, "panel_hidden");
    }
    // 窗口伸缩由前端在「内容不可见」的帧里调用 resize_window 完成；
    // 这里只在确实还大着的时候排一次兜底，防止前端页面失联把大窗口留在桌面上。
    if view.view != "expanded" {
        maybe_schedule_fallback_shrink(app);
    }
    view
}

#[tauri::command]
fn get_state(state: tauri::State<'_, AppState>) -> StateView {
    snapshot(state.inner())
}

#[tauri::command]
fn hover_enter(
    app: tauri::AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
) -> StateView {
    // 窗口缩回胶囊时 WebView 也可能补发一次 enter；鼠标其实已经在窗口外，
    // 放行会让收起过程被自己打断，重新展开。
    if probe_pointer(&window) == PointerProbe::Outside {
        return snapshot(state.inner());
    }
    // 用户已经转向胶囊：选区图标不再有意义，收起并丢弃待确认文本。
    dismiss_anchor(&app);
    dispatch(
        HoverEvent::HoverEnter {
            at_ms: SystemClock.now_ms(),
        },
        &app,
        state.inner(),
        false,
    )
}

#[tauri::command]
fn hover_leave(
    app: tauri::AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
    force: Option<bool>,
) -> StateView {
    // 展开瞬间窗口变宽，WebView 会对被重排的元素补发 leave；光标其实仍在窗口里，
    // 这一次必须吞掉，否则面板刚展开就进入 collapsing，看起来就是「触发不稳定」。
    // force=true 表示前端的「离开顶端触发区」判定，必须无条件采纳。
    if !force.unwrap_or(false) && probe_pointer(&window) == PointerProbe::Inside {
        return snapshot(state.inner());
    }
    dispatch(
        HoverEvent::HoverLeave {
            at_ms: SystemClock.now_ms(),
        },
        &app,
        state.inner(),
        false,
    )
}

#[tauri::command]
fn tick(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(
        HoverEvent::Tick {
            at_ms: SystemClock.now_ms(),
        },
        &app,
        state.inner(),
        false,
    )
}

#[tauri::command]
fn esc(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(
        HoverEvent::Esc {
            at_ms: SystemClock.now_ms(),
        },
        &app,
        state.inner(),
        false,
    )
}

#[tauri::command]
fn outside_click(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(
        HoverEvent::OutsideClick {
            at_ms: SystemClock.now_ms(),
        },
        &app,
        state.inner(),
        false,
    )
}

/// 图钉切换：已锁定则解锁，未锁定则锁定（T01 只能锁定不能解锁的缺陷在此修复）。
#[tauri::command]
fn pin(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let locked = matches!(
        state.hover.lock().expect("hover state poisoned").state(),
        HoverState::Expanded { locked: true, .. }
    );
    dispatch(
        if locked { HoverEvent::Unpin } else { HoverEvent::Pin },
        &app,
        state.inner(),
        false,
    )
}

/// 清空全部卡片：文本、槽位状态与权威源一并复位；同时推进翻译代次，
/// 让仍在途的结果在 `apply` 里整包丢弃，不会把刚清空的卡又填回去。
#[tauri::command]
fn clear_document(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    state.speech.stop(&app, "document_cleared");
    state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .clear();
    state.translation_epoch.fetch_add(1, Ordering::SeqCst);
    snapshot(state.inner())
}

/// 历史列表视图：只回传片段与元信息，不下发全部译文（点击条目时再由 Rust 回填）。
#[derive(Serialize, Clone)]
struct HistoryItemView {
    id: u64,
    /// 源文本片段（最多 120 字符，超出加省略号）。
    snippet: String,
    source_lang: String,
    targets: Vec<String>,
    created_at_ms: u64,
}

fn history_snippet(text: &str, max_chars: usize) -> String {
    let mut snippet: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        snippet.push('…');
    }
    snippet
}

fn history_items(state: &AppState) -> Vec<HistoryItemView> {
    let history = state.history.lock().expect("history poisoned");
    history
        .entries()
        .iter()
        .map(|entry| HistoryItemView {
            id: entry.id,
            snippet: history_snippet(&entry.source_text, 120),
            source_lang: entry.source_lang.clone(),
            targets: entry.targets.clone(),
            created_at_ms: entry.created_at_ms,
        })
        .collect()
}

/// 历史入口：按 MRU 顺序返回最多 10 条。
#[tauri::command]
fn list_history(state: tauri::State<'_, AppState>) -> Vec<HistoryItemView> {
    history_items(state.inner())
}

/// 点击历史条目：用已有译文回填当前卡片并置顶；只对缺失的目标语言补发一次请求。
///
/// 这是用户显式选择，即使条目引擎与当前引擎不同也回填它的译文；只有引擎一致时，
/// 增量补翻的结果才会并回原条目（`history.merge` 自己会拒绝跨引擎合并）。
#[tauri::command]
fn apply_history(
    id: u64,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let entry = {
        let mut history = state.history.lock().expect("history poisoned");
        let entry = history.get(id);
        if entry.is_some() {
            history.touch(id);
        }
        entry
    };
    let Some(entry) = entry else {
        return snapshot(state.inner());
    };
    // 回填会替换卡片文本，正在朗读的旧内容立即停止。
    state.speech.stop(&app, "history_applied");
    let engine = engine_fingerprint(
        &state.provider_config.lock().expect("provider config poisoned"),
    );
    let translations: BTreeMap<Lang, String> = entry
        .translations
        .iter()
        .map(|(code, text)| (Lang::new(code.as_str()), text.clone()))
        .collect();
    let missing = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .restore_history(
            &Lang::new(entry.source_lang.as_str()),
            &entry.source_text,
            &translations,
        );
    match missing {
        Some(plan) => spawn_translation(&app, plan, engine, true, Some(entry.id)),
        None => {
            // 条目已覆盖当前全部目标：不请求，只推进代次作废在途旧批次。
            state.translation_epoch.fetch_add(1, Ordering::SeqCst);
        }
    }
    snapshot(state.inner())
}

/// 清空历史：内存与文件一并清空；同时让进程内缓存失效，避免磁盘清了内存还在复用。
#[tauri::command]
fn clear_history(state: tauri::State<'_, AppState>) -> StateView {
    state.history.lock().expect("history poisoned").clear();
    state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .invalidate_translation_cache();
    snapshot(state.inner())
}

#[tauri::command]
fn copy_card(lang: String, app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let text = {
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator
            .document()
            .slot(&Lang::new(&lang))
            .map(|slot| slot.text.clone())
    };
    if let Some(text) = text {
        if app.clipboard().write_text(text).is_err() {
}
    }
    snapshot(state.inner())
}

/// 编辑提交：该卡成为权威源，其余卡标 syncing，翻译在后台跑完再推 state。
/// 命令本身只做一次本地提交，400ms 防抖期间连续打字不会被网络往返拖住。
#[tauri::command]
fn commit_edit(
    lang: String,
    text: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    if let Some((active_lang, active_text)) = state.speech.active_lang_and_text() {
        if active_lang == lang && active_text != text {
            state.speech.stop(&app, "text_changed");
        }
    }
    let engine = engine_fingerprint(
        &state.provider_config.lock().expect("provider config poisoned"),
    );
    let outcome = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .commit_with_cache(&Lang::new(&lang), text, &engine);
    match outcome {
        Ok(CommitOutcome::Translate(plan)) => {
            resolve_translation_with_history(&app, state.inner(), plan, engine)
        }
        Ok(CommitOutcome::Cached { .. }) => {
            // 同文重提：结果已同步写回，卡片不会停在 syncing；推进 epoch 作废在途旧批次。
            state.translation_epoch.fetch_add(1, Ordering::SeqCst);
            touch_history_for_current(state.inner(), &engine);
        }
        Err(_error) => {}
    }
    snapshot(state.inner())
}

/// 编辑态进入/退出：状态机在 editing 下不产出读取 intent，hover 不覆盖正在输入的内容。
#[tauri::command]
fn edit_start(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(HoverEvent::EditStart, &app, state.inner(), false)
}

#[tauri::command]
fn edit_end(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(HoverEvent::EditEnd, &app, state.inner(), false)
}

/// 只重试 apply 标错的卡：源仍是权威源卡，成功过的卡不发第二次请求。
///
/// 这条路径刻意不读翻译缓存：失败结果本来就不入缓存，手动重试必须真的把失败卡再打一次。
#[tauri::command]
fn retry_failed(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let engine = engine_fingerprint(
        &state.provider_config.lock().expect("provider config poisoned"),
    );
    let plan = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .retry_failed();
    if let Some(plan) = plan {
        spawn_translation(&app, plan, engine, false, None);
    }
    snapshot(state.inner())
}

/// 保存引擎设置并重建链。key 留空表示「保持已保存的值」（前端不回显明文）。
#[tauri::command(rename_all = "snake_case")]
fn set_provider(
    engine: String,
    api_key: String,
    secret: String,
    model: String,
    base_url: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let mut config = state.provider_config.lock().expect("provider config poisoned");
    let previous_fingerprint = engine_fingerprint(&config);
    match Engine::from_code(&engine) {
        Some(engine) => {
            config.engine = engine;
            // key 留空且已保存过 → 保持旧值（前端不回显明文）；否则覆盖。
            if !api_key.is_empty() || !config.has_key_stored() {
                config.api_key = api_key;
            }
            if !secret.is_empty() || !config.has_secret_stored() {
                config.secret = secret;
            }
            config.model = model;
            config.base_url = base_url;
            rebuild_chain(&config, state.inner());
            state.settings.lock().expect("settings poisoned").provider = config.clone();
}
        None => {
}
    }
    let next_fingerprint = engine_fingerprint(&config);
    drop(config);
    if next_fingerprint != previous_fingerprint {
        // 引擎/模型变了：旧译文不再可信。缓存整体失效，并把当前权威源用新引擎重翻一遍。
        let plan = {
            let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
            coordinator.invalidate_translation_cache();
            let current = coordinator.document().authoritative().cloned().and_then(|lang| {
                let text = coordinator.document().slot(&lang)?.text.clone();
                Some((lang, text))
            });
            match current {
                Some((lang, text)) if !text.trim().is_empty() => {
                    coordinator.commit(&lang, text).ok()
                }
                _ => None,
            }
        };
        if let Some(plan) = plan {
            spawn_translation(&app, plan, next_fingerprint, true, None);
        }
    }
    persist(state.inner());
    snapshot(state.inner())
}
/// 手动检查更新：结果写入 update_latest（设置面板按钮），不弹任何提示。
#[tauri::command]
fn check_updates(state: tauri::State<'_, AppState>) -> StateView {
    let current = env!("CARGO_PKG_VERSION");
    let status = check_for_update(&GitHubReleaseFetcher::default(), current);
    match status {
        UpdateStatus::UpToDate => {
}
        UpdateStatus::UpdateAvailable(_latest) => {
}
        UpdateStatus::Failed => {
}
    }
    snapshot(state.inner())
}

/// 保存面板设置（自动读剪贴板、悬停延迟、划词、选区图标、更新开关）并即时生效。
/// 延迟生效通过重建状态机完成，展开态会复位——该动作本身就来自展开的面板，代价可接受。
#[tauri::command(rename_all = "snake_case")]
fn set_settings(
    app: tauri::AppHandle,
    auto_read_clipboard: bool,
    hover_delay_ms: u64,
    selection_enabled: bool,
    selection_anchor_enabled: bool,
    selection_keyboard_fallback: bool,
    auto_check_updates: bool,
    tts_rate: Option<f64>,
    tts_volume: Option<u8>,
    tts_online_fallback: Option<bool>,
    selection_hotkey: Option<String>,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let delay = hover_delay_ms.clamp(50, 2000);
    // 非 Windows 不落地「已开启」的假状态：没有实现就没有开关。
    let anchor_on = cfg!(windows) && selection_anchor_enabled;
    let keyboard_on = cfg!(windows) && selection_keyboard_fallback;
    let hotkey_text = selection_hotkey
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    {
        let mut settings = state.settings.lock().expect("settings poisoned");
        settings.auto_read_clipboard = auto_read_clipboard;
        settings.hover_delay_ms = delay;
        settings.selection_enabled = selection_enabled;
        settings.selection_anchor_enabled = anchor_on;
        settings.selection_keyboard_fallback = keyboard_on;
        settings.auto_check_updates = auto_check_updates;
        if let Some(rate) = tts_rate {
            settings.tts_rate = rate.clamp(0.5, 2.0);
        }
        if let Some(volume) = tts_volume {
            settings.tts_volume = volume.min(100);
        }
        if let Some(online) = tts_online_fallback {
            settings.tts_online_fallback = online;
        }
        let grace = settings.leave_grace_ms;
        *state.hover.lock().expect("hover state poisoned") =
            HoverStateMachine::new(delay, grace);
    }
    // 热键文本只在解析成功后落盘：坏配置不写进 settings.json，错误仍明确回传设置页。
    if let Some(spec) = hotkey_text {
        if bead_core::parse_hotkey(&spec).is_ok() {
            state
                .settings
                .lock()
                .expect("settings poisoned")
                .selection_hotkey = spec.clone();
            #[cfg(windows)]
            apply_hotkey(&app, &spec);
        } else {
            #[cfg(windows)]
            apply_hotkey(&app, &spec);
        }
    }
    persist(state.inner());
    apply_anchor_watcher(&app, anchor_on);
    apply_keyboard_fallback(&app, keyboard_on);
    snapshot(state.inner())
}

/// 键盘兜底开关：写能力位并通知监听线程注册/反注册键盘设备与事件钩子。
fn apply_keyboard_fallback(app: &tauri::AppHandle, enabled: bool) {
    #[cfg(windows)]
    {
        let state = app.state::<AppState>();
        state.keyboard_fallback.store(enabled, Ordering::SeqCst);
        let watcher = state
            .selection_watch
            .lock()
            .expect("selection watcher poisoned");
        if let Some(handle) = watcher.as_ref() {
            handle.refresh_inputs();
        }
    }
    let _ = (app, enabled);
}

/// 开机自启开关（tauri-plugin-autostart：Win 注册表 Run / macOS LaunchAgent / XDG autostart）。
#[tauri::command]
fn set_autostart(
    enabled: bool,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let autostart = app.autolaunch();
    let result = if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    };
    match result {
        Ok(()) => {
            state.settings.lock().expect("settings poisoned").autostart_enabled = enabled;
            persist(state.inner());
        }
        Err(_error) => {}
    }
    snapshot(state.inner())
}

/// 设置落盘。目录在 setup 里由 Tauri app_config_dir 提供。
fn persist(state: &AppState) {
    let dir = state.settings_dir.lock().expect("settings dir poisoned").clone();
    if let Some(dir) = dir {
        let snapshot_settings = state.settings.lock().expect("settings poisoned").clone();
        let _ = settings::save(&dir, &snapshot_settings);
    }
}

/// 测试连接：用当前配置翻译一句样例，只验证链路是否可用，不向界面回写任何提示。
#[tauri::command]
fn test_provider(state: tauri::State<'_, AppState>) -> StateView {
    let config = state.provider_config.lock().expect("provider config poisoned").clone();
    match build_translator(&config, Box::new(UreqTransport)) {
        Ok(translator) => {
            let plan = SyncPlan {
                generation: 0,
                source_lang: Lang::new("zh"),
                source_text: "你好".into(),
                targets: vec![Lang::new("en")],
                created_at_ms: SystemClock.now_ms(),
            };
            // 只验证链路可用；结果不回写界面（用户反馈里不再有任何提示文案）
            let _ = translator.translate(&plan);
        }
        Err(_) => {}
    }
    snapshot(state.inner())
}

/// 拉取 OpenAI 兼容引擎的模型清单（运行时探测，不写死）。非兼容引擎返回空表。
#[tauri::command]
fn list_provider_models(state: tauri::State<'_, AppState>) -> Vec<String> {
    list_models(&state.provider_config.lock().expect("provider config poisoned"))
}

/// 用配置重建 provider 链：当前只挂一个引擎（多引擎回退链留待设置持久化一起做）。
fn rebuild_chain(config: &ProviderConfig, state: &AppState) {
    let mut chain = ProviderChain::new(SystemClock, ChainConfig::default());
    match build_translator(config, Box::new(UreqTransport)) {
        Ok(translator) => chain.push(translator),
        Err(_message) => {
}
    }
    *state.chain.lock().expect("provider chain poisoned") = chain;
}

/// 添加一张卡片：从常见语言里挑第一个未使用的。池已满 5 种时静默忽略。
#[tauri::command]
fn add_card(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let candidates = ["fr", "de", "es", "ru", "ko", "ja", "en", "zh"];
    let existing: Vec<String> = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .pool()
        .languages()
        .iter()
        .map(|l| l.code().to_string())
        .collect();
    if existing.len() >= 5 {
        return snapshot(state.inner());
    }
    let Some(next) = candidates
        .iter()
        .find(|c| !existing.iter().any(|e| e == *c))
    else {
return snapshot(state.inner());
    };
    let mut langs = existing;
    langs.push(next.to_string());
    apply_language_pool(&langs, &app, state.inner());
    snapshot(state.inner())
}

/// 移除一张卡片：至少保留 2 种。传一个在池里不存在的 lang 静默忽略。
#[tauri::command]
fn remove_card(
    lang: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let existing: Vec<String> = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .pool()
        .languages()
        .iter()
        .map(|l| l.code().to_string())
        .collect();
    if existing.len() <= 2 {
return snapshot(state.inner());
    }
    if !existing.contains(&lang) {
        return snapshot(state.inner());
    }
    if let Some((active_lang, _)) = state.speech.active_lang_and_text() {
        if active_lang == lang {
            state.speech.stop(&app, "card_removed");
        }
    }
    let langs: Vec<String> = existing.into_iter().filter(|l| l != &lang).collect();
    apply_language_pool(&langs, &app, state.inner());
    snapshot(state.inner())
}

/// 更换某张卡片的目标语言：from 必须在池里，to 不能与其他卡重复且总数 2–5。
#[tauri::command(rename_all = "snake_case")]
fn change_card_lang(
    from: String,
    to: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let existing: Vec<String> = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .pool()
        .languages()
        .iter()
        .map(|l| l.code().to_string())
        .collect();
    if !existing.contains(&from) || existing.contains(&to) {
        return snapshot(state.inner());
    }
    if let Some((active_lang, _)) = state.speech.active_lang_and_text() {
        if active_lang == from {
            state.speech.stop(&app, "language_changed");
        }
    }
    let langs: Vec<String> = existing
        .into_iter()
        .map(|l| if l == from { to.clone() } else { l })
        .collect();
    apply_language_pool(&langs, &app, state.inner());
    snapshot(state.inner())
}

/// 卡片拖拽排序：只允许在同一批语言内重排（成员不能变），顺序持久化到 settings.languages。
#[tauri::command]
fn reorder_cards(
    order: Vec<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let current: Vec<String> = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .pool()
        .languages()
        .iter()
        .map(|l| l.code().to_string())
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    sorted.dedup();
    let mut expected = current.clone();
    expected.sort();
    expected.dedup();
    if order.len() == current.len() && sorted == expected {
        apply_language_pool(&order, &app, state.inner());
    }
    snapshot(state.inner())
}

/// 面板宽度：卡片固定 189px、间距 10px，3 张卡为 613。
fn board_width(card_count: usize) -> f64 {
    let count = card_count.max(1) as f64;
    BOARD_CHROME_WIDTH + CARD_WIDTH_IN_PANEL * count + CARD_GAP_IN_PANEL * (count - 1.0)
}

/// 面板开着时卡片数变了：前端在「窗口 / 内容」时序里调用此命令完成窗口尺寸同步。
/// 加卡时内容已钉在旧宽、删卡时内容已收到新宽，因此这一次 `SetWindowPos`
/// 的可见画面要么是等宽内容，要么只差一条透明边，不会露出中间帧。
/// 窗口位置始终以胶囊为锚点（胶囊屏幕位置不动）。
#[tauri::command]
fn sync_panel_width(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let Some(window) = app.get_webview_window("main") else {
        return snapshot(state.inner());
    };
    if !window_is_oversized(&window) {
        return snapshot(state.inner()); // 还在胶囊形态：下次展开时 resize_window 会用新宽度
    }
    let Some(anchor) = capsule_shape_anchor(&app) else {
        return snapshot(state.inner());
    };
    let width = {
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        board_width(coordinator.pool().languages().len())
    };
    let (left, top) = match monitor_work_area(&window) {
        Some(area) => place_panel(
            (anchor.0 as f64, anchor.1 as f64),
            width,
            BOARD_WINDOW_HEIGHT,
            area,
        ),
        None => (anchor.0 as f64 - CAPSULE_LEFT_IN_PANEL, anchor.1 as f64),
    };
    set_window_geometry(&window, left, top, width, BOARD_WINDOW_HEIGHT);
    snapshot(state.inner())
}

/// 只读查询面板窗口的目标矩形：不移动窗口、不改变任何状态。
///
/// 前端在卡片增删的那一帧用它拿到「新窗口最后落在哪」（left 是否因贴边 clamp
/// 而平移），从而决定内容补间的起点与终点。几何契约与 `resize_window(true)` /
/// `sync_panel_width` 完全一致：同一套锚点、同一套 `place_panel`、同一个
/// `board_width(语言池计数)`。
#[tauri::command]
fn panel_frame(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> PanelFrame {
    let width = {
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        board_width(coordinator.pool().languages().len())
    };
    let Some(anchor) = capsule_shape_anchor(&app) else {
        // 没有锚点（异常路径）：回当前窗口矩形，前端会判定「宽度未变」而跳过补间。
        return app
            .get_webview_window("main")
            .and_then(|window| current_window_frame(&window))
            .unwrap_or(PanelFrame {
                left: 0.0,
                top: 0.0,
                width: 0.0,
                height: 0.0,
            });
    };
    let area = app
        .get_webview_window("main")
        .and_then(|window| monitor_work_area(&window));
    panel_frame_at((anchor.0 as f64, anchor.1 as f64), width, area)
}

fn apply_language_pool(languages: &[String], app: &tauri::AppHandle, state: &AppState) {
    let mut previous: Vec<String> = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .pool()
        .languages()
        .iter()
        .map(|language| language.code().to_string())
        .collect();
    previous.sort();
    let mut next: Vec<String> = languages.to_vec();
    next.sort();
    let membership_changed = previous != next;

    let pool = LanguagePool::new(languages.iter().map(|c| Lang::new(c.as_str())).collect());
    match pool {
        Ok(pool) => {
            let engine = engine_fingerprint(
                &state.provider_config.lock().expect("provider config poisoned"),
            );
            {
                let mut settings = state.settings.lock().expect("settings poisoned");
                settings.languages = pool.languages().iter().map(|l| l.code().to_string()).collect();
            }
            let plan = {
                let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
                coordinator.replace_pool(pool);
                if membership_changed {
                    // 目标语言集合变了：旧缓存必须作废，并用当前权威源把新集合翻一遍。
                    coordinator.invalidate_translation_cache();
                    let current = coordinator.document().authoritative().cloned().and_then(|lang| {
                        let text = coordinator.document().slot(&lang)?.text.clone();
                        Some((lang, text))
                    });
                    match current {
                        Some((lang, text)) if !text.trim().is_empty() => {
                            coordinator.commit(&lang, text).ok()
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            };
            persist(state);
            if let Some(plan) = plan {
                spawn_translation(app, plan, engine, true, None);
            }
            // 注意：不再在这里直接 sync_panel_width —— 可见期 resize 会被 OS 合成器抓拍到
            // 旧内容 + 新尺寸的中间帧，导致面板视觉上滑一下。现在由前端在内容不可见的
            // 帧里主动调用 sync_panel_width 命令完成尺寸同步。
            let _ = app;
        }
        Err(_error) => {}
    }
}
/// 面板头划词翻译开关：toggle_selection。单独一个命令因为它只改一个布尔字段。
#[tauri::command]
fn toggle_selection(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let _enabled = {
        let mut settings = state.settings.lock().expect("settings poisoned");
        settings.selection_enabled = !settings.selection_enabled;
        settings.selection_enabled
    };
persist(state.inner());
    let _ = app;
    snapshot(state.inner())
}

/// 划词图标点击之后，把真实窗口焦点交给主面板。
///
/// 点击落在 `WS_EX_NOACTIVATE` 的图标窗口上，原应用仍持有前台焦点；面板若一直没焦点，
/// 用户切走或点外部后前端就只能靠失焦收起。这里主动激活主窗口，面板即处于「有焦点」态，
/// 不需要为图标窗口另立例外规则，卡片输入框也能直接接收键盘。
///
/// 调用点在图标 WebView 的点击已经派发、待确认文本已经消费之后，避免抢走那一下点击；
/// 激活失败时退回 Tauri 的 `set_focus` 结果即可，图标窗口已隐藏，不会产生可见抖动。
fn focus_main_panel(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.set_focus();
    #[cfg(windows)]
    {
        // `set_focus` 在 Windows 上最终就是 SetForegroundWindow；若被前台规则拒绝，
        // 用窗口句柄补一次（点击输入发生在本进程的顶层窗口上，允许切前台）。
        if let Ok(hwnd) = window.hwnd() {
            let raw = hwnd.0 as isize;
            unsafe {
                if win::GetForegroundWindow() != raw {
                    win::SetForegroundWindow(raw);
                }
            }
        }
    }
}

/// 收起时要还原的目标窗口：只记「别的应用」。前台已是本应用自己的窗口时没有可还原的目标。
#[cfg(windows)]
fn focus_return_target(foreground: isize, main: isize) -> Option<isize> {
    (foreground != 0 && foreground != main).then_some(foreground)
}

/// 在切换焦点之前记下当前前台窗口（通常是源应用），供收起时还原。
///
/// 必须在 `focus_main_panel` 之前调用：焦点一旦切走，这里读到的就是本应用自己。
#[cfg(windows)]
fn record_focus_return(app: &tauri::AppHandle, state: &AppState) {
    let main = app
        .get_webview_window("main")
        .and_then(|window| window.hwnd().ok())
        .map(|hwnd| hwnd.0 as isize)
        .unwrap_or(0);
    let foreground = unsafe { win::GetForegroundWindow() };
    *state.focus_return.lock().expect("focus return poisoned") =
        focus_return_target(foreground, main);
}

/// 面板收起时把焦点还给点开面板前的应用。
///
/// 只在「焦点仍在主面板」时还原：用户若是点了别的应用才触发收起，前台已经是那个应用，
/// 这里直接放弃，绝不把焦点从用户正在用的窗口抢回来。目标窗口已关闭同样不再还原。
#[cfg(windows)]
fn restore_focus(app: &tauri::AppHandle, state: &AppState) {
    let target = state
        .focus_return
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    let Some(target) = target else {
        return;
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let main = hwnd.0 as isize;
    unsafe {
        if win::GetForegroundWindow() != main || win::IsWindow(target) == 0 {
            return;
        }
        win::SetForegroundWindow(target);
    }
}

#[cfg(not(windows))]
fn restore_focus(_app: &tauri::AppHandle, _state: &AppState) {}

/// 选区图标点击：消费待确认文本，走与热键取词相同的展开与翻译链路。
///
/// 图标本身是 `WS_EX_NOACTIVATE`，点击时原应用短暂保持前台；文本消费成功后由
/// `focus_main_panel` 把焦点交给主面板，之后面板不再受「窗口失焦」收起逻辑影响。
/// 文本可能是 UIA 快照，也可能来自 UIA 失败后的剪贴板兜底（兜底已在检测阶段
/// 恢复剪贴板），**点击本身不读写剪贴板**。
#[tauri::command]
fn anchor_click(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> Option<StateView> {
    #[cfg(windows)]
    let diag = state.selection_diag.clone();
    #[cfg(windows)]
    let (class, pid) = unsafe { selection_watch::foreground_identity() };
    #[cfg(windows)]
    diag.click_total.fetch_add(1, Ordering::Relaxed);

    // 先判门禁、后取文本：锁定/编辑态被拦下时不能把待确认文本消费掉，
    // 否则用户解锁后重试会拿到「没有待确认选区」。
    let enabled = anchor_enabled(&app);
    let (locked, editing) = match state.hover.lock().expect("hover state poisoned").state() {
        HoverState::Expanded { locked, editing, .. } => (locked, editing),
        _ => (false, false),
    };
    let pending_present = state
        .pending_selection
        .lock()
        .ok()
        .map(|pending| pending.is_some())
        .unwrap_or(false);
    if let Some(block) =
        selection_anchor::anchor_click_block(pending_present, enabled, locked, editing)
    {
        #[cfg(windows)]
        {
            if block == selection_anchor::AnchorClickBlock::NoPending {
                diag.click_missed.fetch_add(1, Ordering::Relaxed);
            } else {
                diag.click_blocked.fetch_add(1, Ordering::Relaxed);
            }
            diag.note("click", block.code(), &class, pid);
        }
        // 图标被点了但不该翻译：收起，避免留下一个点了没反应的图标。
        dismiss_anchor(&app);
        return None;
    }
    let Some(text) = state
        .pending_selection
        .lock()
        .ok()
        .and_then(|mut pending| pending.take())
    else {
        // 通过门禁后仍被超时线程抢先清空：同样按过期处理，并给出可判定计数。
        #[cfg(windows)]
        {
            diag.click_missed.fetch_add(1, Ordering::Relaxed);
            diag.note("click", "pending_missing", &class, pid);
        }
        dismiss_anchor(&app);
        return None;
    };
    dismiss_anchor(&app);
    if let SensitiveVerdict::Skipped(_) = SensitiveFilter::classify(&text) {
        #[cfg(windows)]
        {
            diag.click_blocked.fetch_add(1, Ordering::Relaxed);
            diag.note("click", "sensitive", &class, pid);
        }
        return None;
    }
    #[cfg(windows)]
    diag.click_consumed.fetch_add(1, Ordering::Relaxed);
    // 文本已消费、图标已隐藏，此时再切前台：不会吞掉那一下点击，也不会让图标抖动。
    // 同时记下此刻的前台窗口，收起时若焦点仍在本应用就还给它。
    #[cfg(windows)]
    record_focus_return(&app, state.inner());
    focus_main_panel(&app);
    state
        .hover
        .lock()
        .expect("hover state poisoned")
        .transition(HoverEvent::SelectionCaptured);
    let engine = engine_fingerprint(
        &state.provider_config.lock().expect("provider config poisoned"),
    );
    let outcome = {
        let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator.accept_clipboard_with_cache(&text, false, &engine)
    };
    match outcome {
        Some(CommitOutcome::Translate(plan)) => {
            resolve_translation_with_history(&app, state.inner(), plan, engine)
        }
        Some(CommitOutcome::Cached { .. }) => {
            state.translation_epoch.fetch_add(1, Ordering::SeqCst);
            touch_history_for_current(state.inner(), &engine);
        }
        None => {}
    }
    // 快照在提交之后取：缓存命中的译文已经写回，界面不会先看到一份「同步中」的旧状态。
    let view = snapshot(state.inner());
    let _ = app.emit("state", view.clone());
    Some(view)
}

#[derive(Serialize, Default)]
struct SelectionCountersView {
    gesture_candidates: u64,
    uia_ok: u64,
    uia_failed: u64,
    gate_rejections: u64,
    anchor_shown: u64,
    anchor_failed: u64,
    click_total: u64,
    click_consumed: u64,
    click_missed: u64,
    click_blocked: u64,
    fallback_attempts: u64,
    fallback_ok: u64,
    fallback_failed: u64,
}

/// 划词链路只读诊断视图：用于「连续触发 N 次后到底哪一环断了」的判定。
///
/// 不含选中文本、不含按键内容；只含计数、状态名与窗口类名/pid。
#[derive(Serialize)]
struct SelectionDebugView {
    watcher_running: bool,
    anchor_ready: bool,
    anchor_window: bool,
    anchor_visible: bool,
    anchor_enabled: bool,
    keyboard_fallback: bool,
    pending: bool,
    hover_state: &'static str,
    counters: SelectionCountersView,
    last_stage: String,
    last_reason: String,
    last_class: String,
    last_pid: u32,
}

fn hover_code(state: HoverState) -> &'static str {
    match state {
        HoverState::Idle => "idle",
        HoverState::HoverPending { .. } => "hover_pending",
        HoverState::Collapsing { .. } => "collapsing",
        HoverState::Expanded { locked: true, .. } => "expanded_locked",
        HoverState::Expanded { editing: true, .. } => "expanded_editing",
        HoverState::Expanded { .. } => "expanded",
    }
}

/// 排障入口：连续触发划词后调用它，对照计数即可定位断点。
#[tauri::command]
fn selection_debug_state(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> SelectionDebugView {
    let (anchor_enabled, keyboard_fallback) = {
        let settings = state.settings.lock().expect("settings poisoned");
        (
            settings.selection_anchor_enabled,
            settings.selection_keyboard_fallback,
        )
    };
    let hover_state = hover_code(state.hover.lock().expect("hover state poisoned").state());
    let pending = state
        .pending_selection
        .lock()
        .map(|pending| pending.is_some())
        .unwrap_or(false);
    let (anchor_window, anchor_visible) = selection_anchor::window_state(&app);
    let mut view = SelectionDebugView {
        watcher_running: false,
        anchor_ready: cfg!(windows) && state.anchor_ready.load(Ordering::SeqCst),
        anchor_window,
        anchor_visible,
        anchor_enabled,
        keyboard_fallback,
        pending,
        hover_state,
        counters: SelectionCountersView::default(),
        last_stage: "-".into(),
        last_reason: "-".into(),
        last_class: "-".into(),
        last_pid: 0,
    };
    #[cfg(windows)]
    {
        let diag = state.selection_diag.clone();
        view.watcher_running = state
            .selection_watch
            .lock()
            .ok()
            .and_then(|watcher| watcher.as_ref().map(|handle| handle.is_running()))
            .unwrap_or(false);
        view.counters = SelectionCountersView {
            gesture_candidates: diag.gesture_candidates.load(Ordering::Relaxed),
            uia_ok: diag.uia_ok.load(Ordering::Relaxed),
            uia_failed: diag.uia_failed.load(Ordering::Relaxed),
            gate_rejections: diag.gate_rejections.load(Ordering::Relaxed),
            anchor_shown: diag.anchor_shown.load(Ordering::Relaxed),
            anchor_failed: diag.anchor_failed.load(Ordering::Relaxed),
            click_total: diag.click_total.load(Ordering::Relaxed),
            click_consumed: diag.click_consumed.load(Ordering::Relaxed),
            click_missed: diag.click_missed.load(Ordering::Relaxed),
            click_blocked: diag.click_blocked.load(Ordering::Relaxed),
            fallback_attempts: diag.fallback_attempts.load(Ordering::Relaxed),
            fallback_ok: diag.fallback_ok.load(Ordering::Relaxed),
            fallback_failed: diag.fallback_failed.load(Ordering::Relaxed),
        };
        if let Some(last) = diag.last() {
            view.last_stage = last.stage.into();
            view.last_reason = last.reason.into();
            view.last_class = last.class;
            view.last_pid = last.pid;
        }
    }
    view
}

/// 窗口两种形态的尺寸。
///
/// 面板宽度随卡片数增长：卡片宽度固定 189px、间距 10px，加上左右 padding 与 1px inset，
/// 3 张卡时正好 613（与既有展开尺寸一致），2–5 张分别为 414 / 613 / 812 / 1011。
/// 卡片宽度不压缩，加卡只加宽面板。
const CAPSULE_WINDOW_WIDTH: f64 = 68.0;
const CAPSULE_WINDOW_HEIGHT: f64 = 28.0;
const BOARD_WINDOW_HEIGHT: f64 = 300.0;
const CARD_WIDTH_IN_PANEL: f64 = 189.0;
const CARD_GAP_IN_PANEL: f64 = 10.0;
const BOARD_CHROME_WIDTH: f64 = 26.0;
const BOARD_WINDOW_WIDTH: f64 = 613.0;
/// 胶囊左缘在展开面板中的横坐标，必须是整数：用居中的 (613-68)/2 = 272.5 会让胶囊
/// 在两种形态之间抖动半像素。窗口位置按 `x = 胶囊左缘 - 272` 换算，胶囊的屏幕矩形全程不动。
const CAPSULE_LEFT_IN_PANEL: f64 = 272.0;

fn main() {
    // 默认三语池演示「改一处、其余全部联动」；工单 03 起可在设置面板改为 2–5 种。
    let pool = LanguagePool::new(vec![
        Lang::new("zh"),
        Lang::new("en"),
        Lang::new("ja"),
    ])
    .expect("default pool uses exactly three languages");
    // 默认零配置引擎（MyMemory），开箱能翻；切换引擎在设置面板完成（工单 04）。
    let default_config = ProviderConfig::default();
    let mut chain = ProviderChain::new(SystemClock, ChainConfig::default());
    match build_translator(&default_config, Box::new(UreqTransport)) {
        Ok(translator) => chain.push(translator),
        Err(message) => panic!("默认引擎装配失败：{message}"),
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二次启动：唤起已有实例（显示窗口），不起新进程。
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(AppState {
            coordinator: Mutex::new(SyncCoordinator::new(
                pool,
                SystemClock,
                Box::new(HeuristicDetector),
            )),
            hover: Mutex::new(HoverStateMachine::new(
                settings::AppSettings::default().hover_delay_ms,
                settings::AppSettings::default().leave_grace_ms,
            )),

            chain: Mutex::new(chain),
            provider_config: Mutex::new(default_config),
            settings: Mutex::new(settings::AppSettings::default()),
            settings_dir: Mutex::new(None),
            history: Mutex::new(history::TranslationHistory::empty(history::DEFAULT_CAPACITY)),
            speech: SpeechService::new(SpeechManager::new(
                GoogleTtsOnline {
                    transport: bead_providers::UreqByteTransport,
                },
                std::env::temp_dir().join("bead-tts"),
            )),
            update_latest: Mutex::new(None),
            translation_epoch: AtomicU64::new(0),
            pending_selection: Mutex::new(None),
            anchor_epoch: AtomicU64::new(0),
            anchor_ready: AtomicBool::new(false),
            keyboard_fallback: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            hotkey: Mutex::new(None),
            hotkey_error: Mutex::new(None),
            #[cfg(windows)]
            selection_watch: Mutex::new(None),
            #[cfg(windows)]
            selection_diag: Arc::new(selection_watch::SelectionDiagnostics::default()),
            #[cfg(windows)]
            selection_unsupported_hint: Mutex::new(None),
            #[cfg(windows)]
            focus_return: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            hover_enter,
            hover_leave,
            tick,
            esc,
            outside_click,
            resize_window,
            pin,
            clear_document,
            copy_card,
            commit_edit,
            edit_start,
            edit_end,
            retry_failed,
            set_provider,
            set_settings,
            set_autostart,
            check_updates,
            list_history,
            apply_history,
            clear_history,
            open_release_page,
            test_provider,
            list_provider_models,
            speak_card,
            speak_text,
            stop_speaking,
            speech_state,
            speech_capabilities,
            speech_finished,
            speech_failed,
            start_drag,
            persist_position,
            add_card,
            remove_card,
            change_card_lang,
            reorder_cards,
            toggle_selection,
            anchor_click,
            #[cfg(windows)]
            selection_debug_state,
            sync_panel_width,
            panel_frame
        ])
        .setup(move |app| {
            // 初始就是胶囊大小：先剥掉 Windows 非客户区边框，再定尺寸与位置并显示，
            // 避免大窗口与系统边框在默认位置闪一下。
            let window = app.get_webview_window("main").expect("main window exists");
            strip_nc_frame(&window);
            window
                .set_size(LogicalSize::new(CAPSULE_WINDOW_WIDTH, CAPSULE_WINDOW_HEIGHT))
                .expect("resize main window");
            let default_x = window
                .current_monitor()
                .ok()
                .flatten()
                .map(|monitor| {
                    monitor.size().width as f64 / monitor.scale_factor() - CAPSULE_WINDOW_WIDTH
                })
                .unwrap_or(0.0);
            window
                .set_position(LogicalPosition::new(default_x, 0.0))
                .expect("position main window");
            window.show().expect("show main window");
            // 预热系统语音列表：首次点朗读不必等 PowerShell 枚举；后续按 TTL 刷新。
            let warm = app.handle().clone();
            std::thread::spawn(move || {
                let state = warm.state::<AppState>();
                let _ = state.speech.refresh_voices(false);
            });
            // 窗口首次显示后 Tauri/tao 会把边框样式写回去，所以显示后再剥一次，
            // 并用短延时线程补一次覆盖延迟发生的样式回写。
            // 真正让「外框 = 客户区」的是配置里的 shadow: false —— tao 只在
            // 「无边框 + 阴影」时才在 WM_NCCALCSIZE 保留 8px 非客户区，
            // 那 8px 不属于 WebView、会被窗口底色涂白，就是胶囊外面那圈白边。
            strip_nc_frame(&window);
            {
                let window_for_frame = window.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(900));
                    strip_nc_frame(&window_for_frame);
                });
            }

            // 拖动全过程都走这里：胶囊态夹 x 进可用区、贴显示器上缘，并实时回写
            // 锚点缓存；面板态只做纵向夹取。`start_dragging` 是 PostMessage 立即返回，
            // 拖动结束没有单独回调，所以实时回写是锚点唯一的刷新路径。
            let app_for_snap = app.handle().clone();
            let window_for_snap = window.clone();
            window.on_window_event(move |event| {
                let tauri::WindowEvent::Moved(pos) = event else {
                    return;
                };
                let scale = window_scale(&window_for_snap);
                let top = monitor_top(&window_for_snap);
                let x = pos.x as f64 / scale;
                let y = pos.y as f64 / scale;
                let width = window_for_snap
                    .inner_size()
                    .map(|size| size.width as f64 / scale)
                    .unwrap_or(CAPSULE_WINDOW_WIDTH);
                if width > CAPSULE_WINDOW_WIDTH + 1.0 {
                    // 面板态：只保证纵向落在当前显示器可用区。横向不能按「左缘 + 272」
                    // 反推锚点——贴边 clamp 后面板 left 不再等于锚点 - 272，反推会带偏胶囊。
                    let Some(area) = monitor_work_area(&window_for_snap) else {
                        return;
                    };
                    let target_top = clamp_panel_top(y, BOARD_WINDOW_HEIGHT, area);
                    if (target_top - y).abs() > 0.5 {
                        let _ = window_for_snap.set_position(LogicalPosition::new(x, target_top));
                    }
                    return;
                }
                // 胶囊态：夹回可用区（跨屏用并集，允许拖到副屏）并贴显示器上缘；
                // 锚点跟随真实位置。
                let Some(span) = virtual_work_span(&window_for_snap) else {
                    return;
                };
                let target_left = clamp_capsule_left(x, CAPSULE_WINDOW_WIDTH, span);
                if (target_left - x).abs() > 0.5 || (y - top).abs() > 0.5 {
                    let _ = window_for_snap.set_position(LogicalPosition::new(target_left, top));
                }
                app_for_snap
                    .state::<AppState>()
                    .settings
                    .lock()
                    .expect("settings poisoned")
                    .capsule_anchor = Some((target_left.round() as i32, top.round() as i32));
            });

            // 加载用户设置并应用：语言池、悬停延迟、引擎链（工单 05 持久化）。
            let state = app.state::<AppState>();
            let dir = app.path().app_config_dir().ok();
            if let Some(dir) = &dir {
                let loaded = settings::load(dir);
                let saved_anchor = loaded.capsule_anchor;
                if let Ok(pool) = LanguagePool::new(
                    loaded
                        .languages
                        .iter()
                        .map(|code| Lang::new(code.as_str()))
                        .collect(),
                ) {
                    state
                        .coordinator
                        .lock()
                        .expect("coordinator poisoned")
                        .replace_pool(pool);
                    // 面板宽度跟随语言池计数，但这里不动窗口：展开时 resize_window
                    // 会按当前计数提交，收起态改动等下次展开一起生效。
                    // 现在直接改窗口会把用户拖过的锚点与当前形态冲掉。
                }
                *state.hover.lock().expect("hover state poisoned") =
                    HoverStateMachine::new(loaded.hover_delay_ms, loaded.leave_grace_ms);
                *state.provider_config.lock().expect("provider config poisoned") =
                    loaded.provider.clone();
                rebuild_chain(&loaded.provider, state.inner());
                *state.settings.lock().expect("settings poisoned") = loaded;
                *state.settings_dir.lock().expect("settings dir poisoned") = Some(dir.clone());
                // 历史与 settings.json 同级：读不出或不认识格式都退化成空历史，不影响启动。
                *state.history.lock().expect("history poisoned") =
                    history::TranslationHistory::load(dir, history::DEFAULT_CAPACITY);
                // 位置记忆：恢复拖拽后的横向锚点；纵坐标一律吸附显示器上边缘。
                if let Some((x, _y)) = saved_anchor {
                    let top = monitor_top(&window);
                    let _ = window.set_position(LogicalPosition::new(x as f64, top));
                    let on_screen = window.outer_position().ok().is_some_and(|outer| {
                        window
                            .available_monitors()
                            .unwrap_or_default()
                            .iter()
                            .any(|monitor| {
                                let origin = monitor.position();
                                let size = monitor.size();
                                outer.x >= origin.x
                                    && outer.y >= origin.y
                                    && outer.x + CAPSULE_WINDOW_WIDTH as i32
                                        <= origin.x + size.width as i32
                                    && outer.y + CAPSULE_WINDOW_HEIGHT as i32
                                        <= origin.y + size.height as i32
                            })
                    });
                    if !on_screen {
                        let _ = window
                            .set_position(LogicalPosition::new(default_x, monitor_top(&window)));
                    }
                }
                // 自启动：已启用则重写当前路径（便携包移动后自愈）。
                if state.settings.lock().expect("settings poisoned").autostart_enabled {
                    let autostart = app.autolaunch();
                    let _ = autostart.disable();
                    match autostart.enable() {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
            // 记下胶囊左缘作为窗口伸缩的固定锚点：展开/收起只改尺寸，位置全部由锚点算出，
            // 不再从当前几何反推（异步 resize 的中间态几何会把锚点算歪）。
            if let Some(anchor) = capsule_anchor(&window) {
                state.settings.lock().expect("settings poisoned").capsule_anchor = Some(anchor);
            }
            // 划词热键：按用户配置（默认 Ctrl+Alt+Shift+B）注册；失败原因进设置页提示。
            let (keyboard_fallback_on, hotkey_spec) = {
                let settings = state.settings.lock().expect("settings poisoned");
                (
                    settings.selection_keyboard_fallback,
                    settings.selection_hotkey.clone(),
                )
            };
            state
                .keyboard_fallback
                .store(cfg!(windows) && keyboard_fallback_on, Ordering::SeqCst);
            #[cfg(windows)]
            apply_hotkey(app.handle(), &hotkey_spec);
            // 选区锚点图标：先建隐藏窗口（建不出来就让设置项如实置灰），
            // 再按已保存的开关启动 Raw Input 监听；关闭时不注册任何全局监听。
            #[cfg(windows)]
            {
                let ready = selection_anchor::create(app.handle()).is_some();
                state.anchor_ready.store(ready, Ordering::SeqCst);
                if !ready {
                    // 图标窗口建不出来（WebView2 或窗口系统异常）就不能留下「已开启」的假状态。
                    state
                        .settings
                        .lock()
                        .expect("settings poisoned")
                        .selection_anchor_enabled = false;
                }
            }
            let anchor_on = state
                .settings
                .lock()
                .expect("settings poisoned")
                .selection_anchor_enabled;
            apply_anchor_watcher(app.handle(), anchor_on);
            // 启动时异步检查更新（auto_check_updates 开关控制），绝不阻塞启动。
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let state = handle.state::<AppState>();
                    run_update_check(state.inner());
                });
            }
            // 托盘：显隐胶囊 / 退出（工单 08）。
            build_tray(app.handle())?;
            // Wayland 能力提示：定位/置顶受限是协议限制（立项调研已记录），明确告知不静默。
            #[cfg(target_os = "linux")]
            {
                if std::env::var("WAYLAND_DISPLAY").is_ok() {
}
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Bead")
        .run(|app_handle, event| {
            // 位置记忆：锚点由 Moved 监听维护，退出只需要落盘。
            // 胶囊态再从窗口几何补一次兜底；展开态禁止反推——贴边 clamp 后
            // 面板 left ≠ 锚点 - 272，反推会把记忆位置写歪。
            if matches!(event, tauri::RunEvent::Exit) {
                let state = app_handle.state::<AppState>();
                if let Some(window) = app_handle.get_webview_window("main") {
                    if !window_is_oversized(&window) {
                        if let Some(anchor) = capsule_anchor(&window) {
                            let mut settings =
                                state.settings.lock().expect("settings poisoned");
                            settings.capsule_anchor = Some(anchor);
                            settings.window_position = Some(anchor); // 旧字段保持同步
                        }
                    }
                }
                persist(state.inner());
            }
        });
}

/// 托盘菜单：显隐胶囊、退出。
fn build_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏胶囊", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &quit])?;

    let mut tray = TrayIconBuilder::with_id("bead-tray")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip("Bead · 悬停即译");
    if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png")) {
        tray = tray.icon(icon);
    }
    tray.on_menu_event(|app, event| {
        let window = app.get_webview_window("main");
        match event.id().as_ref() {
            "toggle" => {
                if let Some(window) = window {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
            "quit" => app.exit(0),
            _ => {}
        }
    })
    .build(app)?;
    Ok(())
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    fn primary_1080p() -> WorkArea {
        WorkArea {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1040.0,
        }
    }

    fn primary_span() -> (f64, f64) {
        (0.0, 1920.0)
    }

    #[test]
    fn a_capsule_dragged_past_the_left_edge_stops_fully_visible() {
        assert_eq!(clamp_capsule_left(-120.0, CAPSULE_WINDOW_WIDTH, primary_span()), 0.0);
        assert_eq!(clamp_capsule_left(-1.0, CAPSULE_WINDOW_WIDTH, primary_span()), 0.0);
    }

    #[test]
    fn a_capsule_dragged_past_the_right_edge_stops_fully_visible() {
        assert_eq!(
            clamp_capsule_left(1930.0, CAPSULE_WINDOW_WIDTH, primary_span()),
            1920.0 - CAPSULE_WINDOW_WIDTH
        );
    }

    #[test]
    fn a_capsule_inside_the_work_area_is_not_nudged() {
        assert_eq!(clamp_capsule_left(1000.0, CAPSULE_WINDOW_WIDTH, primary_span()), 1000.0);
    }

    #[test]
    fn a_capsule_wider_than_the_work_area_pins_to_its_left_edge() {
        assert_eq!(clamp_capsule_left(25.0, CAPSULE_WINDOW_WIDTH, (0.0, 40.0)), 0.0);
    }

    #[test]
    fn a_capsule_near_the_left_edge_opens_a_panel_fully_inside_the_work_area() {
        let (left, top) = place_panel((40.0, 0.0), 613.0, 300.0, primary_1080p());
        assert_eq!(left, 0.0);
        assert!(left + 613.0 <= primary_1080p().right);
        assert_eq!(top, 0.0);
    }

    #[test]
    fn a_capsule_near_the_right_edge_opens_a_panel_fully_inside_the_work_area() {
        let (left, _) = place_panel((1900.0, 0.0), 613.0, 300.0, primary_1080p());
        assert_eq!(left, 1920.0 - 613.0);
        assert!(left + 613.0 <= primary_1080p().right);
    }

    #[test]
    fn a_panel_that_fits_keeps_the_capsule_slot_centered() {
        let (left, top) = place_panel((1000.0, 0.0), 613.0, 300.0, primary_1080p());
        assert_eq!(left, 1000.0 - CAPSULE_LEFT_IN_PANEL);
        assert_eq!(top, 0.0);
    }

    #[test]
    fn a_panel_wider_than_the_work_area_pins_to_its_left_edge() {
        let small = WorkArea {
            left: 0.0,
            top: 0.0,
            right: 800.0,
            bottom: 600.0,
        };
        let (left, _) = place_panel((500.0, 0.0), 1011.0, 300.0, small);
        assert_eq!(left, 0.0);
    }

    #[test]
    fn a_panel_frame_for_three_cards_matches_the_geometry_the_expand_commits() {
        let frame = panel_frame_at((1000.0, 0.0), board_width(3), Some(primary_1080p()));
        assert_eq!(frame.left, 1000.0 - CAPSULE_LEFT_IN_PANEL);
        assert_eq!(frame.width, 613.0);
        assert_eq!(frame.top, 0.0);
        assert_eq!(frame.height, BOARD_WINDOW_HEIGHT);
    }

    #[test]
    fn adding_a_fourth_card_keeps_the_right_edge_when_the_panel_is_docked_right() {
        let before = panel_frame_at((1900.0, 0.0), board_width(3), Some(primary_1080p()));
        let after = panel_frame_at((1900.0, 0.0), board_width(4), Some(primary_1080p()));
        assert_eq!(before.left + before.width, 1920.0);
        assert_eq!(after.left + after.width, 1920.0);
        assert!(
            after.left < before.left,
            "a wider panel docked right must grow leftwards"
        );
    }

    #[test]
    fn panel_frames_follow_the_fixed_card_pitch_from_two_to_five_cards() {
        let widths: Vec<f64> = (2..=5)
            .map(|count| panel_frame_at((500.0, 0.0), board_width(count), Some(primary_1080p())).width)
            .collect();
        assert_eq!(widths, vec![414.0, 613.0, 812.0, 1011.0]);
    }

    #[test]
    fn a_top_docked_taskbar_pushes_the_panel_below_it() {
        let with_taskbar = WorkArea {
            top: 48.0,
            ..primary_1080p()
        };
        let (_, top) = place_panel((1000.0, 0.0), 613.0, 300.0, with_taskbar);
        assert_eq!(top, 48.0);
    }

    #[test]
    fn a_short_work_area_pins_the_panel_to_its_top() {
        let short = WorkArea {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 240.0,
        };
        assert_eq!(clamp_panel_top(0.0, BOARD_WINDOW_HEIGHT, short), 0.0);
    }

    #[test]
    fn a_capsule_can_cross_into_the_second_monitor_and_stops_at_its_right_edge() {
        // 双屏并排：可用区并集 [0, 3840]，1920 处不夹取（能拖过去），只在最右缘停住。
        let span = union_span(&[primary_1080p(), WorkArea {
            left: 1920.0,
            top: 0.0,
            right: 3840.0,
            bottom: 1040.0,
        }])
        .expect("two monitors");
        assert_eq!(span, (0.0, 3840.0));
        assert_eq!(clamp_capsule_left(1900.0, CAPSULE_WINDOW_WIDTH, span), 1900.0);
        assert_eq!(
            clamp_capsule_left(3900.0, CAPSULE_WINDOW_WIDTH, span),
            3840.0 - CAPSULE_WINDOW_WIDTH
        );
    }

    #[test]
    fn the_drag_span_unions_monitors_on_both_sides() {
        let left_monitor = WorkArea {
            left: -1920.0,
            top: 0.0,
            right: 0.0,
            bottom: 1040.0,
        };
        assert_eq!(union_span(&[left_monitor, primary_1080p()]), Some((-1920.0, 1920.0)));
        assert_eq!(clamp_capsule_left(-1900.0, CAPSULE_WINDOW_WIDTH, (-1920.0, 1920.0)), -1900.0);
        assert_eq!(clamp_capsule_left(-1990.0, CAPSULE_WINDOW_WIDTH, (-1920.0, 1920.0)), -1920.0);
    }

    #[test]
    fn a_panel_stays_on_the_monitor_under_the_capsule() {
        // 副屏在右侧：面板不会横跨两块屏，贴副屏右缘收进 [1920, 3840] 内。
        let secondary = WorkArea {
            left: 1920.0,
            top: 0.0,
            right: 3840.0,
            bottom: 1040.0,
        };
        let (left, _) = place_panel((3760.0, 0.0), 613.0, 300.0, secondary);
        assert_eq!(left, 3840.0 - 613.0);
        assert!(left >= secondary.left);
    }

    #[test]
    fn a_scaled_work_area_is_compared_in_logical_pixels() {
        // 2560×1440 物理、底部任务栏 60px、150% 缩放 → 逻辑可用区 1706.67×920。
        let area = WorkArea::from_physical(
            PhysicalArea {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1380,
            },
            1.5,
        );
        assert!((area.right - 1706.6667).abs() < 0.01);
        assert!((area.bottom - 920.0).abs() < 0.01);
        let span = union_span(&[area]).expect("single monitor");
        let clamped = clamp_capsule_left(5000.0, CAPSULE_WINDOW_WIDTH, span);
        assert!((clamped - (1706.6667 - CAPSULE_WINDOW_WIDTH)).abs() < 0.01);
    }
}

#[cfg(test)]
mod chain_epoch_tests {
    use super::*;

    #[test]
    fn a_batch_that_finds_a_newer_epoch_under_the_lock_gives_up_without_calling_the_network() {
        assert!(locked_work_is_stale(2, 3));
    }

    #[test]
    fn the_latest_batch_proceeds_even_if_it_waited_on_the_chain_lock() {
        assert!(!locked_work_is_stale(3, 3));
        assert!(!locked_work_is_stale(0, 0));
    }
}

#[cfg(all(test, windows))]
mod focus_return_tests {
    use super::*;

    #[test]
    fn a_focus_return_target_is_only_remembered_when_another_window_owns_the_foreground() {
        assert_eq!(focus_return_target(0x11, 0x22), Some(0x11));
        assert_eq!(focus_return_target(0, 0x22), None);
        assert_eq!(focus_return_target(0x22, 0x22), None);
    }
}
