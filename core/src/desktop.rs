//! 與 Windows 桌面整合：
//! 開機自動啟動、用檔案總管開啟檔案 / 網址。

use crate::process::run;
use std::path::Path;
use std::time::Duration;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// 1.x、2.x 也用同一個名稱：換了資料夾放新版時，開機仍會啟動舊位置的程式（見 repoint_autostart）
const RUN_NAME: &str = "ScreenRecorder";

/// reg.exe 用完整路徑（不會執行到放在本程式資料夾裡的同名程式）
fn reg() -> String {
    crate::paths::system32("reg.exe").display().to_string()
}

/// 開發版（debug build）不設定開機自動啟動
const IS_RELEASE: bool = !cfg!(debug_assertions);

/// 開機自動啟動：寫入目前使用者的 Run 機碼（啟動時只常駐系統匣，不開視窗）。開發版回傳 None
pub async fn get_autostart() -> Option<bool> {
    if !IS_RELEASE || !cfg!(windows) {
        return None;
    }
    Some(run(&reg(), &["query", RUN_KEY, "/v", RUN_NAME], Duration::from_secs(5)).await.code == 0)
}

pub async fn set_autostart(on: bool) -> crate::Result<()> {
    if !IS_RELEASE || !cfg!(windows) {
        return Err(crate::Error::config(crate::tr!("開發模式無法設定開機自動啟動", "Start with Windows can't be set in development mode")));
    }
    let exe = std::env::current_exe()?;
    let value = format!("\"{}\" --tray", exe.display());
    let r = if on {
        run(&reg(), &["add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &value, "/f"], Duration::from_secs(5)).await
    } else {
        run(&reg(), &["delete", RUN_KEY, "/v", RUN_NAME, "/f"], Duration::from_secs(5)).await
    };
    if r.code != 0 {
        return Err(crate::Error::config(match (crate::i18n::is_en(), on) {
            (false, _) => format!("無法{}開機自動啟動：{}", if on { "設定" } else { "取消" }, r.stderr.trim()),
            (true, true) => format!("Couldn't turn on Start with Windows: {}", r.stderr.trim()),
            (true, false) => format!("Couldn't turn off Start with Windows: {}", r.stderr.trim()),
        }));
    }
    Ok(())
}

/// 開機自動啟動指向別的程式時（例如舊版留在另一個資料夾、或已經刪掉），改成目前這個程式；
/// 沒有開啟開機自動啟動時不動。從暫存資料夾執行（例如在 zip 裡直接按兩下）時不改，
/// 否則暫存檔刪掉後開機就不會啟動。回傳原本指向的位置（有改的時候）
pub async fn repoint_autostart() -> Option<String> {
    if !IS_RELEASE {
        return None;
    }
    let cmd = read_run_value()?;
    let exe = std::env::current_exe().ok()?;
    if in_dir(&exe, &std::env::temp_dir()) {
        return None;
    }
    let target = run_target(&cmd);
    if target.is_some_and(|t| same_file(Path::new(t), &exe)) && cmd.contains("--tray") {
        return None;
    }
    let old = target.unwrap_or(&cmd).to_string();
    match set_autostart(true).await {
        Ok(()) => {
            crate::info!("開機自動啟動原本指向「{old}」，已改成目前的程式：{}", exe.display());
            Some(old)
        }
        Err(e) => {
            crate::warn!("開機自動啟動指向「{old}」，無法改成目前的程式：{}", e.message());
            None
        }
    }
}

/// Run 機碼裡的命令列（例如 `"C:\錄影\ScreenRecorder.exe" --tray`）
#[cfg(windows)]
fn read_run_value() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = vec![0u16; 4096];
    let mut bytes = (buf.len() * 2) as u32;
    let r =
        unsafe { RegGetValueW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), w!("ScreenRecorder"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)) };
    if r.is_err() {
        return None;
    }
    // 長度包含結尾的 0
    let len = (bytes as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0').to_string())
}

#[cfg(not(windows))]
fn read_run_value() -> Option<String> {
    None
}

/// 命令列裡的程式路徑：`"C:\a b\x.exe" --tray` 或 `C:\x.exe --tray`
fn run_target(cmd: &str) -> Option<&str> {
    let c = cmd.trim();
    if let Some(rest) = c.strip_prefix('"') {
        return rest.split('"').next().filter(|s| !s.is_empty());
    }
    // 沒有引號：路徑裡可能有空白，取到「.exe」為止（只轉換英文字母大小寫，位置不變）
    match c.to_ascii_lowercase().find(".exe") {
        Some(i) => Some(&c[..i + 4]),
        None => c.split_whitespace().next(),
    }
}

/// 是不是同一個檔案（Windows 的路徑不分大小寫；短檔名、捷徑等由 canonicalize 處理）
fn same_file(a: &Path, b: &Path) -> bool {
    if a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) {
        return true;
    }
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// 檔案在不在這個資料夾裡（暫存資料夾常是短檔名，例如 C:\Users\ABCD~1\…：先換成完整路徑再比）
fn in_dir(file: &Path, dir: &Path) -> bool {
    let full = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    full(file).starts_with(full(dir))
}

/// 用檔案總管開啟（select = 在資料夾中選取該檔案）。
/// 由 Shell 啟動的程式不會落在本程式的 Job Object 裡，關閉本程式時不會被一併結束。
pub fn open_with_explorer(target: &str, select: bool) {
    let arg = if select { format!("/select,\"{target}\"") } else { format!("\"{target}\"") };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // 完整路徑：只寫 explorer.exe 時會先找本程式的資料夾
        let mut cmd = std::process::Command::new(crate::paths::windows_dir().join("explorer.exe"));
        // explorer 自己解析參數（/select,"路徑"），不能再被加上一層引號
        cmd.raw_arg(&arg).creation_flags(CREATE_BREAKAWAY_FROM_JOB);
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = arg;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_program_path_from_the_run_value() {
        assert_eq!(run_target(r#""C:\Program Files\錄影\ScreenRecorder.exe" --tray"#), Some(r"C:\Program Files\錄影\ScreenRecorder.exe"));
        assert_eq!(run_target(r"C:\Tools\Screen Recorder.EXE --tray"), Some(r"C:\Tools\Screen Recorder.EXE"));
        assert_eq!(run_target(r"C:\Tools\recorder --tray"), Some(r"C:\Tools\recorder"));
        assert_eq!(run_target(r#""" --tray"#), None);
    }

    #[test]
    fn compares_paths_like_windows() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ScreenRecorder.exe");
        std::fs::write(&exe, b"x").unwrap();
        let upper = std::path::PathBuf::from(exe.to_string_lossy().to_uppercase());
        assert!(same_file(&exe, &upper));
        assert!(same_file(&exe, &dir.path().join(".").join("ScreenRecorder.exe")));
        assert!(!same_file(&exe, &dir.path().join("old").join("ScreenRecorder.exe")));
        assert!(in_dir(&exe, dir.path()));
        assert!(in_dir(&exe, &dir.path().join("..").join(dir.path().file_name().unwrap())));
        assert!(!in_dir(&exe, &dir.path().join("old")));
    }
}
