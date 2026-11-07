//! Windows 本地朗读：SAPI5（System.Speech）子进程。
//!
//! 为什么用 PowerShell 而不是新增 crate：SAPI5 通过 System.Speech 已随 Windows 提供，
//! 零依赖、零包体；代价是每次朗读多一次进程启动（几百毫秒）。子进程可被 kill，
//! 因此支持「新请求抢占旧朗读」；长文本写入临时文件再读，避开命令行长度与引号转义。
//!
//! 音色由上层 `bead_core::choose_voice` 按 language 选出；这里只负责在给定音色/语速/
//! 音量下发声，不猜语言、不降级到其他语言音色。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use bead_core::VoiceInfo;

static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);

/// 一次正在进行的本地朗读。`stop` 会直接 kill PowerShell 进程，音频随之中断。
pub struct LocalPlayback {
    pub child: Child,
    pub text_path: PathBuf,
}

impl LocalPlayback {
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.text_path);
    }

    /// 子进程是否已自行结束（不阻塞）。`Some(0)` 表示正常读完；非零表示 PowerShell/SAPI 失败，
    /// 必须上报而不是当成「播放完成」——否则合成失败会被静默吞掉。
    pub fn finished(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }
}

impl Drop for LocalPlayback {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.text_path);
    }
}

/// 枚举 SAPI5 音色。返回 `(Name, Culture.Name, Enabled)`；失败如实上报，不伪造空列表。
#[cfg(windows)]
pub fn list_voices() -> Result<Vec<VoiceInfo>, String> {
    let script = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$items = @()
foreach ($voice in $synth.GetInstalledVoices()) {
  $items += [pscustomobject]@{
    id = $voice.VoiceInfo.Name
    culture = $voice.VoiceInfo.Culture.Name
    enabled = [bool]$voice.Enabled
  }
}
$synth.Dispose()
if ($items.Count -eq 0) { Write-Output '[]' } else { $items | ConvertTo-Json -Compress -Depth 3 }
"#;
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("无法启动系统语音服务：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("系统语音服务返回错误：{}", stderr.trim()));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    parse_voices(body.trim())
}

#[cfg(windows)]
fn parse_voices(body: &str) -> Result<Vec<VoiceInfo>, String> {
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("系统语音列表解析失败：{error}"))?;
    let entries: Vec<&serde_json::Value> = match &value {
        serde_json::Value::Array(items) => items.iter().collect(),
        serde_json::Value::Object(_) => vec![&value],
        _ => return Err("系统语音列表格式异常".into()),
    };
    Ok(entries
        .into_iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let culture = entry.get("culture")?.as_str()?.to_string();
            let enabled = entry.get("enabled")?.as_bool().unwrap_or(true);
            Some(VoiceInfo {
                id,
                culture,
                enabled,
            })
        })
        .collect())
}

/// 用指定音色朗读整段文本。长文本走临时文件，不拼接进命令行。
#[cfg(windows)]
pub fn speak(
    text: &str,
    voice: &VoiceInfo,
    rate: i32,
    volume: u8,
) -> Result<LocalPlayback, String> {
    let text_path = write_text_file(text)?;
    let script = build_script(&text_path, voice, rate, volume);
    let child = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    }
    .map_err(|error| {
        let _ = std::fs::remove_file(&text_path);
        format!("启动系统语音失败：{error}")
    })?;
    Ok(LocalPlayback { child, text_path })
}

#[cfg(windows)]
fn write_text_file(text: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("bead-tts");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建朗读缓存目录失败：{error}"))?;
    let path = dir.join(format!(
        "speak-{}-{}.txt",
        std::process::id(),
        SCRIPT_SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&path, text).map_err(|error| format!("写入朗读文本失败：{error}"))?;
    Ok(path)
}

#[cfg(windows)]
fn build_script(text_path: &PathBuf, voice: &VoiceInfo, rate: i32, volume: u8) -> String {
    let path = ps_quote(&text_path.to_string_lossy());
    let voice_id = ps_quote(&voice.id);
    let culture = ps_quote(&voice.culture);
    let rate = rate.clamp(-10, 10);
    let volume = volume.min(100);
    format!(
        r#"$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$synth.Rate = {rate}
$synth.Volume = {volume}
$voice = '{voice_id}'
$culture = '{culture}'
if ($voice -ne '') {{
  try {{ $synth.SelectVoice($voice) }} catch {{
    try {{ $synth.SelectVoiceByHints([System.Speech.Synthesis.VoiceGender]::NotSet, [System.Speech.Synthesis.VoiceAge]::NotSet, 0, (New-Object System.Globalization.CultureInfo($culture))) }} catch {{}}
  }}
}} elseif ($culture -ne '') {{
  try {{ $synth.SelectVoiceByHints([System.Speech.Synthesis.VoiceGender]::NotSet, [System.Speech.Synthesis.VoiceAge]::NotSet, 0, (New-Object System.Globalization.CultureInfo($culture))) }} catch {{}}
}}
$text = [System.IO.File]::ReadAllText('{path}', [System.Text.Encoding]::UTF8)
$synth.Speak($text)
$synth.Dispose()
"#
    )
}

#[cfg(windows)]
fn ps_quote(value: &str) -> String {
    value.replace('\'', "''")
}

/// 1.0× → SAPI 的 0；0.5× → -5；2.0× → 10。SAPI 的整数档位有限，按近似映射。
pub fn rate_to_sapi(factor: f64) -> i32 {
    let factor = factor.clamp(0.5, 2.0);
    (((factor - 1.0) * 10.0).round() as i32).clamp(-10, 10)
}

#[cfg(not(windows))]
pub fn list_voices() -> Result<Vec<VoiceInfo>, String> {
    Err("当前平台暂未接入本地朗读".into())
}

#[cfg(not(windows))]
pub fn speak(
    _text: &str,
    _voice: &VoiceInfo,
    _rate: i32,
    _volume: u8,
) -> Result<LocalPlayback, String> {
    Err("当前平台暂未接入本地朗读".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn rate_maps_to_sapi_steps() {
        assert_eq!(rate_to_sapi(1.0), 0);
        assert_eq!(rate_to_sapi(2.0), 10);
        assert_eq!(rate_to_sapi(0.5), -5);
        assert_eq!(rate_to_sapi(3.0), 10);
        assert_eq!(rate_to_sapi(0.1), -5);
    }

    #[test]
    fn single_quotes_are_escaped_for_powershell() {
        assert_eq!(ps_quote("it's"), "it''s");
    }

    #[test]
    fn voices_json_accepts_array_and_single_object() {
        let array = r#"[{"id":"A","culture":"zh-CN","enabled":true},{"id":"B","culture":"en-US","enabled":false}]"#;
        assert_eq!(parse_voices(array).unwrap().len(), 2);
        let single = r#"{"id":"A","culture":"zh-CN","enabled":true}"#;
        assert_eq!(parse_voices(single).unwrap()[0].id, "A");
        assert!(parse_voices("[]").unwrap().is_empty());
    }
}
