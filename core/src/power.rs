//! 不讓電腦睡眠：錄影中（長時間沒有操作時 Windows 會自動睡眠，錄影就中斷了）與排程錄影等待中。
//! SetThreadExecutionState 是跟著執行緒的設定：用一條專用的執行緒持有，KeepAwake 丟掉時恢復。

/// 持有期間電腦不會因為閒置而睡眠；display = true 時螢幕也不會關（錄畫面時需要）
pub struct KeepAwake {
    #[cfg(windows)]
    _stop: std::sync::mpsc::Sender<()>,
}

pub fn keep_awake(display: bool) -> KeepAwake {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = std::thread::Builder::new().name("keep-awake".into()).spawn(move || unsafe {
            let flags = if display { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS | ES_SYSTEM_REQUIRED };
            SetThreadExecutionState(flags);
            // 傳送端丟掉時 recv 回傳錯誤：恢復成平常的設定
            let _ = rx.recv();
            SetThreadExecutionState(ES_CONTINUOUS);
        });
        KeepAwake { _stop: tx }
    }
    #[cfg(not(windows))]
    {
        let _ = display;
        KeepAwake {}
    }
}
