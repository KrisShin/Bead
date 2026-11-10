//! PP-OCRv4 推理链路：DB 文本检测 → 180° 方向分类 → CTC 识别。
//!
//! 模型输出形式（已用 onnx 探针确认，见 `demo/ocr-probe/probe.py`）：
//! - det：`[1, 1, H, W]` 概率图，阈值 0.3 二值化；
//! - cls：`[1, 2]`，1 表示需要旋转 180°；
//! - rec：`[1, T, 6625]`，6625 = 6623 个字符（字典内嵌在模型 metadata 的 `character` 键）
//!   + blank + 空格。解码：0 = blank，1..=6623 = 字典下标 - 1，6624 = 空格。
//!
//! 因此**不需要**额外的 `ppocr_keys_v1.txt`，模型自带字符表。
//!
//! 检测后处理不引 OpenCV：用连通域 + 轴对齐外框 + unclip 扩张近似 PaddleOCR 的
//! minAreaRect 流程。截图里的文字基本水平，倾斜框精度损失可以接受，换来零 C 依赖。
//! 单个汉字的独立连通域会按「垂直重叠 + 水平邻近」合并回同一行。

use std::path::Path;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

use super::Image;

const DET_NAME: &str = "ch_PP-OCRv4_det_infer.onnx";
const CLS_NAME: &str = "ch_ppocr_mobile_v2.0_cls_infer.onnx";
const REC_NAME: &str = "ch_PP-OCRv4_rec_infer.onnx";
const RUNTIME_NAME: &str = "onnxruntime.dll";

/// 检测输入最长边；PP-OCRv4 的默认值。
const DET_LIMIT: usize = 960;
const DET_THRESHOLD: f32 = 0.3;
/// 框内平均置信度低于此值直接丢弃（PaddleOCR 默认 0.6，这里放宽以免漏检）。
const BOX_THRESHOLD: f32 = 0.5;
/// unclip 扩张比例：文字框向外扩，保证边缘字不被裁掉。
const UNCLIP_RATIO: f32 = 1.5;
const REC_HEIGHT: usize = 48;
const CLS_WIDTH: usize = 192;
const MAX_REC_WIDTH: usize = 3200;

const DET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const DET_STD: [f32; 3] = [0.229, 0.224, 0.225];

/// 三个 ONNX 会话 + 内嵌字符字典。
pub struct Engine {
    det: Mutex<Session>,
    cls: Mutex<Session>,
    rec: Mutex<Session>,
    dict: Vec<String>,
}

impl Engine {
    /// 从资产目录加载；`onnxruntime.dll` 必须是绝对路径且已下载校验。
    pub fn load(dir: &Path) -> Result<Self, String> {
        let runtime = dir.join(RUNTIME_NAME);
        if !runtime.is_file() {
            return Err("缺少 onnxruntime.dll".into());
        }
        let environment = ort::init_from(&runtime)
            .map_err(|error| format!("加载 onnxruntime 失败：{error}"))?
            .with_name("bead-ocr")
            .build()
            .map_err(|error| format!("初始化推理环境失败：{error}"))?;
        let det = session(&environment, &dir.join(DET_NAME))?;
        let cls = session(&environment, &dir.join(CLS_NAME))?;
        let rec = session(&environment, &dir.join(REC_NAME))?;
        let raw = rec
            .metadata()
            .map_err(|error| format!("读取识别模型元数据失败：{error}"))?
            .custom("character")
            .ok_or("识别模型缺少内嵌字符字典")?;
        let mut dict: Vec<String> = raw.split('\n').map(|entry| entry.to_string()).collect();
        if dict.last().is_some_and(|entry| entry.is_empty()) {
            dict.pop();
        }
        if dict.len() < 100 {
            return Err("字符字典异常".into());
        }
        Ok(Self {
            det: Mutex::new(det),
            cls: Mutex::new(cls),
            rec: Mutex::new(rec),
            dict,
        })
    }

    /// 识别一整张图；空结果返回 `None`（调用方静默回退系统 OCR）。
    pub fn recognize(&self, image: &Image) -> Option<String> {
        let boxes = self.detect(image)?;
        if boxes.is_empty() {
            return None;
        }
        let mut crops: Vec<Image> = boxes
            .iter()
            .filter_map(|region| crop(image, *region))
            .collect();
        if crops.is_empty() {
            return None;
        }
        self.classify(&mut crops);
        let mut lines = Vec::new();
        for crop in &crops {
            if let Some(text) = self.recognize_line(crop) {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    lines.push(text);
                }
            }
        }
        let text = lines.join("\n").trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    fn detect(&self, image: &Image) -> Option<Vec<Region>> {
        let (width, height) = det_size(image.width as usize, image.height as usize);
        let resized = resize(image, width, height);
        let data = to_tensor_input(&resized, DET_MEAN, DET_STD);
        let tensor = Tensor::from_array(([1i64, 3, height as i64, width as i64], data)).ok()?;
        let mut det = self.det.lock().ok()?;
        let outputs = det.run(ort::inputs!["x" => tensor]).ok()?;
        let (_, values) = outputs[0].try_extract_tensor::<f32>().ok()?;
        let probability = values.to_vec();
        drop(outputs);
        drop(det);

        let scale_x = image.width as f32 / width as f32;
        let scale_y = image.height as f32 / height as f32;
        Some(detect_regions(
            &probability,
            width,
            height,
            scale_x,
            scale_y,
            image.width,
            image.height,
        ))
    }

    fn classify(&self, crops: &mut [Image]) {
        let Ok(mut cls) = self.cls.lock() else {
            return;
        };
        for crop in crops.iter_mut() {
            let ratio = crop.width as f32 / crop.height.max(1) as f32;
            let width = ((REC_HEIGHT as f32 * ratio).ceil() as usize).clamp(4, CLS_WIDTH);
            let resized = resize(crop, width, REC_HEIGHT);
            let mut data = vec![0f32; 3 * REC_HEIGHT * CLS_WIDTH];
            fill_normalized(&resized, &mut data, width, REC_HEIGHT, CLS_WIDTH, [0.5; 3], [0.5; 3]);
            let Ok(tensor) = Tensor::from_array((
                [1i64, 3, REC_HEIGHT as i64, CLS_WIDTH as i64],
                data,
            )) else {
                continue;
            };
            let Ok(outputs) = cls.run(ort::inputs!["x" => tensor]) else {
                continue;
            };
            let Ok((_, values)) = outputs[0].try_extract_tensor::<f32>() else {
                continue;
            };
            if values.len() < 2 {
                continue;
            }
            let rotated = values[1] > values[0] && values[1] > 0.9;
            if rotated {
                rotate_180(crop);
            }
        }
    }

    fn recognize_line(&self, crop: &Image) -> Option<String> {
        let ratio = crop.width as f32 / crop.height.max(1) as f32;
        let width = ((REC_HEIGHT as f32 * ratio).ceil() as usize).clamp(4, MAX_REC_WIDTH);
        let resized = resize(crop, width, REC_HEIGHT);
        let data = to_tensor_input(&resized, [0.5; 3], [0.5; 3]);
        let tensor =
            Tensor::from_array(([1i64, 3, REC_HEIGHT as i64, width as i64], data)).ok()?;
        let mut rec = self.rec.lock().ok()?;
        let outputs = rec.run(ort::inputs!["x" => tensor]).ok()?;
        let (shape, values) = outputs[0].try_extract_tensor::<f32>().ok()?;
        let timesteps = *shape.get(1)? as usize;
        let classes = *shape.get(2)? as usize;
        if timesteps == 0 || classes == 0 || values.len() < timesteps * classes {
            return None;
        }
        Some(decode_ctc(values, timesteps, classes, &self.dict, 0.5))
    }
}

fn session(environment: &ort::environment::Environment, path: &Path) -> Result<Session, String> {
    if !path.is_file() {
        return Err(format!(
            "缺少模型文件：{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    Session::builder(environment)
        .map_err(|error| format!("创建会话失败：{error}"))?
        .commit_from_file(path)
        .map_err(|error| format!("加载模型失败：{error}"))
}

/// 检测输入尺寸：长边不超过 960，两边都对齐到 32 的倍数（PP-OCR 的下采样要求）。
fn det_size(width: usize, height: usize) -> (usize, usize) {
    let scale = (DET_LIMIT as f32 / width.max(height).max(1) as f32).min(1.0);
    let round = |value: usize| -> usize { ((value as f32 * scale / 32.0).round() as usize * 32).max(32) };
    (round(width), round(height))
}

/// 二值概率图 → 连通域 → 过滤 → unclip 扩张 → 合并成行 → 排序。
fn detect_regions(
    probability: &[f32],
    width: usize,
    height: usize,
    scale_x: f32,
    scale_y: f32,
    image_width: u32,
    image_height: u32,
) -> Vec<Region> {
    if probability.len() < width * height || width == 0 || height == 0 {
        return Vec::new();
    }
    let mask: Vec<bool> = probability[..width * height]
        .iter()
        .map(|value| *value > DET_THRESHOLD)
        .collect();
    let mut visited = vec![false; mask.len()];
    let mut regions = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for start in 0..mask.len() {
        if !mask[start] || visited[start] {
            continue;
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let mut count = 0usize;
        let mut score = 0f32;
        visited[start] = true;
        stack.push(start);
        while let Some(index) = stack.pop() {
            let x = index % width;
            let y = index / width;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            count += 1;
            score += probability[index];
            let x_start = x.saturating_sub(1);
            let y_start = y.saturating_sub(1);
            for ny in y_start..=(y + 1).min(height - 1) {
                for nx in x_start..=(x + 1).min(width - 1) {
                    let neighbour = ny * width + nx;
                    if mask[neighbour] && !visited[neighbour] {
                        visited[neighbour] = true;
                        stack.push(neighbour);
                    }
                }
            }
        }
        if count < 8 {
            continue;
        }
        let box_width = (max_x - min_x + 1) as f32;
        let box_height = (max_y - min_y + 1) as f32;
        if box_width.min(box_height) < 3.0 {
            continue;
        }
        if score / (count as f32) < BOX_THRESHOLD {
            continue;
        }
        let distance = box_width * box_height * UNCLIP_RATIO / (2.0 * (box_width + box_height));
        regions.push(Region {
            x0: (min_x as f32 - distance) * scale_x,
            y0: (min_y as f32 - distance) * scale_y,
            x1: (max_x as f32 + 1.0 + distance) * scale_x,
            y1: (max_y as f32 + 1.0 + distance) * scale_y,
        });
    }

    let regions = merge_regions(regions);
    let mut regions: Vec<Region> = regions
        .into_iter()
        .filter_map(|region| region.clamped(image_width, image_height))
        .collect();
    regions.sort_by(|a, b| {
        let line = (a.center_y() - b.center_y()).abs();
        if line < a.height().min(b.height()) * 0.5 {
            a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.center_y()
                .partial_cmp(&b.center_y())
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });
    regions
}

/// 合并同一行里被汉字间隙切开的框：垂直重叠过半且水平间距小于行高。
fn merge_regions(mut regions: Vec<Region>) -> Vec<Region> {
    let mut merged = true;
    while merged {
        merged = false;
        let mut index = 0;
        while index < regions.len() {
            let mut other = index + 1;
            while other < regions.len() {
                if should_merge(&regions[index], &regions[other]) {
                    regions[index] = regions[index].union(&regions[other]);
                    regions.remove(other);
                    merged = true;
                } else {
                    other += 1;
                }
            }
            index += 1;
        }
    }
    regions
}

fn should_merge(a: &Region, b: &Region) -> bool {
    let overlap = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    let min_height = a.height().min(b.height());
    if min_height <= 0.0 || overlap / min_height < 0.6 {
        return false;
    }
    let height_ratio = a.height().max(b.height()) / min_height;
    if height_ratio > 2.0 {
        return false;
    }
    let gap = (a.x0 - b.x1).max(b.x0 - a.x1);
    gap < min_height * 0.8
}

#[derive(Clone, Copy, Debug)]
struct Region {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Region {
    fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    fn center_y(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }

    fn union(&self, other: &Region) -> Region {
        Region {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn clamped(&self, width: u32, height: u32) -> Option<Region> {
        let region = Region {
            x0: self.x0.max(0.0).min(width as f32),
            y0: self.y0.max(0.0).min(height as f32),
            x1: self.x1.max(0.0).min(width as f32),
            y1: self.y1.max(0.0).min(height as f32),
        };
        (region.width() >= 3.0 && region.height() >= 2.0).then_some(region)
    }
}

fn crop(image: &Image, region: Region) -> Option<Image> {
    let x0 = region.x0.floor().max(0.0) as usize;
    let y0 = region.y0.floor().max(0.0) as usize;
    let x1 = (region.x1.ceil() as usize).min(image.width as usize);
    let y1 = (region.y1.ceil() as usize).min(image.height as usize);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let width = x1 - x0;
    let height = y1 - y0;
    let mut bgr = Vec::with_capacity(width * height * 3);
    for y in y0..y1 {
        let start = (y * image.width as usize + x0) * 3;
        bgr.extend_from_slice(&image.bgr[start..start + width * 3]);
    }
    Some(Image {
        bgr,
        width: width as u32,
        height: height as u32,
    })
}

fn rotate_180(image: &mut Image) {
    let stride = image.width as usize * 3;
    let height = image.height as usize;
    let mut flipped = vec![0u8; image.bgr.len()];
    for y in 0..height {
        let source = &image.bgr[y * stride..(y + 1) * stride];
        let target = &mut flipped[(height - 1 - y) * stride..(height - y) * stride];
        for x in 0..image.width as usize {
            target[x * 3] = source[(image.width as usize - 1 - x) * 3];
            target[x * 3 + 1] = source[(image.width as usize - 1 - x) * 3 + 1];
            target[x * 3 + 2] = source[(image.width as usize - 1 - x) * 3 + 2];
        }
    }
    image.bgr = flipped;
}

/// 双线性缩放（BGR，行优先）。
fn resize(image: &Image, width: usize, height: usize) -> Image {
    if width == image.width as usize && height == image.height as usize {
        return Image {
            bgr: image.bgr.clone(),
            width: image.width,
            height: image.height,
        };
    }
    let source_width = image.width.max(1) as usize;
    let source_height = image.height.max(1) as usize;
    let mut bgr = vec![0u8; width * height * 3];
    let scale_x = source_width as f32 / width as f32;
    let scale_y = source_height as f32 / height as f32;
    for y in 0..height {
        let fy = ((y as f32 + 0.5) * scale_y - 0.5).clamp(0.0, source_height as f32 - 1.0);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let wy = fy - y0 as f32;
        for x in 0..width {
            let fx = ((x as f32 + 0.5) * scale_x - 0.5).clamp(0.0, source_width as f32 - 1.0);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let wx = fx - x0 as f32;
            for channel in 0..3 {
                let p00 = image.bgr[(y0 * source_width + x0) * 3 + channel] as f32;
                let p10 = image.bgr[(y0 * source_width + x1) * 3 + channel] as f32;
                let p01 = image.bgr[(y1 * source_width + x0) * 3 + channel] as f32;
                let p11 = image.bgr[(y1 * source_width + x1) * 3 + channel] as f32;
                let top = p00 + (p10 - p00) * wx;
                let bottom = p01 + (p11 - p01) * wx;
                bgr[(y * width + x) * 3 + channel] = (top + (bottom - top) * wy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Image {
        bgr,
        width: width as u32,
        height: height as u32,
    }
}

/// BGR → 归一化 CHW 张量（N=1）。
fn to_tensor_input(image: &Image, mean: [f32; 3], std: [f32; 3]) -> Vec<f32> {
    let width = image.width as usize;
    let height = image.height as usize;
    let mut data = vec![0f32; 3 * width * height];
    fill_normalized(image, &mut data, width, height, width, mean, std);
    data
}

/// 写入 `[3, height, padded_width]` 的归一化数据，右侧补零（cls 需要固定宽度）。
fn fill_normalized(
    image: &Image,
    data: &mut [f32],
    width: usize,
    height: usize,
    padded_width: usize,
    mean: [f32; 3],
    std: [f32; 3],
) {
    for channel in 0..3 {
        let plane = channel * padded_width * height;
        for y in 0..height {
            for x in 0..width.min(padded_width) {
                let value = image.bgr[(y * width + x) * 3 + channel] as f32 / 255.0;
                data[plane + y * padded_width + x] = (value - mean[channel]) / std[channel];
            }
        }
    }
}

/// CTC 贪心解码：跳过 blank、合并相邻重复；`dict.len()` 之后的那一维是空格。
fn decode_ctc(
    values: &[f32],
    timesteps: usize,
    classes: usize,
    dict: &[String],
    _threshold: f32,
) -> String {
    let mut text = String::new();
    let mut previous = usize::MAX;
    for step in 0..timesteps {
        let row = &values[step * classes..step * classes + classes];
        let mut best = 0usize;
        let mut best_score = f32::MIN;
        for (index, score) in row.iter().enumerate() {
            if *score > best_score {
                best_score = *score;
                best = index;
            }
        }
        if best != 0 && best != previous {
            if best <= dict.len() {
                text.push_str(&dict[best - 1]);
            } else {
                text.push(' ');
            }
        }
        previous = best;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_input_is_aligned_to_32_and_capped() {
        assert_eq!(det_size(1920, 1080), (960, 544));
        assert_eq!(det_size(200, 100), (192, 96));
        assert_eq!(det_size(10, 10), (32, 32));
    }

    #[test]
    fn ctc_decoding_skips_blanks_and_repeats() {
        let dict = vec!["a".to_string(), "b".to_string()];
        // blank, a, a, blank, b, space
        let mut values = vec![0f32; 6 * 4];
        for (step, class) in [(0usize, 0usize), (1, 1), (2, 1), (3, 0), (4, 2), (5, 3)] {
            values[step * 4 + class] = 1.0;
        }
        assert_eq!(decode_ctc(&values, 6, 4, &dict, 0.5), "ab ");
    }

    #[test]
    fn vertical_overlap_and_short_gap_merges_boxes() {
        let left = Region {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 20.0,
        };
        let right = Region {
            x0: 12.0,
            y0: 1.0,
            x1: 22.0,
            y1: 21.0,
        };
        let merged = merge_regions(vec![left, right]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].x1, 22.0);
    }

    #[test]
    fn resize_produces_requested_dimensions() {
        let image = Image {
            bgr: vec![255u8; 4 * 2 * 3],
            width: 4,
            height: 2,
        };
        let resized = resize(&image, 8, 4);
        assert_eq!((resized.width, resized.height), (8, 4));
        assert_eq!(resized.bgr.len(), 8 * 4 * 3);
    }

    #[test]
    fn rotation_flips_pixels() {
        let mut image = Image {
            bgr: vec![1, 2, 3, 4, 5, 6],
            width: 2,
            height: 1,
        };
        rotate_180(&mut image);
        assert_eq!(image.bgr, vec![4, 5, 6, 1, 2, 3]);
    }

    /// 真模型自测：需要手动把 4 个资产放到 `BEAD_OCR_DIR`，并给一张原始 BGR 图。
    ///
    /// 默认 `#[ignore]`，因为 CI 和日常 `cargo test` 不下载 35MB 资产；
    /// 升级模型或调整解码映射时用它确认识别文本正确：
    /// `BEAD_OCR_DIR=... BEAD_OCR_BGR=... BEAD_OCR_BGR_SIZE=900x220 cargo test self_test -- --ignored --nocapture`
    #[test]
    #[ignore = "需要手动准备模型目录与 BGR 测试图"]
    fn self_test_with_downloaded_models() {
        let dir = std::env::var("BEAD_OCR_DIR").expect("BEAD_OCR_DIR");
        let fixture = std::env::var("BEAD_OCR_BGR").expect("BEAD_OCR_BGR");
        let size = std::env::var("BEAD_OCR_BGR_SIZE").expect("BEAD_OCR_BGR_SIZE");
        let (width, height) = size.split_once('x').expect("WxH");
        let image = Image {
            bgr: std::fs::read(fixture).expect("读取 BGR 图"),
            width: width.parse().expect("width"),
            height: height.parse().expect("height"),
        };
        let engine = Engine::load(Path::new(&dir)).expect("加载引擎");
        let text = engine.recognize(&image).expect("识别结果");
        println!("=== 识别文本 ===\n{text}\n================");
        assert!(text.contains("Bead"));
        assert!(text.contains("12345"));
    }
}
