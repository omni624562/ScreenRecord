//! 與 Windows 桌面整合：
//! 開機自動啟動、用檔案總管開啟檔案 / 網址。

use crate::process::run;
use std::time::Duration;

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "ScreenRecorder";

/// 開發版（debug build）不設定開機自動啟動
const IS_RELEASE: bool = !cfg!(debug_assertions);

/// 開機自動啟動：寫入目前使用者的 Run 機碼（啟動時只常駐系統匣，不開視窗）。開發版回傳 None
pub async fn get_autostart() -> Option<bool> {
    if !IS_RELEASE || !cfg!(windows) {
        return None;
    }
    Some(run("reg.exe", &["query", RUN_KEY, "/v", RUN_NAME], Duration::from_secs(5)).await.code == 0)
}

pub async fn set_autostart(on: bool) -> crate::Result<()> {
    if !IS_RELEASE || !cfg!(windows) {
        return Err(crate::Error::config(crate::tr!("開發模式無法設定開機自動啟動", "Start with Windows can't be set in development mode")));
    }
    let exe = std::env::current_exe()?;
    let value = format!("\"{}\" --tray", exe.display());
    let r = if on {
        run("reg.exe", &["add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &value, "/f"], Duration::from_secs(5)).await
    } else {
        run("reg.exe", &["delete", RUN_KEY, "/v", RUN_NAME, "/f"], Duration::from_secs(5)).await
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

/// 用檔案總管開啟（select = 在資料夾中選取該檔案）。
/// 由 Shell 啟動的程式不會落在本程式的 Job Object 裡，關閉本程式時不會被一併結束。
pub fn open_with_explorer(target: &str, select: bool) {
    let arg = if select { format!("/select,\"{target}\"") } else { format!("\"{target}\"") };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let mut cmd = std::process::Command::new("explorer.exe");
        // explorer 自己解析參數（/select,"路徑"），不能再被加上一層引號
        cmd.raw_arg(&arg).creation_flags(CREATE_BREAKAWAY_FROM_JOB);
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = arg;
    }
}
