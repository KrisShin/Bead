#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 独立探针：只回答「Windows 上 Tauri v2 的窗口定位 / 置顶、剪贴板读一次、hover 200ms 防抖是否可行」。
//! 不依赖主程序，不注册任何剪贴板监听器，不写原文日志；原剪贴板为文本时写回恢复，进程自行退出。

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Mutex,
};

use serde::Serialize;
use tauri::{Manager, PhysicalPosition};
use tauri_plugin_clipboard_manager::ClipboardExt;

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[derive(Serialize, Clone)]
struct ProbeReport {
    window_position_set_ok: bool,
    requested_position: (i32, i32),
    observed_position: Option<(i32, i32)>,
    always_on_top_call_ok: bool,
    clipboard_read_ok: bool,
    clipboard_read_error: Option<String>,
    clipboard_write_ok: bool,
    clipboard_readback_error: Option<String>,
    clipboard_write_readback_ok: bool,
    clipboard_restored: bool,
    clipboard_write_probe: String,
    clipboard_content_hash: Option<u64>,
    hover_debounce_ms: Option<u32>,
    hover_observed_ok: bool,
    clipboard_listener_registered: bool,
}

impl Default for ProbeReport {
    fn default() -> Self {
        Self {
            window_position_set_ok: false,
            requested_position: (0, 0),
            observed_position: None,
            always_on_top_call_ok: false,
            clipboard_read_ok: false,
            clipboard_read_error: None,
            clipboard_write_ok: false,
            clipboard_readback_error: None,
            clipboard_write_readback_ok: false,
            clipboard_restored: false,
            clipboard_write_probe: "not_run".into(),
            clipboard_content_hash: None,
            hover_debounce_ms: None,
            hover_observed_ok: false,
            clipboard_listener_registered: false,
        }
    }
}

struct ProbeState(Mutex<ProbeReport>);

#[tauri::command]
fn report_hover(elapsed_ms: u32, state: tauri::State<'_, ProbeState>) {
    let mut report = state.0.lock().expect("probe state poisoned");
    report.hover_debounce_ms = Some(elapsed_ms);
    report.hover_observed_ok = elapsed_ms >= 190;
}

#[tauri::command]
fn probe_exit(app: tauri::AppHandle, state: tauri::State<'_, ProbeState>) {
    let report = state.0.lock().expect("probe state poisoned").clone();
    println!("BEAD_PROBE_JSON={}", serde_json::to_string(&report).expect("serialize report"));
    app.exit(0);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(ProbeState(Mutex::new(ProbeReport::default())))
        .invoke_handler(tauri::generate_handler![report_hover, probe_exit])
        .setup(|app| {
            let state = app.state::<ProbeState>();
            let mut report = state.0.lock().expect("probe state poisoned");

            if let Some(window) = app.get_webview_window("main") {
                let requested = (1180i32, 260i32);
                report.requested_position = requested;
                report.window_position_set_ok = window
                    .set_position(PhysicalPosition::new(requested.0, requested.1))
                    .is_ok();
                report.observed_position = window
                    .outer_position()
                    .ok()
                    .map(|p| (p.x as i32, p.y as i32));
                report.always_on_top_call_ok = window.set_always_on_top(true).is_ok();
            }

            let clipboard = app.handle().clipboard();
            let marker = "bead-probe-marker".to_string();
            match clipboard.read_text() {
                Ok(original) => {
                    report.clipboard_read_ok = true;
                    report.clipboard_content_hash = Some(hash_text(&original));
                    report.clipboard_write_ok = clipboard.write_text(marker.clone()).is_ok();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let readback = match clipboard.read_text() {
                        Ok(text) => Some(text),
                        Err(error) => {
                            report.clipboard_readback_error = Some(error.to_string());
                            None
                        }
                    };
                    report.clipboard_write_readback_ok = report.clipboard_write_ok
                        && readback.as_deref() == Some(marker.as_str());
                    report.clipboard_restored = clipboard.write_text(original).is_ok();
                    report.clipboard_write_probe = "full_with_restore".into();
                }
                Err(error) => {
                    let message = error.to_string();
                    let lower = message.to_lowercase();
                    report.clipboard_read_error = Some(message);
                    // 只有确认剪贴板里没有文本时，才写入标记并读回，随后写回空串恢复「无文本」状态。
                    // 非文本或权限错误一律跳过写入，避免覆盖用户原始数据。
                    if lower.contains("empty") || lower.contains("no text") {
                        report.clipboard_write_ok = clipboard.write_text(marker.clone()).is_ok();
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        let readback = match clipboard.read_text() {
                            Ok(text) => Some(text),
                            Err(error) => {
                                report.clipboard_readback_error = Some(error.to_string());
                                None
                            }
                        };
                        report.clipboard_write_readback_ok = report.clipboard_write_ok
                            && readback.as_deref() == Some(marker.as_str());
                        report.clipboard_restored = clipboard.write_text(String::new()).is_ok();
                        report.clipboard_write_probe = "marker_then_empty".into();
                    } else {
                        report.clipboard_write_probe = "skipped_non_text_or_permission".into();
                    }
                }
            }

            // Watchdog: even if the WebView never runs its script, no window or process survives.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(12));
                handle.exit(0);
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("probe failed to run");
}
