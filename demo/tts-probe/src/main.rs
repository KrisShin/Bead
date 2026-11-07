//! OneCore（WinRT `Windows.Media.SpeechSynthesis`）探针。
//!
//! 只回答四个问题，不改主程序：
//! 1) `SpeechSynthesizer::AllVoices()` 能否枚举出音色，能否看到 Natural Voices；
//! 2) 能否对 zh / en / ja 合成音频；
//! 3) 从调用到流可用、再到读全字节的耗时；
//! 4) 产物的 ContentType 与体积（决定主程序能否直接落盘给 `<audio>` 播放）。
//!
//! 输出为一行行 JSON，便于人工比对；音频留在 `%TEMP%\bead-tts-probe\`，不自动清理。

use std::{fs, path::PathBuf, time::Instant};

use windows::core::HSTRING;
use windows::Media::SpeechSynthesis::SpeechSynthesizer;
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

const SAMPLES: [(&str, &str); 3] = [
    ("zh", "你好，这是一段用于验证 OneCore 朗读的中文文本。"),
    ("en", "Hello, this is a probe for OneCore speech synthesis."),
    ("ja", "こんにちは、これは OneCore の音声合成を確認する日本語の文章です。"),
];

fn out_dir() -> PathBuf {
    std::env::temp_dir().join("bead-tts-probe")
}

fn main() {
    // 探针不能卡住：60 秒还没跑完就直接退出，并留下可判定的错误行。
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(60));
        println!("{{\"probe\":\"onecore\",\"error\":\"watchdog_timeout\"}}");
        std::process::exit(3);
    });

    match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
        Ok(()) => println!("{{\"probe\":\"onecore\",\"phase\":\"init\",\"ok\":true}}"),
        Err(error) => {
            // RPC_E_CHANGED_MODE：当前线程已以别的套间模型初始化，WinRT 仍可用。
            let code = error.code().0 as u32;
            let already = code == 0x8001_0106;
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"init\",\"ok\":{},\"already_initialized\":{},\"code\":\"0x{:08X}\",\"message\":\"{}\"}}",
                already,
                already,
                code,
                json_escape(&error.message())
            );
            if !already {
                return;
            }
        }
    }

    let voices = match enumerate_voices() {
        Ok(voices) => voices,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"voices\",\"ok\":false,\"error\":\"{}\"}}",
                json_escape(&error)
            );
            return;
        }
    };
    let natural_count = voices
        .iter()
        .filter(|voice| voice.display.to_lowercase().contains("natural"))
        .count();
    println!(
        "{{\"probe\":\"onecore\",\"phase\":\"voices\",\"ok\":true,\"count\":{},\"natural_count\":{},\"voices\":[{}]}}",
        voices.len(),
        natural_count,
        voices
            .iter()
            .map(|voice| format!(
                "{{\"display\":\"{}\",\"language\":\"{}\",\"id\":\"{}\",\"gender\":{}}}",
                json_escape(&voice.display),
                json_escape(&voice.language),
                json_escape(&voice.id),
                voice.gender
            ))
            .collect::<Vec<_>>()
            .join(",")
    );

    let dir = out_dir();
    if let Err(error) = fs::create_dir_all(&dir) {
        println!(
            "{{\"probe\":\"onecore\",\"phase\":\"outdir\",\"ok\":false,\"error\":\"{}\"}}",
            json_escape(&error.to_string())
        );
        return;
    }

    for (lang, text) in SAMPLES {
        let matched = voices
            .iter()
            .find(|voice| voice.language.to_lowercase().starts_with(lang));
        let Some(voice) = matched else {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"error\":\"no_voice_for_language\",\"text_chars\":{}}}",
                lang,
                text.chars().count()
            );
            continue;
        };
        synthesize(lang, text, voice, &dir);
    }
}

struct ProbeVoice {
    display: String,
    language: String,
    id: String,
    gender: i32,
}

fn enumerate_voices() -> Result<Vec<ProbeVoice>, String> {
    let voices = SpeechSynthesizer::AllVoices().map_err(|error| error.message())?;
    let mut collected = Vec::new();
    for voice in &voices {
        collected.push(ProbeVoice {
            display: voice.DisplayName().map(|v| v.to_string()).unwrap_or_default(),
            language: voice.Language().map(|v| v.to_string()).unwrap_or_default(),
            id: voice.Id().map(|v| v.to_string()).unwrap_or_default(),
            gender: voice.Gender().map(|v| v.0).unwrap_or(-1),
        });
    }
    Ok(collected)
}

fn synthesize(lang: &str, text: &str, voice: &ProbeVoice, dir: &PathBuf) {
    let started = Instant::now();
    let synthesizer = match SpeechSynthesizer::new() {
        Ok(value) => value,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"error\":\"{}\"}}",
                lang,
                json_escape(&error.message())
            );
            return;
        }
    };
    // 重新按 language 找到 WinRT 的对象，避免把探针结构体当接口传进去。
    let voices = match SpeechSynthesizer::AllVoices() {
        Ok(value) => value,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"error\":\"{}\"}}",
                lang,
                json_escape(&error.message())
            );
            return;
        }
    };
    let selected = voices
        .into_iter()
        .find(|candidate| candidate.Id().map(|id| id.to_string()).unwrap_or_default() == voice.id);
    let Some(selected) = selected else {
        println!(
            "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"error\":\"voice_disappeared\"}}",
            lang
        );
        return;
    };
    if let Err(error) = synthesizer.SetVoice(&selected) {
        println!(
            "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"error\":\"set_voice: {}\"}}",
            lang,
            json_escape(&error.message())
        );
        return;
    }

    let operation = match synthesizer.SynthesizeTextToStreamAsync(&HSTRING::from(text)) {
        Ok(value) => value,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"error\":\"{}\"}}",
                lang,
                json_escape(&error.message())
            );
            return;
        }
    };
    let stream = match operation.join() {
        Ok(value) => value,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"stream_ready_ms\":{},\"error\":\"{}\"}}",
                lang,
                started.elapsed().as_millis(),
                json_escape(&error.message())
            );
            return;
        }
    };
    let stream_ready_ms = started.elapsed().as_millis();
    let size = match stream.Size() {
        Ok(value) => value,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"stream_ready_ms\":{},\"error\":\"{}\"}}",
                lang,
                stream_ready_ms,
                json_escape(&error.message())
            );
            return;
        }
    };
    let content_type = stream
        .ContentType()
        .map(|value| value.to_string())
        .unwrap_or_default();

    let read_started = Instant::now();
    let read = (|| -> windows::core::Result<(u32, Vec<u8>)> {
        let input = stream.GetInputStreamAt(0)?;
        let reader = DataReader::CreateDataReader(&input)?;
        let loaded = reader.LoadAsync(size as u32)?.join()?;
        let mut buffer = vec![0u8; size as usize];
        reader.ReadBytes(&mut buffer[..loaded as usize])?;
        Ok((loaded, buffer))
    })();
    let read_ms = read_started.elapsed().as_millis();

    let (loaded, buffer) = match read {
        Ok(value) => value,
        Err(error) => {
            println!(
                "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":false,\"stream_ready_ms\":{},\"bytes\":{},\"content_type\":\"{}\",\"error\":\"{}\"}}",
                lang,
                stream_ready_ms,
                size,
                json_escape(&content_type),
                json_escape(&error.message())
            );
            return;
        }
    };

    let ext = if content_type.contains("mpeg") || content_type.contains("mp3") {
        "mp3"
    } else if content_type.contains("wav") {
        "wav"
    } else {
        "bin"
    };
    let path = dir.join(format!("{lang}.{ext}"));
    let write_ok = fs::write(&path, &buffer[..loaded as usize]).is_ok();
    println!(
        "{{\"probe\":\"onecore\",\"phase\":\"synth\",\"lang\":\"{}\",\"ok\":{},\"voice\":\"{}\",\"text_chars\":{},\"stream_ready_ms\":{},\"read_ms\":{},\"bytes\":{},\"content_type\":\"{}\",\"write_ok\":{},\"file\":\"{}\"}}",
        lang,
        write_ok,
        json_escape(&voice.display),
        text.chars().count(),
        stream_ready_ms,
        read_ms,
        size,
        json_escape(&content_type),
        write_ok,
        json_escape(&path.display().to_string())
    );
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
