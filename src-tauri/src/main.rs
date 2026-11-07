#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri 壳层：持有全部系统能力与业务状态，WebView 只发 intent、只收 state 快照。
//!
//! 剪贴板读取只出现在 `consume` 的 `ReadClipboardOnce` 分支里，且每个 intent 最多读一次；
//! 这里没有监听器、没有轮询、没有后台刷新。默认被动因此是结构性的，不靠设置开关去维持。
//! 翻译经 bead-core 的 ProviderChain（回退/重试/熔断）落到 bead-providers 的真实引擎；
//! key 只存在 AppState 内存与用户配置目录里，不写日志、不明文回显。
//! 设置（自动读剪贴板、悬停延迟、引擎、语言池）持久化到系统配置目录（工单 05）。

mod settings;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bead_core::{
    check_for_update, ChainConfig, Clock, HeuristicDetector, HoverAction, HoverEvent, HoverState,
    HoverStateMachine, Lang, LanguagePool, OfflineSpeaker, OnlineTts, ProviderChain, SensitiveFilter, SensitiveVerdict, SlotStatus, SpeechError, SpeechManager,
    SpeechOutcome, SyncCoordinator, SyncPlan, UpdateStatus,
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
        pub fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
        pub fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
    }

    pub const WM_HOTKEY: u32 = 0x0312;
    pub const MOD_CONTROL: u32 = 0x0002;
    pub const MOD_ALT: u32 = 0x0001;
    pub const VK_B: u8 = 0x42;
    pub const KEYEVENTF_KEYUP: u32 = 0x0002;
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

#[cfg(windows)]
fn spawn_hotkey_thread(app: tauri::AppHandle) {
    std::thread::spawn(move || unsafe {
        // 快捷键目前固定 Ctrl+Alt+B（settings.selection_hotkey 为可配置位，改键重启生效并如实标注）。
        if win::RegisterHotKey(0, 1, win::MOD_CONTROL | win::MOD_ALT, win::VK_B as u32) == 0 {
            let _state = app.state::<AppState>();
}
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
            if msg.message == win::WM_HOTKEY && msg.wparam == 1 {
                let state = app.state::<AppState>();
                if !state.settings.lock().expect("settings poisoned").selection_enabled {
                    continue;
                }
                capture_and_translate(&app);
            }
        }
    });
}

/// 取选中文本并送入：保存剪贴板 → 模拟 Ctrl+C → 读 → 恢复 → 过滤 → 翻译。
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
    // 窗口伸缩由前端驱动：先把 expanded 快照推过去，前端在内容全透明的那一帧再自己 resize。
    // 这里若抢先 resize，WebView 上一帧的白底面板会被留在胶囊大小的窗口里。
    let view = snapshot(state.inner());
    let _ = app.emit("state", view);
    let plan = {
        let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator.accept_clipboard(&text, false)
    };
    if let Some(plan) = plan {
        spawn_translation(app, plan);
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

/// 离线语音：Windows SAPI（System.Speech），零第三方依赖。子进程自己发声，不阻塞壳层。
/// 初版不按语言挑音色（Windows 已装语音有限），按文本内容走默认语音；诚实标注为粗粒度。
struct SapiSpeaker;

impl OfflineSpeaker for SapiSpeaker {
    fn speak(&self, _language: &Lang, text: &str) -> Result<(), SpeechError> {
        let script = format!(
            "(New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('{}')",
            text.replace('\'', "''")
        );
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .spawn()
            .map(|_| ())
            .map_err(|error| SpeechError::OfflineFailed(format!("SAPI 启动失败：{error}")))
    }
}

/// 在线 TTS：Google translate_tts（无 key；本网络不可达时即走离线降级路径）。
struct GoogleTtsOnline {
    transport: bead_providers::UreqByteTransport,
}

impl OnlineTts for GoogleTtsOnline {
    fn fetch(&self, language: &Lang, text: &str) -> Result<Vec<u8>, SpeechError> {
        bead_providers::GoogleTts::fetch(&self.transport, language, text)
    }
}

struct AppState {
    coordinator: Mutex<SyncCoordinator<SystemClock>>,
    hover: Mutex<HoverStateMachine>,

    chain: Mutex<ProviderChain<SystemClock>>,
    provider_config: Mutex<ProviderConfig>,
    settings: Mutex<settings::AppSettings>,
    settings_dir: Mutex<Option<PathBuf>>,
    speech: Mutex<SpeechManager<GoogleTtsOnline, SapiSpeaker>>,
    update_latest: Mutex<Option<String>>,
    /// 翻译任务代次：只有最新一代可以推送 state，防止慢请求回来覆盖新内容。
    translation_epoch: AtomicU64,
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

/// 朗读结果视图：file = 前端用 convertFileSrc 播放；played = SAPI 已自播。
#[derive(Serialize, Clone)]
struct SpeakView {
    kind: String,
    url: String,
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
    autostart_enabled: bool,
    auto_check_updates: bool,
}

#[derive(Serialize, Clone)]
struct StateView {
    view: String,
    locked: bool,
    cards: Vec<CardView>,

    provider: ProviderView,
    settings: SettingsView,
    speak: Option<SpeakView>,
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
    let coordinator = state.coordinator.lock().expect("coordinator poisoned");
    let hover_state = hover.state();
    let (view, locked) = match hover_state {
        HoverState::Idle => ("capsule", false),
        HoverState::HoverPending { .. } => ("hover_pending", false),
        HoverState::Expanded { locked, .. } => ("expanded", locked),
        HoverState::Collapsing { .. } => ("collapsing", false),
    };
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
            autostart_enabled: settings.autostart_enabled,
            auto_check_updates: settings.auto_check_updates,
        },
        speak: None,
        update_latest: state.update_latest.lock().expect("update poisoned").clone(),
    }
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

/// 朗读一张卡：在线抓取→缓存→前端播放，失败自动降级 SAPI 自播，再失败出轻提示。
///
/// 在线 TTS 在不可达网络里要等超时、SAPI 是外部进程，两者都不能占着 IPC 线程：
/// 放进 blocking 线程后命令立即让出运行时，前端在点下「朗读」后仍可继续操作。
#[tauri::command]
async fn speak_card(lang: String, app: tauri::AppHandle) -> StateView {
    let language = Lang::new(&lang);
    let text = {
        let state = app.state::<AppState>();
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator
            .document()
            .slot(&language)
            .map(|slot| slot.text.clone())
            .unwrap_or_default()
    };
    if text.trim().is_empty() {
        let state = app.state::<AppState>();
return snapshot(state.inner());
    }

    let handle = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let outcome = state
            .speech
            .lock()
            .expect("speech poisoned")
            .request(&language, &text);
        outcome
    })
    .await;

    let state = app.state::<AppState>();
    let mut speak = None;
    match outcome {
        Ok(Ok(SpeechOutcome::PlayFile(path))) => {
            speak = Some(SpeakView {
                kind: "file".into(),
                url: path.to_string_lossy().to_string(),
            });
        }
        Ok(Ok(SpeechOutcome::Played)) => {
            speak = Some(SpeakView {
                kind: "played".into(),
                url: String::new(),
            });
        }
        _ => {
}
    }
    let mut view = snapshot(state.inner());
    view.speak = speak;
    view
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
fn spawn_translation(app: &tauri::AppHandle, plan: SyncPlan) {
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
        let latest = state.translation_epoch.load(Ordering::SeqCst);
        {
            let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
            match result {
                Ok(result) => {
                    // apply 内部按 generation 丢弃过期结果，这里无需二次判断。
                    let _ = coordinator.apply(result);
                    if epoch == latest {
}
                }
                Err(_) if epoch == latest => {
}
                Err(_) => {}
            }
        }
        if epoch == state.translation_epoch.load(Ordering::SeqCst) {
            let view = snapshot(state.inner());
            let _ = handle.emit("state", view);
        }
    });
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
                            let plan = state
                                .coordinator
                                .lock()
                                .expect("coordinator poisoned")
                                .accept_clipboard(&text, force_refresh);
                            if let Some(plan) = plan {
                                // 翻译链含 10s 超时 + 重试，绝不能让展开/刷新命令等它跑完。
                                spawn_translation(app, plan);
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
fn clear_document(state: tauri::State<'_, AppState>) -> StateView {
    state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .clear();
    state.translation_epoch.fetch_add(1, Ordering::SeqCst);
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
    let plan = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .commit(&Lang::new(&lang), text);
    match plan {
        Ok(plan) => spawn_translation(&app, plan),
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
#[tauri::command]
fn retry_failed(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let plan = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .retry_failed();
    if let Some(plan) = plan {
        spawn_translation(&app, plan);
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
    state: tauri::State<'_, AppState>,
) -> StateView {
    let mut config = state.provider_config.lock().expect("provider config poisoned");
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
    drop(config);
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

/// 保存面板设置（自动读剪贴板、悬停延迟、划词、更新开关）并即时生效。
/// 延迟生效通过重建状态机完成，展开态会复位——该动作本身就来自展开的面板，代价可接受。
#[tauri::command(rename_all = "snake_case")]
fn set_settings(
    auto_read_clipboard: bool,
    hover_delay_ms: u64,
    selection_enabled: bool,
    auto_check_updates: bool,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let delay = hover_delay_ms.clamp(50, 2000);
    {
        let mut settings = state.settings.lock().expect("settings poisoned");
        settings.auto_read_clipboard = auto_read_clipboard;
        settings.hover_delay_ms = delay;
        settings.selection_enabled = selection_enabled;
        settings.auto_check_updates = auto_check_updates;
        let grace = settings.leave_grace_ms;
        *state.hover.lock().expect("hover state poisoned") =
            HoverStateMachine::new(delay, grace);
    }
    persist(state.inner());
    snapshot(state.inner())
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
    let pool = LanguagePool::new(languages.iter().map(|c| Lang::new(c.as_str())).collect());
    match pool {
        Ok(pool) => {
            {
                let mut settings = state.settings.lock().expect("settings poisoned");
                settings.languages = pool.languages().iter().map(|l| l.code().to_string()).collect();
            }
            state
                .coordinator
                .lock()
                .expect("coordinator poisoned")
                .replace_pool(pool);
            persist(state);
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
            speech: Mutex::new(SpeechManager::new(
                GoogleTtsOnline {
                    transport: bead_providers::UreqByteTransport,
                },
                SapiSpeaker,
                std::env::temp_dir().join("bead-tts"),
            )),
            update_latest: Mutex::new(None),
            translation_epoch: AtomicU64::new(0),
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
            open_release_page,
            test_provider,
            list_provider_models,
            speak_card,
            start_drag,
            persist_position,
            add_card,
            remove_card,
            change_card_lang,
            reorder_cards,
            toggle_selection,
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
            // 划词热键线程：注册失败静默降级；开关关闭时热键仍在但不动作（默认关闭，规范要求）。
            spawn_hotkey_thread(app.handle().clone());
            // 启动时异步检查更新（auto_check_updates 开关控制），绝不阻塞启动。
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let state = handle.state::<AppState>();
                    run_update_check(state.inner());
                });
            }
            // 托盘：设置 / 显隐胶囊 / 退出（工单 08）。
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

/// 托盘菜单：设置（显示并展开面板开关）、显隐胶囊、退出。
fn build_tray(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏胶囊", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &settings_item, &quit])?;

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
            "settings" => {
                if let Some(window) = &window {
                    let _ = window.show();
                    let _ = window.set_focus();
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
