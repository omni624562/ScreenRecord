//! 單調時鐘（100ns 單位）。Windows 上是 QPC：WASAPI 封包時間戳與 FFmpeg 的 av_gettime_relative() 都以 QPC 為基準，
//! 用同一個時鐘才能讓聲音與畫面長時間對齊。其他平台（測試）用 Instant。

#[cfg(windows)]
pub fn now_100ns() -> i64 {
    use std::sync::OnceLock;
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    static FREQ: OnceLock<i64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    });
    let mut c = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
    }
    ((c as i128 * 10_000_000) / freq as i128) as i64
}

#[cfg(not(windows))]
pub fn now_100ns() -> i64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    (START.get_or_init(Instant::now).elapsed().as_nanos() / 100) as i64
}
