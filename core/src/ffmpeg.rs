//! 尋找 ffmpeg.exe 並偵測功能：ddagrab、gdigrab、H.264 編碼器。

use crate::args::{encoder_spec, EncoderSpec, ENCODERS, HARDWARE_ENCODERS};
use crate::paths::{app_dir, data_dir};
use crate::process::{last_lines, run};
use crate::types::{FfmpegInfo, MonitorInfo};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

const EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

/// 在 PATH 裡找執行檔
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// 依序尋找：程式資料夾 → 程式資料夾\ffmpeg\bin → 程式資料夾\bin → %LOCALAPPDATA%\ScreenRecorder（自動下載的位置）→ PATH
pub fn locate_ffmpeg() -> (Option<PathBuf>, Vec<String>) {
    let app = app_dir();
    let local = [app.join(EXE), app.join("ffmpeg").join("bin").join(EXE), app.join("bin").join(EXE), data_dir().join(EXE)];
    let mut searched: Vec<String> = local.iter().map(|p| p.display().to_string()).collect();
    if let Some(p) = local.iter().find(|p| p.is_file()) {
        return (Some(p.clone()), searched);
    }
    searched.push("PATH".into());
    (which(EXE), searched)
}

/// 實際試編一小段：FFmpeg 列得出來不代表能用（例如沒有 NVENC 的顯示卡、沒有 Intel 內顯就不能用 QSV）
pub async fn encoder_works(ffmpeg: &Path, enc: &EncoderSpec) -> bool {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=0.5", "-vf"].iter().map(|s| s.to_string()).collect();
    a.push(format!("format={}", enc.pix_fmt));
    a.extend(enc.live());
    a.extend(["-f".into(), "null".into(), "-".into()]);
    run(ffmpeg, &a, Duration::from_secs(15)).await.code == 0
}

/// 實測硬體編碼器，依 HARDWARE_ENCODERS 的順序回傳可用的名稱
pub async fn test_hw_encoders(ffmpeg: &Path, listed: &[String]) -> Vec<String> {
    let mut ok = Vec::new();
    for name in HARDWARE_ENCODERS {
        if let Some(spec) = encoder_spec(name) {
            if listed.iter().any(|l| l == name) && encoder_works(ffmpeg, &spec).await {
                ok.push(name.to_string());
            }
        }
    }
    ok
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ffmpeg version (\S+)").unwrap());
static DDAGRAB_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\S+\s+ddagrab\s").unwrap());
static GDIGRAB_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*D\S*\s+gdigrab\s").unwrap());

pub fn parse_version(stdout: &str) -> Option<String> {
    VERSION_RE.captures(stdout).map(|c| c[1].to_string())
}

/// 從 `ffmpeg -encoders` 的輸出找出列出的 H.264 編碼器（依 ENCODERS 的順序）
pub fn listed_encoders(encoders_stdout: &str) -> Vec<EncoderSpec> {
    ENCODERS
        .iter()
        .copied()
        .filter(|e| Regex::new(&format!(r"(?m)^\s*V\S*\s+{}\s", regex::escape(e.name))).unwrap().is_match(encoders_stdout))
        .collect()
}

/// 偵測結果：介面顯示用的 FfmpegInfo，加上程式內部要用的編碼器與列出的硬體編碼器
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub info: FfmpegInfo,
    pub encoder: Option<EncoderSpec>,
    pub hw_listed: Vec<String>,
}

pub async fn probe_ffmpeg() -> Probe {
    let (path, searched) = locate_ffmpeg();
    let base = FfmpegInfo { searched, ..Default::default() };
    let Some(path) = path else { return Probe { info: base, ..Default::default() } };
    let path_str = path.display().to_string();
    let t = Duration::from_secs(20);
    let (ver, filters, devices, encoders) = tokio::join!(
        run(&path, &["-hide_banner", "-version"], t),
        run(&path, &["-hide_banner", "-filters"], t),
        run(&path, &["-hide_banner", "-devices"], t),
        run(&path, &["-hide_banner", "-encoders"], t),
    );
    if ver.code != 0 {
        let err = format!("無法執行 ffmpeg：{}", last_lines(&ver.stderr, 3));
        return Probe { info: FfmpegInfo { path: Some(path_str), error: Some(err), ..base }, ..Default::default() };
    }
    let version = parse_version(&ver.stdout).unwrap_or_else(|| "unknown".into());
    let listed = listed_encoders(&encoders.stdout);
    // libx264 品質最好且不吃顯示卡；沒有時才實測硬體 / MediaFoundation 編碼器
    let mut encoder = listed.iter().copied().find(|e| e.name == "libx264");
    if encoder.is_none() {
        for e in &listed {
            if encoder_works(&path, e).await {
                encoder = Some(*e);
                break;
            }
        }
    }
    let hw_listed = listed.iter().filter(|e| HARDWARE_ENCODERS.contains(&e.name)).map(|e| e.name.to_string()).collect();
    Probe {
        info: FfmpegInfo {
            found: true,
            path: Some(path_str),
            version: Some(version),
            has_ddagrab: DDAGRAB_RE.is_match(&filters.stdout),
            has_gdigrab: GDIGRAB_RE.is_match(&devices.stdout),
            encoder: encoder.map(|e| e.name.to_string()),
            error: encoder.is_none().then(|| "這個 FFmpeg 沒有可用的 H.264 編碼器（建議改用 gyan.dev 的 full / essentials 版本）".to_string()),
            ..base
        },
        encoder,
        hw_listed,
    }
}

/// 實際用 ddagrab 抓一張畫面，確認 Desktop Duplication 在這台電腦可用
pub async fn test_ddagrab(ffmpeg: &Path, monitor: Option<&MonitorInfo>) -> Result<(), String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error"].iter().map(|s| s.to_string()).collect();
    if let Some(m) = monitor {
        a.extend(["-init_hw_device".into(), format!("d3d11va=dda:{}", m.adapter), "-filter_hw_device".into(), "dda".into()]);
    }
    a.push("-filter_complex".into());
    a.push(format!("ddagrab=output_idx={}:framerate=5,hwdownload,format=bgra", monitor.map(|m| m.output).unwrap_or(0)));
    a.extend(["-frames:v", "1", "-f", "null", "-"].iter().map(|s| s.to_string()));
    let r = run(ffmpeg, &a, Duration::from_secs(15)).await;
    if r.code == 0 && !r.timed_out {
        return Ok(());
    }
    Err(if r.timed_out {
        "測試逾時".into()
    } else {
        let l = last_lines(&r.stderr, 3);
        if l.is_empty() { format!("exit {}", r.code) } else { l }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffmpeg_listings() {
        assert_eq!(parse_version("ffmpeg version 9.0.2-essentials_build-www.gyan.dev Copyright").as_deref(), Some("9.0.2-essentials_build-www.gyan.dev"));
        let enc = " V....D libx264              libx264 H.264 / AVC\n V....D h264_qsv             H.264 (Intel Quick Sync)\n A....D aac                  AAC\n";
        let names: Vec<_> = listed_encoders(enc).iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["libx264", "h264_qsv"]);
        assert!(DDAGRAB_RE.is_match(" ... ddagrab           |->V       Grab Windows Desktop images\n"));
        assert!(GDIGRAB_RE.is_match(" D  gdigrab         GDI API Windows frame grabber\n"));
        assert!(!GDIGRAB_RE.is_match(" E  sdl,sdl2        SDL2 output device\n"));
    }

    #[test]
    fn which_finds_programs_on_path() {
        if cfg!(unix) {
            assert!(which("sh").is_some());
        }
        assert!(which("definitely-not-a-program-xyz").is_none());
    }
}
