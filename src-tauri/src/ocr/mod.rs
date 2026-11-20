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
use std::time::{Duration, Instant};

use manifest::Manifest;

/// 进入识别前的最大长边：超过就按面积平均降采样。
///
/// 检测模型本来就把长边压到 960，4K 截图先缩到 1600 再转 BGR，可以省掉
/// 一份 25MB 级的全尺寸 BGR 缓冲与随之翻倍的张量中间产物，对识别结果没有影响。
const MAX_INPUT_EDGE: usize = 1600;

/// 进入识别前的最小长边：小于它就放大。
///
/// 检测模型只降不升（长边 ≤960），200px 宽的小图送进去实际只有 192px，
/// 7px 小字会因连通域过碎而识别错字。实测 200×40（7px 字号）原尺寸识别
/// 出「到译」、放大 2 倍后恢复「翻译」；而正常尺寸的截图放大前后结果一致，
/// 因此只在长边明显偏小时放大，最多 3 倍（小图本身像素量很低，代价可忽略）。
const MIN_INPUT_EDGE: usize = 640;
const MAX_UPSCALE: f32 = 3.0;

/// 空闲多久自动卸载引擎（释放 ONNX 会话常驻内存）。
///
/// 识别是低频动作；几分钟不用还留着近 90MB 的会话内存不合理。下次识别重新加载，
/// 代价是几百毫秒（模型文件已在本机缓存）。卸载只丢会话，`onnxruntime.dll` 与
/// Environment 保留——DLL 加载后无法卸载，Environment 也必须在进程内唯一。
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);

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
    ///
    /// 长边超过 [`MAX_INPUT_EDGE`] 时同一次遍历里按面积平均降采样，不额外产生
    /// 全尺寸中间图；长边小于 [`MIN_INPUT_EDGE`] 时双线性放大（最多 3 倍）以改善
    /// 小字识别。调用方拿到即可用，识别结束后由调用方立即释放。
    pub fn from_bgra(bgra: &[u8], width: u32, height: u32) -> Option<Self> {
        let pixels = (width as usize).checked_mul(height as usize)?;
        if width == 0 || height == 0 || bgra.len() < pixels.checked_mul(4)? {
            return None;
        }
        let source_width = width as usize;
        let source_height = height as usize;
        let longest = source_width.max(source_height);
        if longest <= MAX_INPUT_EDGE && longest >= MIN_INPUT_EDGE {
            let mut bgr = Vec::with_capacity(pixels * 3);
            for pixel in bgra.chunks_exact(4).take(pixels) {
                bgr.extend_from_slice(&[pixel[0], pixel[1], pixel[2]]);
            }
            return Some(Self {
                bgr,
                width,
                height,
            });
        }

        if longest > MAX_INPUT_EDGE {
            let scale = MAX_INPUT_EDGE as f64 / longest as f64;
            let target_width = ((source_width as f64 * scale).round() as usize).max(1);
            let target_height = ((source_height as f64 * scale).round() as usize).max(1);
            let mut bgr = vec![0u8; target_width * target_height * 3];
            for target_y in 0..target_height {
                let y0 = target_y * source_height / target_height;
                let y1 =
                    (((target_y + 1) * source_height / target_height).max(y0 + 1)).min(source_height);
                for target_x in 0..target_width {
                    let x0 = target_x * source_width / target_width;
                    let x1 = (((target_x + 1) * source_width / target_width).max(x0 + 1))
                        .min(source_width);
                    let (mut blue, mut green, mut red) = (0u32, 0u32, 0u32);
                    for y in y0..y1 {
                        let start = (y * source_width + x0) * 4;
                        for index in (start..start + (x1 - x0) * 4).step_by(4) {
                            blue += bgra[index] as u32;
                            green += bgra[index + 1] as u32;
                            red += bgra[index + 2] as u32;
                        }
                    }
                    let count = ((x1 - x0) * (y1 - y0)) as u32;
                    let target = (target_y * target_width + target_x) * 3;
                    bgr[target] = (blue / count) as u8;
                    bgr[target + 1] = (green / count) as u8;
                    bgr[target + 2] = (red / count) as u8;
                }
            }
            return Some(Self {
                bgr,
                width: target_width as u32,
                height: target_height as u32,
            });
        }

        let factor = (MIN_INPUT_EDGE as f32 / longest.max(1) as f32).min(MAX_UPSCALE);
        let target_width = (source_width as f32 * factor).round() as usize;
        let target_height = (source_height as f32 * factor).round() as usize;
        Some(Self {
            bgr: upscale_bgra(bgra, source_width, source_height, target_width, target_height),
            width: target_width as u32,
            height: target_height as u32,
        })
    }
}

/// 双线性放大 BGRA → BGR；目标尺寸大于源尺寸。
fn upscale_bgra(
    bgra: &[u8],
    source_width: usize,
    source_height: usize,
    target_width: usize,
    target_height: usize,
) -> Vec<u8> {
    let mut bgr = vec![0u8; target_width * target_height * 3];
    let scale_x = source_width as f32 / target_width as f32;
    let scale_y = source_height as f32 / target_height as f32;
    for y in 0..target_height {
        let fy = ((y as f32 + 0.5) * scale_y - 0.5).clamp(0.0, source_height as f32 - 1.0);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let wy = fy - y0 as f32;
        for x in 0..target_width {
            let fx = ((x as f32 + 0.5) * scale_x - 0.5).clamp(0.0, source_width as f32 - 1.0);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let wx = fx - x0 as f32;
            let top = (y0 * source_width + x0) * 4;
            let bottom = (y1 * source_width + x0) * 4;
            let target = (y * target_width + x) * 3;
            for channel in 0..3 {
                let top_left = bgra[top + channel] as f32;
                let top_right = bgra[(y0 * source_width + x1) * 4 + channel] as f32;
                let bottom_left = bgra[bottom + channel] as f32;
                let bottom_right = bgra[(y1 * source_width + x1) * 4 + channel] as f32;
                let upper = top_left + (top_right - top_left) * wx;
                let lower = bottom_left + (bottom_right - bottom_left) * wx;
                bgr[target + channel] = (upper + (lower - upper) * wy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    bgr
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
    /// 最近一次识别时间，供空闲卸载线程判断。
    last_used: Mutex<Instant>,
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
            last_used: Mutex::new(Instant::now()),
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
        if let Ok(mut last_used) = self.last_used.lock() {
            *last_used = Instant::now();
        }
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

    /// 空闲超时就丢掉会话；返回是否真的卸载了。由后台低频率线程调用。
    ///
    /// 正在识别的调用方持有 `Arc<Engine>`，这里 take 掉只会让内存在那次识别结束后回收，
    /// 不会中断识别。
    pub fn unload_if_idle(&self) -> bool {
        let idle = self
            .last_used
            .lock()
            .map(|last_used| last_used.elapsed())
            .unwrap_or_default();
        if idle < IDLE_UNLOAD {
            return false;
        }
        let mut slot = self.engine.lock().expect("ocr engine poisoned");
        slot.take().is_some()
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
        // 640 宽落在原尺寸区间，不触发放大/缩小，正好验证通道顺序。
        let mut bgra = vec![0u8; 640 * 4];
        bgra[..8].copy_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        let image = Image::from_bgra(&bgra, 640, 1).expect("image");
        assert_eq!(&image.bgr[..6], &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn short_buffers_are_rejected() {
        assert!(Image::from_bgra(&[0, 0, 0], 1, 1).is_none());
        assert!(Image::from_bgra(&[], 0, 0).is_none());
    }

    #[test]
    fn oversized_images_are_downscaled_before_ocr() {
        let bgra = vec![255u8; 4000 * 200 * 4];
        let image = Image::from_bgra(&bgra, 4000, 200).expect("image");
        assert_eq!((image.width, image.height), (1600, 80));
        assert_eq!(image.bgr.len(), 1600 * 80 * 3);
    }

    #[test]
    fn tiny_images_are_upscaled_at_most_three_times() {
        let bgra = vec![128u8; 100 * 40 * 4];
        let image = Image::from_bgra(&bgra, 100, 40).expect("image");
        assert_eq!((image.width, image.height), (300, 120));

        // 320 宽按 640 上限放大 2 倍。
        let bgra = vec![128u8; 320 * 56 * 4];
        let image = Image::from_bgra(&bgra, 320, 56).expect("image");
        assert_eq!((image.width, image.height), (640, 112));
    }

    #[test]
    fn normal_images_keep_their_size() {
        let bgra = vec![128u8; 640 * 480 * 4];
        let image = Image::from_bgra(&bgra, 640, 480).expect("image");
        assert_eq!((image.width, image.height), (640, 480));
        assert_eq!(image.bgr.len(), 640 * 480 * 3);
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
