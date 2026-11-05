#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri 壳层：持有全部系统能力与业务状态，WebView 只发 intent、只收 state 快照。
//!
//! 剪贴板读取只出现在 `consume` 的 `ReadClipboardOnce` 分支里，且每个 intent 最多读一次；
//! 这里没有监听器、没有轮询、没有后台刷新。默认被动因此是结构性的，不靠设置开关去维持。
//! 工单 01 用固定双语池和 fake translator 打通完整链路，provider、语言池设置与编辑联动留到后续工单。

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use bead_core::{
    Clock, HoverAction, HoverEvent, HoverState, HoverStateMachine, Lang, LanguagePool, ProviderError,
    SlotStatus, SyncCoordinator, SyncPlan, TranslationItem, TranslationSet, Translator,
    DEFAULT_HOVER_DELAY_MS, DEFAULT_LEAVE_GRACE_MS,
};
use serde::Serialize;
use tauri::{LogicalPosition, LogicalSize, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before the unix epoch")
            .as_millis() as u64
    }
}

/// 工单 01 的假翻译器：不联网、不写 key，只把源文本按目标语言包一层，用于打通完整链路。
struct EchoTranslator;

impl Translator for EchoTranslator {
    fn translate(&self, request: &SyncPlan) -> Result<TranslationSet, ProviderError> {
        Ok(TranslationSet {
            generation: request.generation,
            items: request
                .targets
                .iter()
                .map(|language| TranslationItem {
                    lang: language.clone(),
                    text: Ok(format!("[{}] {}", language.code(), request.source_text)),
                })
                .collect(),
        })
    }
}

struct AppState {
    coordinator: Mutex<SyncCoordinator<SystemClock>>,
    hover: Mutex<HoverStateMachine>,
    notice: Mutex<Option<String>>,
}

#[derive(Serialize, Clone)]
struct CardView {
    lang: String,
    text: String,
    status: String,
    authoritative: bool,
}

#[derive(Serialize, Clone)]
struct StateView {
    view: String,
    locked: bool,
    cards: Vec<CardView>,
    notice: Option<String>,
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

    StateView {
        view: view.to_string(),
        locked,
        cards,
        notice: notice.clone(),
    }
}

/// 只有状态机产出的 intent 才能触碰系统能力。force_refresh 对应界面上的显式「读取剪贴板」。
fn consume(actions: Vec<HoverAction>, app: &tauri::AppHandle, state: &AppState, force_refresh: bool) {
    for action in actions {
        match action {
            HoverAction::ReadClipboardOnce => match app.clipboard().read_text() {
                Ok(text) => {
                    let plan = state
                        .coordinator
                        .lock()
                        .expect("coordinator poisoned")
                        .accept_clipboard(&text, force_refresh);
                    if let Some(plan) = plan {
                        if let Ok(result) = EchoTranslator.translate(&plan) {
                            let _ = state
                                .coordinator
                                .lock()
                                .expect("coordinator poisoned")
                                .apply(result);
                        }
                    }
                    *state.notice.lock().expect("notice poisoned") = None;
                }
                Err(_) => {
                    *state.notice.lock().expect("notice poisoned") =
                        Some("剪贴板为空或非文本".to_string());
                }
            },
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
    snapshot(state)
}

#[tauri::command]
fn get_state(state: tauri::State<'_, AppState>) -> StateView {
    snapshot(state.inner())
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

#[tauri::command]
fn pin(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> StateView {
    dispatch(HoverEvent::Pin, &app, state.inner(), false)
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

/// 编辑提交：该卡成为权威源，fake 译文立刻填充其余卡（工单 04 换成 provider 链后才引入
/// 真正的异步与过期 generation 竞态；同步命令下 apply 天然发生在同一次调用里）。
#[tauri::command]
fn commit_edit(lang: String, text: String, state: tauri::State<'_, AppState>) -> StateView {
    let plan = state
        .coordinator
        .lock()
        .expect("coordinator poisoned")
        .commit(&Lang::new(&lang), text);
    match plan {
        Ok(plan) => {
            if let Ok(result) = EchoTranslator.translate(&plan) {
                let _ = state
                    .coordinator
                    .lock()
                    .expect("coordinator poisoned")
                    .apply(result);
            }
            *state.notice.lock().expect("notice poisoned") = None;
        }
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
        if let Ok(result) = EchoTranslator.translate(&plan) {
            let _ = state
                .coordinator
                .lock()
                .expect("coordinator poisoned")
                .apply(result);
        }
    }
    snapshot(state.inner())
}

fn main() {
    // 工单 02 用固定三语池演示「改一处、其余全部联动」，语言池设置属工单 03。
    let pool = LanguagePool::new(vec![
        Lang::new("zh"),
        Lang::new("en"),
        Lang::new("ja"),
    ])
    .expect("ticket 02 uses exactly three languages");
    // 窗口宽度随语言数增长：卡片等宽不压缩。每卡目标 185px + 卡间 9px + 内边距 40px。
    let card_count = pool.languages().len();
    let board_width = 40.0 + 185.0 * card_count as f64 + 9.0 * (card_count as f64 - 1.0);
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState {
            coordinator: Mutex::new(SyncCoordinator::new(pool, SystemClock)),
            hover: Mutex::new(HoverStateMachine::new(
                DEFAULT_HOVER_DELAY_MS,
                DEFAULT_LEAVE_GRACE_MS,
            )),
            notice: Mutex::new(None),
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
            retry_failed
        ])
        .setup(move |app| {
            // 贴屏幕顶缘、右缘，尺寸就位后才显示，避免窗口在默认位置闪现。
            // 多显示器与换边持久化属工单 08；这里只对主显示器取默认位。
            let window = app.get_webview_window("main").expect("main window exists");
            window
                .set_size(LogicalSize::new(board_width, 300.0))
                .expect("resize main window");
            if let Some(monitor) = window.current_monitor().expect("query current monitor") {
                let screen_width = monitor.size().width as f64 / monitor.scale_factor();
                window
                    .set_position(LogicalPosition::new(screen_width - board_width, 0.0))
                    .expect("position main window");
            }
            window.show().expect("show main window");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Bead");
}
