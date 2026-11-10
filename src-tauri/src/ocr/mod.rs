//! 内置 PP-OCRv4 引擎：清单、下载校验、推理与引擎调度。
//!
//! 为什么这样而不那样：
//! - 模型与运行时都不进安装包：默认包体仍是 3.5MB 量级。只有用户在设置页显式安装后，
//!   才从用户数据目录按绝对路径加载 `onnxruntime.dll` 与三个 `.onnx`。
//! - 引擎懒加载：只在真正需要识别（且用户选择「自带」）时初始化 3 个会话，
//!   启动路径不碰 ort、不读模型文件。
//! - 任何一步失败都返回 `None`，由调用方静默回退系统 OCR；设置页只显示内联状态，
//!   项目已移除全部 notice / 提示条。
//! - 不做续传、不引入对象存储、下载前不再测速：按清单里的源顺序逐个尝试。

pub mod download;
pub mod infer;
pub mod manifest;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use manifest::Manifest;

/// 剪贴板图像的最小公共表示：BGR 三通道，行优先。
///
/// 用 BGR 而不是 RGB 是为了与 PP-OCR 训练时的 OpenCV 读图顺序一致；
/// 系统 OCR 仍走 BGRA 路径，两者都由 `clipboard_ocr` 提供。
pub struct Image {
    pub bgr: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Image {
    /// 从剪贴板解码得到的 BGRA 转成 BGR；长度不足时返回 `None`。
    pub fn from_bgra(bgra: &[u8], width: u32, height: u32) -> Option<Self> {
        let pixels = (width as usize).checked_mul(height as usize)?;
        if width == 0 || height == 0 || bgra.len() < pixels.checked_mul(4)? {
            return None;
        }
        let mut bgr = Vec::with_capacity(pixels * 3);
        for pixel in bgra.chunks_exact(4).take(pixels) {
            bgr.extend_from_slice(&[pixel[0], pixel[1], pixel[2]]);
        }
        Some(Self {
            bgr,
            width,
            height,
        })
    }
}

/// 引擎选择（持久化在 `settings.json` 的 `ocr_engine` 字段）。
pub const ENGINE_SYSTEM: &str = "system";
pub const ENGINE_BUNDLED: &str = "bundled";

/// OCR 资产与推理的统一入口，挂在 `AppState` 上。
pub struct Manager {
    /// 应用数据目录下的 `ocr/`；启动时由 `configure` 写入。
    dir: Mutex<PathBuf>,
    /// 当前清单：远端拉取成功则用远端，否则用编译期内置的兜底清单。
    manifest: Mutex<Manifest>,
    /// 已加载的推理引擎；删除资产后置空，识别自动回退系统 OCR。
    engine: Mutex<Option<Arc<infer::Engine>>>,
    installing: AtomicBool,
    /// 最近一次可展示给设置页的失败原因（不弹提示、不写日志中的路径之外的机密）。
    error: Mutex<Option<String>>,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            dir: Mutex::new(PathBuf::new()),
            manifest: Mutex::new(Manifest::builtin()),
            engine: Mutex::new(None),
            installing: AtomicBool::new(false),
            error: Mutex::new(None),
        }
    }
}

impl Manager {
    /// 绑定应用数据目录（`<app_data>/ocr`）。
    pub fn configure(&self, app_data: &Path) {
        *self.dir.lock().expect("ocr dir poisoned") = app_data.join("ocr");
    }

    /// 远端清单优先、内置清单兜底；失败不改变现有清单。
    pub fn refresh_manifest(&self) {
        if let Some(remote) = Manifest::fetch() {
            *self.manifest.lock().expect("ocr manifest poisoned") = remote;
        }
    }

    /// 待安装（或已安装）的版本号。
    pub fn version(&self) -> String {
        self.installed_manifest()
            .map(|(_, manifest)| manifest.version)
            .unwrap_or_else(|| self.manifest.lock().expect("ocr manifest poisoned").version.clone())
    }

    /// 需要下载的总体积；已安装时是已落盘版本的体积。
    pub fn total_size(&self) -> u64 {
        self.installed_manifest()
            .map(|(_, manifest)| manifest.total_size())
            .unwrap_or_else(|| self.manifest.lock().expect("ocr manifest poisoned").total_size())
    }

    pub fn installed(&self) -> bool {
        self.installed_manifest().is_some()
    }

    pub fn busy(&self) -> bool {
        self.installing.load(Ordering::SeqCst)
    }

    pub fn last_error(&self) -> Option<String> {
        self.error.lock().expect("ocr error poisoned").clone()
    }

    /// 识别入口：只有已安装 + 已加载成功才返回文本；其他情况一律 `None`。
    ///
    /// `language` 是卡片语言族（`zh` / `en` / `None`）。当前只有中英 bundle，
    /// 其他语言直接放弃内置引擎，交给系统 OCR。
    pub fn recognize(&self, image: &Image, language: Option<&str>) -> Option<String> {
        if let Some(language) = language {
            let supported = self
                .manifest
                .lock()
                .expect("ocr manifest poisoned")
                .supports_language(language);
            if !supported {
                return None;
            }
        }
        let (dir, _) = self.installed_manifest()?;
        let engine = self.engine_for(&dir)?;
        engine.recognize(image)
    }

    fn engine_for(&self, dir: &Path) -> Option<Arc<infer::Engine>> {
        let mut slot = self.engine.lock().expect("ocr engine poisoned");
        if let Some(engine) = slot.as_ref() {
            return Some(engine.clone());
        }
        match infer::Engine::load(dir) {
            Ok(engine) => {
                let engine = Arc::new(engine);
                *slot = Some(engine.clone());
                *self.error.lock().expect("ocr error poisoned") = None;
                Some(engine)
            }
            Err(message) => {
                *self.error.lock().expect("ocr error poisoned") = Some(message);
                None
            }
        }
    }

    /// 已安装版本：目录中存在 `installed.json` 且清单里的每个文件尺寸都对得上。
    ///
    /// 只做尺寸检查，不在每次读状态时重算 35MB 的 SHA256；SHA256 在安装时逐文件校验。
    fn installed_manifest(&self) -> Option<(PathBuf, Manifest)> {
        let dir = self.dir.lock().expect("ocr dir poisoned").clone();
        if dir.as_os_str().is_empty() {
            return None;
        }
        let mut candidates: Vec<(PathBuf, Manifest)> = Vec::new();
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path.join("installed.json")) else {
                continue;
            };
            let Some(manifest) = Manifest::parse(&text) else {
                continue;
            };
            if manifest
                .files
                .iter()
                .all(|file| file_size_matches(&path.join(&file.name), file.size))
            {
                candidates.push((path, manifest));
            }
        }
        candidates.sort_by_key(|entry| std::cmp::Reverse(version_key(&entry.1.version)));
        candidates.into_iter().next()
    }

    /// 阻塞式安装；进度通过回调上报（主程序把它转成 Tauri 事件）。
    pub fn install_blocking(&self, report: impl Fn(download::Progress)) -> Result<(), String> {
        let dir = self.dir.lock().expect("ocr dir poisoned").clone();
        if dir.as_os_str().is_empty() {
            return Err("OCR 目录尚未就绪".into());
        }
        let manifest = self.manifest.lock().expect("ocr manifest poisoned").clone();
        let target = dir.join(&manifest.version);
        self.installing.store(true, Ordering::SeqCst);
        *self.error.lock().expect("ocr error poisoned") = None;
        let result = download::install(&target, &manifest, &mut |progress| report(progress));
        if let Err(message) = result.as_ref() {
            *self.error.lock().expect("ocr error poisoned") = Some(message.clone());
        } else {
            // 新版本装好后清掉旧版本目录；DLL 若仍被本进程占用，删除会失败，忽略即可。
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && path != target {
                        let _ = std::fs::remove_dir_all(path);
                    }
                }
            }
        }
        self.installing.store(false, Ordering::SeqCst);
        result
    }

    /// 删除全部已下载版本并卸载引擎句柄（DLL 由系统在进程退出时释放）。
    pub fn remove(&self) {
        let dir = self.dir.lock().expect("ocr dir poisoned").clone();
        // 句柄先释放，避免 Windows 上文件仍被当前进程占用。
        *self.engine.lock().expect("ocr engine poisoned") = None;
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        *self.error.lock().expect("ocr error poisoned") = None;
    }
}

fn file_size_matches(path: &Path, size: u64) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.len() == size)
        .unwrap_or(false)
}

fn version_key(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_is_converted_to_bgr_in_row_order() {
        let bgra = vec![1, 2, 3, 255, 4, 5, 6, 255];
        let image = Image::from_bgra(&bgra, 2, 1).expect("image");
        assert_eq!(image.bgr, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn short_buffers_are_rejected() {
        assert!(Image::from_bgra(&[0, 0, 0], 1, 1).is_none());
        assert!(Image::from_bgra(&[], 0, 0).is_none());
    }

    #[test]
    fn builtin_manifest_lists_the_four_required_assets() {
        let manifest = Manifest::builtin();
        assert!(manifest.asset("det").is_some());
        assert!(manifest.asset("rec").is_some());
        assert!(manifest.asset("cls").is_some());
        assert!(manifest.asset("runtime").is_some());
        assert_eq!(manifest.total_size(), 35_477_895);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(version_key("1.10.0") > version_key("1.9.9"));
    }

    /// 真下载自测：联网从清单里的源拉 4 个资产并逐个校验 SHA256。
    /// 默认忽略，避免日常测试和 CI 依赖外网与 35MB 流量。
    #[test]
    #[ignore = "需要联网下载 35MB 资产"]
    fn self_test_download_and_verify() {
        let dir = std::env::temp_dir().join("bead-ocr-download-selftest");
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = Manifest::builtin();
        let mut seen = 0usize;
        download::install(&dir, &manifest, &mut |_| seen += 1).expect("下载并校验失败");
        assert!(seen > 0);
        for file in &manifest.files {
            assert!(dir.join(&file.name).is_file(), "{} 未落盘", file.name);
        }
        assert!(dir.join("installed.json").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
