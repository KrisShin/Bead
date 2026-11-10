//! 模型下载器：按清单顺序、逐文件从多个源回退，落盘前做 SHA256 校验。
//!
//! 不做续传：35MB 在国内直连源约 20 秒下完，为断点续传引入分片协议得不偿失。
//! 不引入对象存储：资产已经托管在魔搭 / Gitee Release / GitHub Release。
//! 先写 `.part` 再改名，半截文件永远不会被当成已安装。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::manifest::{Asset, Manifest};

/// 进度阶段：仅供设置页内联展示，不弹任何提示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Downloading,
    Verifying,
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Downloading => "downloading",
            Phase::Verifying => "verifying",
            Phase::Done => "done",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    /// 当前文件名；`Done` 阶段为空。
    pub file: String,
    /// 当前文件序号（从 0 开始）与总数。
    pub index: usize,
    pub count: usize,
    /// 当前文件已收字节与总体已收字节。
    pub received: u64,
    pub overall: u64,
    pub total: u64,
}

/// 安装到 `dir`：已存在且尺寸正确的文件直接跳过（重复安装不会重复下载）。
pub fn install(
    dir: &Path,
    manifest: &Manifest,
    report: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|error| format!("创建目录失败：{error}"))?;
    let total = manifest.total_size();
    let count = manifest.files.len();
    let mut overall = 0u64;

    for (index, asset) in manifest.files.iter().enumerate() {
        let target = dir.join(&asset.name);
        if file_matches(&target, asset.size, &asset.sha256) {
            overall += asset.size;
            report(Progress {
                phase: Phase::Verifying,
                file: asset.name.clone(),
                index,
                count,
                received: asset.size,
                overall,
                total,
            });
            continue;
        }
        download_asset(asset, &target, Job { index, count, overall, total }, report)?;
        overall += asset.size;
    }

    // 清单本身作为安装标记：下次启动只按尺寸复核，不重算 35MB 哈希。
    let marker = serde_json::to_vec_pretty(manifest).map_err(|error| format!("清单序列化失败：{error}"))?;
    fs::write(dir.join("installed.json"), marker).map_err(|error| format!("写入安装标记失败：{error}"))?;
    report(Progress {
        phase: Phase::Done,
        file: String::new(),
        index: count,
        count,
        received: 0,
        overall: total,
        total,
    });
    Ok(())
}

/// 当前批次位置与总进度，避免在各层函数里重复传四个标量。
#[derive(Clone, Copy)]
struct Job {
    index: usize,
    count: usize,
    overall: u64,
    total: u64,
}

fn download_asset(
    asset: &Asset,
    target: &Path,
    job: Job,
    report: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let mut last_error = String::from("清单里没有可用下载源");
    for source in &asset.sources {
        match download_from(source, asset, target, job, report) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(format!("{} 下载失败：{last_error}", asset.name))
}

fn download_from(
    source: &str,
    asset: &Asset,
    target: &Path,
    job: Job,
    report: &mut dyn FnMut(Progress),
) -> Result<(), String> {
    let response = ureq::get(source)
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|error| error.to_string())?;
    let mut reader = response.into_reader();

    let part = part_path(target);
    let mut file = File::create(&part).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut received = 0u64;
    let mut last_report = Instant::now();

    loop {
        let read = reader.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
        hasher.update(&buffer[..read]);
        received += read as u64;
        if last_report.elapsed() >= Duration::from_millis(120) {
            last_report = Instant::now();
            report(Progress {
                phase: Phase::Downloading,
                file: asset.name.clone(),
                index: job.index,
                count: job.count,
                received,
                overall: job.overall + received,
                total: job.total,
            });
        }
    }
    file.flush().map_err(|error| error.to_string())?;
    drop(file);

    report(Progress {
        phase: Phase::Verifying,
        file: asset.name.clone(),
        index: job.index,
        count: job.count,
        received,
        overall: job.overall + received,
        total: job.total,
    });

    if received != asset.size {
        let _ = fs::remove_file(&part);
        return Err(format!("体积不符（期望 {}，实际 {received}）", asset.size));
    }
    let digest = hex::encode(hasher.finalize());
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        let _ = fs::remove_file(&part);
        return Err("SHA256 校验失败".into());
    }
    fs::rename(&part, target).map_err(|error| error.to_string())?;
    Ok(())
}

fn file_matches(path: &Path, size: u64, sha256: &str) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() != size {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(_) => return false,
        }
    }
    hex::encode(hasher.finalize()).eq_ignore_ascii_case(sha256)
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_files_keep_the_original_extension_visible() {
        let path = part_path(Path::new("C:/tmp/model.onnx"));
        assert_eq!(path.file_name().unwrap(), "model.onnx.part");
    }

    #[test]
    fn size_mismatch_is_not_a_match() {
        let dir = std::env::temp_dir();
        let path = dir.join("bead-ocr-download-test.bin");
        fs::write(&path, b"abc").unwrap();
        assert!(!file_matches(&path, 4, &"0".repeat(64)));
        let _ = fs::remove_file(path);
    }
}
