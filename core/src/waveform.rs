//! 聲音波形：剪輯視窗的時間軸下方畫出聲音大小，一眼看出哪裡在講話、哪裡沒聲音。
//! 用 FFmpeg 把聲音轉成單聲道、低取樣率的原始資料，每一小段取最大音量。

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

/// 解碼的取樣率（只看大小，不用好音質）
const RATE: u32 = 4000;
/// 每秒幾個值
pub const PER_SEC: u32 = 50;

/// 每一小段的音量（0～255，用平方根放大小聲的部分）；沒有聲音軌或失敗時 None
pub fn load(ffmpeg: &Path, path: &str, cancel: &AtomicBool) -> Option<Vec<u8>> {
    let mut child = crate::process::std_command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i", path, "-vn", "-map", "0:a:0", "-ac", "1", "-ar", &RATE.to_string(), "-f", "s16le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let bucket = (RATE / PER_SEC) as usize;
    let mut peaks = Vec::new();
    let (mut cur, mut n) = (0i32, 0usize);
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Option<u8> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let got = match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(k) => k,
        };
        let mut bytes: Vec<u8> = Vec::with_capacity(got + 1);
        bytes.extend(carry.take());
        bytes.extend_from_slice(&buf[..got]);
        if bytes.len() % 2 == 1 {
            carry = bytes.pop();
        }
        for s in bytes.chunks_exact(2) {
            let v = (i16::from_le_bytes([s[0], s[1]]) as i32).abs();
            cur = cur.max(v);
            n += 1;
            if n == bucket {
                peaks.push(level(cur));
                (cur, n) = (0, 0);
            }
        }
    }
    if n > 0 {
        peaks.push(level(cur));
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    normalize(&mut peaks);
    (ok && !peaks.is_empty()).then_some(peaks)
}

/// 放大到最大聲的地方是 255：麥克風比較小聲的錄影也看得清楚（全部很小聲時不放大，避免把雜音放大成講話）
pub fn normalize(peaks: &mut [u8]) {
    let max = peaks.iter().copied().max().unwrap_or(0);
    if max < 40 || max == 255 {
        return;
    }
    for p in peaks.iter_mut() {
        *p = ((*p as u32 * 255) / max as u32) as u8;
    }
}

/// 最大值（0～32767）→ 0～255，用平方根讓小聲的講話也看得到
pub fn level(peak: i32) -> u8 {
    ((peak.clamp(0, 32767) as f64 / 32767.0).sqrt() * 255.0).round() as u8
}

/// a～b 秒之間最大的音量（0～1）
pub fn max_in(peaks: &[u8], a: f64, b: f64) -> f32 {
    let i0 = (a * PER_SEC as f64).floor().max(0.0) as usize;
    let i1 = ((b * PER_SEC as f64).ceil().max(0.0) as usize).max(i0 + 1);
    let slice = &peaks[i0.min(peaks.len())..i1.min(peaks.len())];
    slice.iter().copied().max().unwrap_or(0) as f32 / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_ranges() {
        assert_eq!(level(0), 0);
        assert_eq!(level(32767), 255);
        // 四分之一的音量顯示成一半高
        assert_eq!(level(8192), 128);
        let p = vec![0u8, 0, 255, 0, 51];
        assert_eq!(max_in(&p, 0.0, 0.03), 0.0);
        assert_eq!(max_in(&p, 0.0, 0.05), 1.0);
        assert!((max_in(&p, 0.08, 0.1) - 0.2).abs() < 1e-6);
        assert_eq!(max_in(&p, 5.0, 6.0), 0.0);
        assert_eq!(max_in(&[], 0.0, 1.0), 0.0);
        let mut v = vec![0u8, 60, 120];
        normalize(&mut v);
        assert_eq!(v, vec![0, 127, 255]);
        let mut quiet = vec![0u8, 10, 20];
        normalize(&mut quiet);
        assert_eq!(quiet, vec![0, 10, 20]);
    }

    #[test]
    fn reads_a_real_file_when_ffmpeg_is_available() {
        let Some(ffmpeg) = which("ffmpeg") else { return };
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.m4a");
        // 前 1 秒安靜、後 1 秒大聲
        let ok = std::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=44100:cl=mono:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:d=1:r=44100",
                "-filter_complex",
                "[0][1]concat=n=2:v=0:a=1",
            ])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let p = load(&ffmpeg, &f.display().to_string(), &AtomicBool::new(false)).unwrap();
        assert!((95..=110).contains(&p.len()), "{}", p.len());
        assert!(max_in(&p, 0.1, 0.8) < 0.05);
        assert!(max_in(&p, 1.2, 1.8) > 0.9);
    }

    fn which(name: &str) -> Option<std::path::PathBuf> {
        std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
    }
}
