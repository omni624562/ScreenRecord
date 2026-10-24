//! 下載 mpv 播放元件（libmpv-2.dll，見 mpv.rs）：設定 → 外部元件，或檢視器播放大影片時的提示。
//!
//! - 鎖定版本：shinchiro 編譯的 mpv-dev（SourceForge 上 mpv 官方列出的 Windows 版本），本程式已用這個版本測試過
//! - SHA-256 內建在程式裡：檔案被調包或下載不完整都會被擋下
//! - 7z 壓縮檔用純 Rust 解開（Windows 10 內建的 tar 解不了 LZMA），只取出 libmpv-2.dll，
//!   放在 exe 旁邊（可寫入時）或 %LOCALAPPDATA%\ScreenRecorder

use crate::{tr, trf};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const MPV_BUILD: &str = "20261011-git-accb7b2821";
/// 壓縮檔裡的 DLL
const DLL_NAME: &str = "libmpv-2.dll";
pub const MPV_SHA256: &str = "cf63b4ce66e03e5e70acf2648cbc3177656c77c5b9583e16d83efc58801182b7";
/// 壓縮檔裡那個 libmpv-2.dll 的 SHA-256：載入前檢查（exe 旁邊被放了別的同名 DLL 不會被載入）
pub const MPV_DLL_SHA256: &str = "e023856d0d2a96c66e75098c290dfd83b66a1c87740bb58152864e53e74f5ffb";
/// 下載的大小（約幾 MB）
pub const MPV_MB: u32 = 32;

pub fn mpv_urls() -> Vec<String> {
    vec![format!("https://sourceforge.net/projects/mpv-player-windows/files/libmpv/mpv-dev-x86_64-{MPV_BUILD}.7z/download")]
}

/// 要安裝到哪裡：exe 旁邊可寫入就放那裡，否則放資料夾（跟 FFmpeg 一樣）
pub fn install_path() -> PathBuf {
    let dir = crate::downloader::download_target().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(crate::paths::data_dir);
    dir.join(crate::mpv::LIB_NAME)
}

/// 已經安裝（下載的或自己放的）
pub fn installed_path() -> Option<PathBuf> {
    crate::mpv::candidates().into_iter().find(|p| p.is_absolute() && p.is_file())
}

/// 下載並安裝（在背景執行緒呼叫）：progress 是千分比，cancel 設成 true 就停止
pub fn download(progress: &AtomicU32, cancel: &AtomicBool) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err(tr!("請用系統的套件管理程式安裝 libmpv（例如 apt install libmpv2）", "Install libmpv with your system's package manager (e.g. apt install libmpv2)").into());
    }
    let target = install_path();
    let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| trf!("無法建立資料夾：{e}", "Couldn't create the folder: {e}"))?;
    let archive = dir.join(format!("mpv-{}.7z.part", std::process::id()));
    let r = download_to(&mpv_urls()[0], &archive, progress, cancel).and_then(|_| install_archive(&archive, &target));
    let _ = std::fs::remove_file(&archive);
    r.map(|_| target)
}

fn download_to(url: &str, out: &Path, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    crate::info!("[外部元件] 下載 mpv 播放元件（{MPV_BUILD}）");
    let resp = crate::http::agent().redirects(8).build().get(url).call().map_err(|e| trf!("下載 mpv 播放元件失敗：{}", "Couldn't download the mpv component: {}", crate::http::describe(&e)))?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(MPV_MB as u64 * 1_000_000);
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(out).map_err(|e| trf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(tr!("已取消下載", "Download canceled").into());
        }
        let n = reader.read(&mut buf).map_err(|e| trf!("下載中斷：{}", "Download interrupted: {}", crate::http::describe_io(&e)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| trf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"))?;
        got += n as u64;
        // 解壓縮時停在 99.9%
        progress.store(((got * 1000) / total.max(1)).min(999) as u32, Ordering::Relaxed);
    }
    let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != MPV_SHA256 {
        return Err(tr!("檔案的 SHA-256 與預期不符（下載不完整或檔案遭竄改），已刪除", "The file's SHA-256 doesn't match (incomplete download or tampered file) and was deleted").into());
    }
    Ok(())
}

/// 從 mpv-dev 的 7z 取出 libmpv-2.dll 放到 target（使用者自己下載的 7z 也走這裡）；也可以直接給 dll
pub fn install_file(src: &Path, target: &Path) -> Result<(), String> {
    let is_dll = src.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll"));
    if is_dll {
        let tmp = target.with_extension("dll.part");
        std::fs::copy(src, &tmp).map_err(|e| crate::error::plain(&e))?;
        return finish(&tmp, target);
    }
    install_archive(src, target)
}

fn install_archive(archive: &Path, target: &Path) -> Result<(), String> {
    let tmp = target.with_extension("dll.part");
    let mut found = false;
    let mut reader = sevenz_rust2::ArchiveReader::new(std::fs::File::open(archive).map_err(|e| e.to_string())?, sevenz_rust2::Password::empty())
        .map_err(|e| trf!("這不是 mpv 的 7z 壓縮檔：{e}", "This isn't an mpv 7z archive: {e}"))?;
    reader
        .for_each_entries(|entry, data| {
            let name = entry.name().rsplit(['/', '\\']).next().unwrap_or("");
            if entry.is_directory() || !name.eq_ignore_ascii_case(DLL_NAME) {
                // 其他檔案（標頭檔、說明）略過：照樣要讀過去
                std::io::copy(data, &mut std::io::sink())?;
                return Ok(true);
            }
            let mut out = std::fs::File::create(&tmp)?;
            std::io::copy(data, &mut out)?;
            found = true;
            Ok(false)
        })
        .map_err(|e| trf!("解壓縮失敗：{e}", "Couldn't extract the archive: {e}"))?;
    if !found {
        return Err(trf!("壓縮檔裡沒有 {}", "{} isn't in the archive", DLL_NAME));
    }
    finish(&tmp, target)
}

pub(crate) fn file_sha256(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 使用者自己選來安裝的其他版本（SHA-256 一行一個）：放在資料夾裡，不在 exe 旁邊
fn allowed_list() -> PathBuf {
    crate::paths::data_dir().join("mpv-allowed.txt")
}

/// 可以載入這個 DLL 嗎：本程式下載的版本，或使用者在「外部元件」自己選來安裝的檔案。
/// Windows 以外用系統的 libmpv，不檢查
pub fn dll_trusted(p: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let Some(h) = file_sha256(p) else { return false };
    h == MPV_DLL_SHA256 || std::fs::read_to_string(allowed_list()).is_ok_and(|t| t.lines().any(|l| l.trim() == h))
}

/// 使用者自己選的檔案：記下來，之後載入時認得
fn allow(p: &Path) {
    let Some(h) = file_sha256(p).filter(|h| h != MPV_DLL_SHA256) else { return };
    let list = allowed_list();
    let mut text = std::fs::read_to_string(&list).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == h) {
        text.push_str(&h);
        text.push('\n');
        let _ = crate::paths::write_atomic(&list, text.as_bytes());
    }
}

/// 確認是 Windows 的 DLL 再放到定位
fn finish(tmp: &Path, target: &Path) -> Result<(), String> {
    let mut head = [0u8; 2];
    let ok = std::fs::File::open(tmp).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"MZ" && std::fs::metadata(tmp).is_ok_and(|m| m.len() > 1_000_000);
    if !ok && cfg!(windows) {
        let _ = std::fs::remove_file(tmp);
        return Err(trf!("{} 不完整或不是 Windows 的 DLL", "{} is incomplete or not a Windows DLL", crate::mpv::LIB_NAME));
    }
    let _ = std::fs::remove_file(target);
    std::fs::rename(tmp, target).map_err(|e| trf!("無法儲存 {}：{e}", "Couldn't save {}: {e}", crate::mpv::LIB_NAME))?;
    if cfg!(windows) {
        allow(target);
    }
    crate::info!("[外部元件] 已安裝 {}", target.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 實際的 mpv-dev 7z（SCREENRECORDER_TEST_MPV_7Z 指定；沒有時略過）
    #[test]
    fn extracts_the_dll_from_the_real_archive() {
        let Ok(path) = std::env::var("SCREENRECORDER_TEST_MPV_7Z") else { return };
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(crate::mpv::LIB_NAME);
        install_file(Path::new(&path), &target).unwrap();
        let mut head = [0u8; 2];
        std::fs::File::open(&target).unwrap().read_exact(&mut head).unwrap();
        assert_eq!(&head, b"MZ");
        assert!(std::fs::metadata(&target).unwrap().len() > 10_000_000);
        assert!(std::fs::read_dir(dir.path()).unwrap().count() == 1, "暫存檔已清掉");
    }

    #[test]
    fn rejects_files_that_are_not_archives() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("x.7z");
        std::fs::write(&bad, b"not a 7z").unwrap();
        assert!(install_file(&bad, &dir.path().join("out.dll")).is_err());
    }
}
