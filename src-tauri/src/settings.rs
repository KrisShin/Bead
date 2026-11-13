//! 用户设置：持久化到系统配置目录（Windows %APPDATA%\dev.bead.app 等）。
//!
//! 未知字段直接忽略（serde 默认行为）、缺失字段回落默认——升级路径由此保证：
//! 新版本加字段不炸旧配置，旧配置缺新字段不炸新版本。文件损坏时回落默认并继续启动，
//! 绝不让坏配置挡住主程序。key/secret 随 provider 配置一起落盘（用户本机配置目录，
//! 权限即用户自身），不写日志。
//!
//! 已从界面撤掉的设置（启动检查更新、朗读语速/音量、在线朗读兜底、键盘选区兜底）
//! 直接删字段：行为改为常量，旧配置里的同名键靠「未知字段忽略」静默丢弃。
//! `selection_anchor_enabled` 例外——它仍保留字段以便读取老配置，但不再参与任何判断
//! （图标显示已与划词开关合并）。

use std::path::{Path, PathBuf};

use bead_providers::ProviderConfig;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    #[serde(default = "default_true")]
    pub auto_read_clipboard: bool,
    #[serde(default = "default_hover_delay")]
    pub hover_delay_ms: u64,
    #[serde(default = "default_leave_grace")]
    pub leave_grace_ms: u64,
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default = "default_languages")]
    pub languages: Vec<String>,
    /// 划词默认关闭（规范：默认被动；开启后快捷键触发取词）。
    #[serde(default)]
    pub selection_enabled: bool,
    #[serde(default = "default_hotkey")]
    pub selection_hotkey: String,
    /// 选区锚点图标的旧字段：**仅保留可反序列化，不生效**。
    /// 图标显示已与「划词」开关合并（开启划词即可显示图标），该字段不再作为
    /// 用户可设置的独立偏好参与任何判断；保留是为了老配置静默通过解析。
    #[serde(default)]
    pub selection_anchor_enabled: bool,
    /// 开机自启（用户显式开启；便携包移动后启动时自动重写路径并提示）。
    #[serde(default)]
    pub autostart_enabled: bool,
    /// OCR 引擎：`system`（系统 WinRT，默认）或 `bundled`（内置 PP-OCRv4）。
    /// `bundled` 未安装或加载失败时运行时静默回退系统 OCR，不改写用户选择。
    #[serde(default = "default_ocr_engine")]
    pub ocr_engine: String,
    /// 窗口位置（旧字段，逻辑坐标；保留写入以兼容旧构建）。
    #[serde(default)]
    pub window_position: Option<(i32, i32)>,
    /// 胶囊锚点：胶囊左上角逻辑坐标，拖拽后记忆。形态切换只向左/下生长，
    /// 右缘与上缘由这个锚点决定，因此展开不会把胶囊挪走。
    #[serde(default)]
    pub capsule_anchor: Option<(i32, i32)>,
}

fn default_hotkey() -> String {
    bead_core::DEFAULT_HOTKEY.into()
}

fn default_true() -> bool {
    true
}

fn default_hover_delay() -> u64 {
    400
}

fn default_leave_grace() -> u64 {
    800
}

fn default_ocr_engine() -> String {
    crate::ocr::ENGINE_SYSTEM.into()
}

fn default_languages() -> Vec<String> {
    vec!["zh".into(), "en".into(), "ja".into()]
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_read_clipboard: true,
            hover_delay_ms: default_hover_delay(),
            leave_grace_ms: default_leave_grace(),
            provider: ProviderConfig::default(),
            languages: default_languages(),
            selection_enabled: false,
            selection_hotkey: default_hotkey(),
            selection_anchor_enabled: false,
            autostart_enabled: false,
            window_position: None,
            capsule_anchor: None,
            ocr_engine: default_ocr_engine(),
        }
    }
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

pub fn load(dir: &Path) -> AppSettings {
    let path = settings_path(dir);
    match std::fs::read_to_string(&path) {
        Ok(body) => match serde_json::from_str::<AppSettings>(&body) {
            Ok(settings) => settings,
            Err(_) => AppSettings::default(), // 损坏的配置：回落默认，不挡启动
        },
        Err(_) => AppSettings::default(), // 首次启动
    }
}

pub fn save(dir: &Path, settings: &AppSettings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("创建配置目录失败：{error}"))?;
    let body = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("序列化设置失败：{error}"))?;
    std::fs::write(settings_path(dir), body).map_err(|error| format!("写入设置失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bead-settings-test-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_roundtrip_preserves_every_field() {
        let dir = temp_dir("roundtrip");
        let settings = AppSettings {
            auto_read_clipboard: false,
            hover_delay_ms: 350,
            leave_grace_ms: 900,
            selection_enabled: true,
            selection_hotkey: "Ctrl+Alt+B".into(),
            selection_anchor_enabled: true,
            autostart_enabled: false,
            window_position: Some((100, 100)),
            capsule_anchor: Some((100, 100)),
            ocr_engine: "bundled".into(),
            provider: ProviderConfig {
                engine: bead_providers::Engine::Zhipu,
                api_key: "key-1".into(),
                secret: String::new(),
                model: "glm-4.7-flash".into(),
                base_url: String::new(),
            },
            languages: vec!["zh".into(), "de".into()],
        };
        save(&dir, &settings).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.auto_read_clipboard, false);
        assert_eq!(loaded.hover_delay_ms, 350);
        assert_eq!(loaded.provider.engine, bead_providers::Engine::Zhipu);
        assert_eq!(loaded.provider.api_key, "key-1");
        assert_eq!(loaded.languages, vec!["zh".to_string(), "de".to_string()]);
        assert_eq!(loaded.capsule_anchor, Some((100, 100)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_with_the_removed_tts_and_update_fields_still_loads() {
        // 五个开关已从界面撤掉并固定为默认值；老配置里的这些键必须静默通过解析，
        // 且不能影响其它字段，避免升级后整份配置被当成损坏文件回落默认。
        let dir = temp_dir("removed-fields");
        let legacy = r#"{
            "auto_read_clipboard": false,
            "hover_delay_ms": 350,
            "auto_check_updates": false,
            "tts_rate": 1.7,
            "tts_volume": 35,
            "tts_online_fallback": false,
            "selection_keyboard_fallback": false,
            "selection_anchor_enabled": true,
            "selection_enabled": true,
            "provider": {"engine": "deepl", "api_key": "k"}
        }"#;
        std::fs::write(settings_path(&dir), legacy).unwrap();
        let loaded = load(&dir);
        assert!(!loaded.auto_read_clipboard);
        assert_eq!(loaded.hover_delay_ms, 350);
        assert!(loaded.selection_enabled);
        assert_eq!(loaded.provider.engine, bead_providers::Engine::DeepL);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_fields_fall_back_to_defaults() {
        let dir = temp_dir("compat");
        let legacy = r#"{
            "auto_read_clipboard": false,
            "some_removed_field": 123,
            "provider": {"engine": "deepl", "api_key": "k"}
        }"#;
        std::fs::write(settings_path(&dir), legacy).unwrap();
        let loaded = load(&dir);
        assert!(!loaded.auto_read_clipboard);
        assert_eq!(loaded.hover_delay_ms, 400); // 缺失字段回落默认
        assert_eq!(loaded.provider.engine, bead_providers::Engine::DeepL);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupted_settings_file_falls_back_to_defaults_instead_of_blocking_startup() {
        let dir = temp_dir("corrupt");
        std::fs::write(settings_path(&dir), "{ not json ").unwrap();
        let loaded = load(&dir);
        assert!(loaded.auto_read_clipboard);
        std::fs::remove_dir_all(&dir).ok();
    }
}
