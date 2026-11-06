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
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bead_core::{
    check_for_update, ChainConfig, Clock, HeuristicDetector, HoverAction, HoverEvent, HoverState,
    HoverStateMachine, Lang, LanguagePool, OfflineSpeaker, OnlineTts, ProviderChain, SensitiveFilter, SensitiveVerdict, SlotStatus, SpeechError, SpeechManager,
    SpeechOutcome, SyncCoordinator, SyncPlan, UpdateStatus,
};
use bead_providers::{build_translator, list_models, Engine, GitHubReleaseFetcher, ProviderConfig, UreqTransport};
use serde::Serialize;
use tauri::{LogicalPosition, LogicalSize, Manager};
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
            let state = app.state::<AppState>();
            *state.notice.lock().expect("notice poisoned") =
                Some("划词快捷键 Ctrl+Alt+B 注册失败：可能被其他程序占用".to_string());
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
            *state.notice.lock().expect("notice poisoned") =
                Some("取词失败：前台窗口没有可复制的选中文本".to_string());
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
            *state.notice.lock().expect("notice poisoned") =
                Some("面板已锁定或正在编辑：划词结果未送入".to_string());
            return;
        }
        _ => {}
    }
    if let SensitiveVerdict::Skipped(reason) = SensitiveFilter::classify(&text) {
        *state.notice.lock().expect("notice poisoned") =
            Some(format!("已跳过敏感内容（{reason}）"));
        return;
    }
    // 展开并送入（SelectionCaptured 展开无读取 intent，文本走本路径）
    state
        .hover
        .lock()
        .expect("hover state poisoned")
        .transition(HoverEvent::SelectionCaptured);
    apply_window_shape(app, state.inner(), "expanded");
    let plan = {
        let mut coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator.accept_clipboard(&text, false)
    };
    if let Some(plan) = plan {
        match run_chain(state.inner(), &plan) {
            Ok(result) => {
                let _ = state
                    .coordinator
                    .lock()
                    .expect("coordinator poisoned")
                    .apply(result);
                *state.notice.lock().expect("notice poisoned") = None;
            }
            Err(_) => {
                *state.notice.lock().expect("notice poisoned") =
                    Some("翻译失败：全部引擎都不可用".to_string());
            }
        }
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
    notice: Mutex<Option<String>>,
    chain: Mutex<ProviderChain<SystemClock>>,
    provider_config: Mutex<ProviderConfig>,
    settings: Mutex<settings::AppSettings>,
    settings_dir: Mutex<Option<PathBuf>>,
    speech: Mutex<SpeechManager<GoogleTtsOnline, SapiSpeaker>>,
    update_latest: Mutex<Option<String>>,
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
    notice: Option<String>,
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
    let notice = state.notice.lock().expect("notice poisoned");

    let hover_state = hover.state();
    let (view, locked) = match hover_state {
        HoverState::Idle => ("capsule", false),
        HoverState::HoverPending { .. } => ("hover_pending", false),
        HoverState::Expanded { locked, .. } => ("expanded", locked),
        HoverState::Collapsing { .. } => ("collapsing", false),
    };
    let authoritative = coordinator.document().authoritative();
    let cards = coordinator
        .document()
        .slots()
        .map(|(language, slot)| CardView {
            lang: language.code().to_string(),
            text: slot.text.clone(),
            status: status_name(slot.status).to_string(),
            authoritative: Some(language) == authoritative,
        })
        .collect();

    let provider_config = state.provider_config.lock().expect("provider config poisoned");
    let settings = state.settings.lock().expect("settings poisoned");
    StateView {
        view: view.to_string(),
        locked,
        cards,
        notice: notice.clone(),
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
    if let UpdateStatus::UpdateAvailable(latest) = status {
        *state.update_latest.lock().expect("update poisoned") = Some(latest.clone());
        *state.notice.lock().expect("notice poisoned") =
            Some(format!("发现新版本 {latest}，可在设置面板前往下载"));
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
#[tauri::command]
fn speak_card(lang: String, state: tauri::State<'_, AppState>) -> StateView {
    let text = {
        let coordinator = state.coordinator.lock().expect("coordinator poisoned");
        coordinator
            .document()
            .slot(&Lang::new(&lang))
            .map(|slot| slot.text.clone())
            .unwrap_or_default()
    };
    let mut view = snapshot(state.inner());
    if text.trim().is_empty() {
        *state.notice.lock().expect("notice poisoned") = Some("这张卡没有内容可朗读".to_string());
        view = snapshot(state.inner());
        return view;
    }
    match state.speech.lock().expect("speech poisoned").request(&Lang::new(&lang), &text) {
        Ok(SpeechOutcome::PlayFile(path)) => {
            view.speak = Some(SpeakView {
                kind: "file".into(),
                url: path.to_string_lossy().to_string(),
            });
        }
        Ok(SpeechOutcome::Played) => {
            view.speak = Some(SpeakView {
                kind: "played".into(),
                url: String::new(),
            });
        }
        Err(_) => {
            *state.notice.lock().expect("notice poisoned") =
                Some("朗读失败：在线与离线语音都不可用".to_string());
        }
    }
    view
}

/// 窗口形态跟随状态：胶囊态只留顶部一条（其余桌面区域不被透明窗口挡住点击），
/// 展开态恢复整板。位置始终贴屏幕顶缘右缘。
fn apply_window_shape(app: &tauri::AppHandle, state: &AppState, view_kind: &str) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let expanded = view_kind == "expanded";
    let height = if expanded { 300.0 } else { CAPSULE_WINDOW_HEIGHT };
    let width = board_width(state.settings.lock().expect("settings poisoned").languages.len());
    if let Ok(Some(monitor)) = window.current_monitor() {
        let screen_width = monitor.size().width as f64 / monitor.scale_factor();
        let _ = window.set_size(LogicalSize::new(width, height));
        let _ = window.set_position(LogicalPosition::new(screen_width - width, 0.0));
    }
}

/// 翻译统一走链：回退/重试/熔断都在这里，调用方只看成功或「全部引擎失败」。
fn run_chain(state: &AppState, plan: &SyncPlan) -> Result<bead_core::TranslationSet, bead_core::ProviderError> {
    state
        .chain
        .lock()
        .expect("provider chain poisoned")
        .translate(plan)
}

/// 只有状态机产出的 intent 才能触碰系统能力。force_refresh 对应界面上的显式「读取剪贴板」。
fn consume(actions: Vec<HoverAction>, app: &tauri::AppHandle, state: &AppState, force_refresh: bool) {
    for action in actions {
        match action {
            HoverAction::ReadClipboardOnce => {
                // 关闭自动读取后：除手动粘贴/输入外无任何读取（重读按钮也拦下，规范从严）。
                if !state.settings.lock().expect("settings poisoned").auto_read_clipboard {
                    *state.notice.lock().expect("notice poisoned") =
                        Some("自动读剪贴板已关闭：可直接在卡片中粘贴或输入".to_string());
                    continue;
                }
                match app.clipboard().read_text() {
                    Ok(text) => match SensitiveFilter::classify(&text) {
                        SensitiveVerdict::Skipped(reason) => {
                            // 敏感内容连 provider 链都不进：过滤发生在读取之后、提交之前。
                            *state.notice.lock().expect("notice poisoned") =
                                Some(format!("已跳过敏感内容（{reason}），可在卡片中手动输入需要翻译的部分"));
                        }
                        SensitiveVerdict::Allowed => {
                            let plan = state
                                .coordinator
                                .lock()
                                .expect("coordinator poisoned")
                                .accept_clipboard(&text, force_refresh);
                            if let Some(plan) = plan {
                                match run_chain(state, &plan) {
                                    Ok(result) => {
                                        let _ = state
                                            .coordinator
                                            .lock()
                                            .expect("coordinator poisoned")
                                            .apply(result);
                                        *state.notice.lock().expect("notice poisoned") = None;
                                    }
                                    Err(_) => {
                                        *state.notice.lock().expect("notice poisoned") =
                                            Some("翻译失败：全部引擎都不可用，稍后重试或检查引擎设置".to_string());
                                    }
                                }
                            } else {
                                *state.notice.lock().expect("notice poisoned") = None;
                            }
                        }
                    },
                    Err(_) => {
                        *state.notice.lock().expect("notice poisoned") =
                            Some("剪贴板为空或非文本".to_string());
                    }
                }
            }
            HoverAction::ClearDocument => {
                state
                    .coordinator
                    .lock()
                    .expect("coordinator poisoned")
                    .clear();
                *state.notice.lock().expect("notice poisoned") = None;
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
    apply_window_shape(app, state, &view.view);
    view
}

#[tauri::command]
fn get_state(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    let view = snapshot(state.inner());
    apply_window_shape(&app, state.inner(), &view.view);
    view
}

#[tauri::command]
fn hover_enter(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
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
fn hover_leave(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
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

#[tauri::command]
fn refresh(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(HoverEvent::Refresh, &app, state.inner(), true)
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
            *state.notice.lock().expect("notice poisoned") =
                Some("复制失败：剪贴板不可用".to_string());
        }
    }
    snapshot(state.inner())
}

/// 编辑提交：该卡成为权威源，链式翻译立刻填充其余卡。同步命令下网络耗时体现在
/// 这一次调用的返回上，WebView 不冻结（命令在独立线程执行），渲染保留策略保证
/// 等待期间还能继续打字。
#[tauri::command]
fn commit_edit(lang: String, text: String, state: tauri::State<'_, AppState>) -> StateView {
    let plan = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .commit(&Lang::new(&lang), text);
    match plan {
        Ok(plan) => match run_chain(state.inner(), &plan) {
            Ok(result) => {
                let _ = state
                    .coordinator
                    .lock()
                    .expect("coordinator poisoned")
                    .apply(result);
                *state.notice.lock().expect("notice poisoned") = None;
            }
            Err(_) => {
                *state.notice.lock().expect("notice poisoned") =
                    Some("翻译失败：全部引擎都不可用，稍后重试或检查引擎设置".to_string());
            }
        },
        Err(error) => {
            *state.notice.lock().expect("notice poisoned") =
                Some(format!("提交失败：{error:?}"));
        }
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
fn retry_failed(state: tauri::State<'_, AppState>) -> StateView {
    let plan = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .retry_failed();
    if let Some(plan) = plan {
        match run_chain(state.inner(), &plan) {
            Ok(result) => {
                let _ = state
                    .coordinator
                    .lock()
                    .expect("coordinator poisoned")
                    .apply(result);
                *state.notice.lock().expect("notice poisoned") = None;
            }
            Err(_) => {
                *state.notice.lock().expect("notice poisoned") =
                    Some("重试失败：全部引擎都不可用".to_string());
            }
        }
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
            *state.notice.lock().expect("notice poisoned") =
                Some(format!("已切换到 {} 引擎", engine.code()));
        }
        None => {
            *state.notice.lock().expect("notice poisoned") = Some("未知引擎".to_string());
        }
    }
    drop(config);
    persist(state.inner());
    snapshot(state.inner())
}

/// 手动检查更新：结果写入 notice 与 update_latest（设置面板按钮）。
#[tauri::command]
fn check_updates(state: tauri::State<'_, AppState>) -> StateView {
    let current = env!("CARGO_PKG_VERSION");
    let status = check_for_update(&GitHubReleaseFetcher::default(), current);
    match status {
        UpdateStatus::UpToDate => {
            *state.update_latest.lock().expect("update poisoned") = None;
            *state.notice.lock().expect("notice poisoned") =
                Some(format!("当前已是最新版本 v{current}"));
        }
        UpdateStatus::UpdateAvailable(latest) => {
            *state.update_latest.lock().expect("update poisoned") = Some(latest.clone());
            *state.notice.lock().expect("notice poisoned") =
                Some(format!("发现新版本 {latest}（当前 v{current}）"));
        }
        UpdateStatus::Failed => {
            *state.notice.lock().expect("notice poisoned") =
                Some("检查更新失败：网络不可达或被限流，稍后再试".to_string());
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
        Err(error) => {
            *state.notice.lock().expect("notice poisoned") =
                Some(format!("开机自启设置失败：{error}"));
        }
    }
    snapshot(state.inner())
}

/// 设置落盘。目录在 setup 里由 Tauri app_config_dir 提供。
fn persist(state: &AppState) {
    let dir = state.settings_dir.lock().expect("settings dir poisoned").clone();
    if let Some(dir) = dir {
        let snapshot_settings = state.settings.lock().expect("settings poisoned").clone();
        if let Err(message) = settings::save(&dir, &snapshot_settings) {
            *state.notice.lock().expect("notice poisoned") = Some(message);
        }
    }
}

/// 测试连接：用当前配置翻译一句样例，结果写进 notice。
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
            match translator.translate(&plan) {
                Ok(result) => {
                    let text = result
                        .items
                        .first()
                        .and_then(|item| item.text.clone().ok())
                        .unwrap_or_default();
                    *state.notice.lock().expect("notice poisoned") =
                        Some(format!("连接成功：{}", text));
                }
                Err(error) => {
                    *state.notice.lock().expect("notice poisoned") =
                        Some(format!("连接失败：{}", error.0));
                }
            }
        }
        Err(message) => {
            *state.notice.lock().expect("notice poisoned") = Some(message);
        }
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
        Err(message) => {
            *state.notice.lock().expect("notice poisoned") = Some(message);
        }
    }
    *state.chain.lock().expect("provider chain poisoned") = chain;
}

/// 设置面板变更语言池：合法池（2–5 种、不重复）才替换；换池后窗口宽度随之重算并保持贴顶。
#[tauri::command]
fn set_languages(
    languages: Vec<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> StateView {
    let pool = LanguagePool::new(
        languages
            .iter()
            .map(|code| Lang::new(code.as_str()))
            .collect(),
    );
    match pool {
        Ok(pool) => {
            let count = pool.languages().len();
            {
                let mut settings = state.settings.lock().expect("settings poisoned");
                settings.languages = pool.languages().iter().map(|l| l.code().to_string()).collect();
            }
            state
                .coordinator
                .lock()
                .expect("coordinator poisoned")
                .replace_pool(pool);
            if let Some(window) = app.get_webview_window("main") {
                let width = board_width(count);
                let _ = window.set_size(LogicalSize::new(width, 300.0));
                if let Ok(Some(monitor)) = window.current_monitor() {
                    let screen_width = monitor.size().width as f64 / monitor.scale_factor();
                    let _ = window.set_position(LogicalPosition::new(screen_width - width, 0.0));
                }
            }
            *state.notice.lock().expect("notice poisoned") = None;
            persist(state.inner());
        }
        Err(error) => {
            *state.notice.lock().expect("notice poisoned") =
                Some(format!("语言组合无效：{error:?}"));
        }
    }
    snapshot(state.inner())
}

/// 窗口宽度随语言数增长：卡片等宽不压缩。每卡目标 185px + 卡间 9px + 内边距 40px。
fn board_width(card_count: usize) -> f64 {
    40.0 + 185.0 * card_count as f64 + 9.0 * (card_count as f64 - 1.0)
}

/// 胶囊态窗口高度：胶囊 26px + 1px 余量，保证除胶囊外没有任何可点透明区。
const CAPSULE_WINDOW_HEIGHT: f64 = 27.0;

fn main() {
    // 默认三语池演示「改一处、其余全部联动」；工单 03 起可在设置面板改为 2–5 种。
    let pool = LanguagePool::new(vec![
        Lang::new("zh"),
        Lang::new("en"),
        Lang::new("ja"),
    ])
    .expect("default pool uses exactly three languages");
    let card_count = pool.languages().len();
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
            notice: Mutex::new(None),
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
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            hover_enter,
            hover_leave,
            tick,
            esc,
            outside_click,
            pin,
            refresh,
            copy_card,
            commit_edit,
            edit_start,
            edit_end,
            retry_failed,
            set_languages,
            set_provider,
            set_settings,
            set_autostart,
            check_updates,
            open_release_page,
            test_provider,
            list_provider_models,
            speak_card
        ])
        .setup(move |app| {
            // 贴屏幕顶缘、右缘，尺寸就位后才显示，避免窗口在默认位置闪现。
            // 多显示器与换边持久化属工单 08；这里只对主显示器取默认位。
            let window = app.get_webview_window("main").expect("main window exists");
            let width = board_width(card_count);
            window
                .set_size(LogicalSize::new(width, 300.0))
                .expect("resize main window");
            if let Some(monitor) = window.current_monitor().expect("query current monitor") {
                let screen_width = monitor.size().width as f64 / monitor.scale_factor();
                window
                    .set_position(LogicalPosition::new(screen_width - width, 0.0))
                    .expect("position main window");
            }
            window.show().expect("show main window");

            // 加载用户设置并应用：语言池、悬停延迟、引擎链（工单 05 持久化）。
            let state = app.state::<AppState>();
            let dir = app.path().app_config_dir().ok();
            if let Some(dir) = &dir {
                let loaded = settings::load(dir);
                let saved_position = loaded.window_position;
                if let Ok(pool) = LanguagePool::new(
                    loaded
                        .languages
                        .iter()
                        .map(|code| Lang::new(code.as_str()))
                        .collect(),
                ) {
                    let count = pool.languages().len();
                    state
                        .coordinator
                        .lock()
                        .expect("coordinator poisoned")
                        .replace_pool(pool);
                    let width = board_width(count);
                    let _ = window.set_size(LogicalSize::new(width, 300.0));
                    if let Ok(Some(monitor)) = window.current_monitor() {
                        let screen_width =
                            monitor.size().width as f64 / monitor.scale_factor();
                        let _ =
                            window.set_position(LogicalPosition::new(screen_width - width, 0.0));
                    }
                }
                *state.hover.lock().expect("hover state poisoned") =
                    HoverStateMachine::new(loaded.hover_delay_ms, loaded.leave_grace_ms);
                *state.provider_config.lock().expect("provider config poisoned") =
                    loaded.provider.clone();
                rebuild_chain(&loaded.provider, state.inner());
                *state.settings.lock().expect("settings poisoned") = loaded;
                *state.settings_dir.lock().expect("settings dir poisoned") = Some(dir.clone());
                // 位置记忆：恢复上次位置，越界（窗口左上角不在任何显示器内）回落主屏右上。
                if let Some((x, y)) = saved_position {
                    let _ = window.set_position(LogicalPosition::new(x, y));
                    let outer = window.outer_position().ok();
                    let on_screen = outer.is_some_and(|outer| {
                        window
                            .available_monitors()
                            .unwrap_or_default()
                            .iter()
                            .any(|monitor| {
                                let origin = monitor.position();
                                let size = monitor.size();
                                outer.x >= origin.x
                                    && outer.y >= origin.y
                                    && outer.x < origin.x + size.width as i32
                                    && outer.y < origin.y + size.height as i32
                            })
                    });
                    if !on_screen {
                        let width = board_width(
                            state
                                .coordinator
                                .lock()
                                .expect("coordinator poisoned")
                                .document()
                                .slots()
                                .count(),
                        );
                        if let Ok(Some(monitor)) = window.current_monitor() {
                            let screen_width =
                                monitor.size().width as f64 / monitor.scale_factor();
                            let _ = window
                                .set_position(LogicalPosition::new(screen_width - width, 0.0));
                        }
                    }
                }
                // 自启动：已启用则重写当前路径（便携包移动后自愈），并提示一次。
                if state.settings.lock().expect("settings poisoned").autostart_enabled {
                    let autostart = app.autolaunch();
                    let _ = autostart.disable();
                    match autostart.enable() {
                        Ok(()) => {
                            *state.notice.lock().expect("notice poisoned") =
                                Some("已更新开机自启动路径（检测到程序位置变化或例行校正）".to_string());
                        }
                        Err(error) => {
                            *state.notice.lock().expect("notice poisoned") =
                                Some(format!("开机自启动更新失败：{error}"));
                        }
                    }
                }
            }
            // 划词热键线程：注册失败写 notice；开关关闭时热键仍在但不动作（默认关闭，规范要求）。
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
                    *app.state::<AppState>()
                        .notice
                        .lock()
                        .expect("notice poisoned") = Some(
                        "Wayland 环境：窗口定位与置顶受协议限制，可能无法贴边吸附".to_string(),
                    );
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Bead")
        .run(|app_handle, event| {
            // 位置记忆：退出时把窗口左上角（逻辑坐标）写进设置。
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(window) = app_handle.get_webview_window("main") {
                    if let Ok(outer) = window.outer_position() {
                        let scale = window
                            .current_monitor()
                            .ok()
                            .flatten()
                            .map(|monitor| monitor.scale_factor())
                            .unwrap_or(1.0);
                        let logical_x = (outer.x as f64 / scale).round() as i32;
                        let logical_y = (outer.y as f64 / scale).round() as i32;
                        let state = app_handle.state::<AppState>();
                        state.settings.lock().expect("settings poisoned").window_position =
                            Some((logical_x, logical_y));
                        persist(state.inner());
                    }
                }
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
    if let Ok(icon) = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.ico")) {
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
