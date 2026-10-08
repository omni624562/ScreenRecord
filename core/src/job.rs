//! Job Object：本程式結束（包括當掉）時，FFmpeg 等子程序一併結束，不會殘留在背景繼續錄影或佔用檔案。
//! 允許 breakaway：開給使用者的程式（瀏覽器）用 spawn_detached 脫離，關閉本程式時不會被連帶關掉。

#[cfg(windows)]
pub fn install() {
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let Ok(job) = CreateJobObjectW(None, windows::core::PCWSTR::null()) else { return };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, size).is_err() {
            return;
        }
        if let Err(e) = AssignProcessToJobObject(job, GetCurrentProcess()) {
            crate::warn!("無法建立 Job Object：{e}");
        }
        // job 的 handle 刻意不關閉：程式結束時系統關閉它，子程序隨之結束
    }
}

#[cfg(not(windows))]
pub fn install() {}

/// 啟動與本程式無關的程式（不在 Job Object 內、沒有主控台、不等待）
pub fn spawn_detached(program: &str, args: &[String]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
    }
    cmd.spawn().map(|_| ())
}
