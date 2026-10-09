//! 剪輯預覽的聲音輸出：把 FFmpeg 解出來的 48 kHz / 立體聲 / float32 送到預設播放裝置（WASAPI 共用模式）。
//! 其他平台沒有聲音（只把資料讀掉）。

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
#[cfg(any(windows, test))]
const BYTES_PER_FRAME: usize = CHANNELS * 4;

/// 播放到資料結束或 stop 被設定為止（在呼叫的執行緒上執行）。
/// started：開始出聲時呼叫一次（傳入輸出延遲，秒），讓畫面與聲音對齊
pub fn play(reader: impl Read, stop: Arc<AtomicBool>, started: impl FnOnce(f64)) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::play(reader, stop, started)
    }
    #[cfg(not(windows))]
    {
        let mut reader = reader;
        started(0.0);
        let mut buf = [0u8; 16384];
        while !stop.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        Ok(())
    }
}

/// 從 reader 讀滿 buf（讀到結尾時回傳實際讀到的位元組數）
#[cfg(any(windows, test))]
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::Duration;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    /// 200 ms 緩衝
    const BUFFER_100NS: i64 = 2_000_000;

    fn hex(e: &windows::core::Error) -> String {
        format!("0x{:08x}", e.code().0 as u32)
    }

    pub fn play(mut reader: impl Read, stop: Arc<AtomicBool>, started: impl FnOnce(f64)) -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("無法建立音訊裝置列舉器 ({})", hex(&e)))?;
            let device = en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "找不到播放裝置".to_string())?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| format!("Activate 失敗 ({})", hex(&e)))?;
            let fmt = WAVEFORMATEX {
                wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
                nChannels: CHANNELS as u16,
                nSamplesPerSec: SAMPLE_RATE,
                nAvgBytesPerSec: SAMPLE_RATE * BYTES_PER_FRAME as u32,
                nBlockAlign: BYTES_PER_FRAME as u16,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, &fmt, None).map_err(|e| format!("Initialize 失敗 ({})", hex(&e)))?;
            let render: IAudioRenderClient = client.GetService().map_err(|e| format!("GetService 失敗 ({})", hex(&e)))?;
            let size = client.GetBufferSize().map_err(|e| format!("GetBufferSize 失敗 ({})", hex(&e)))?;
            let latency = client.GetStreamLatency().unwrap_or(0) as f64 / 1e7;
            let mut bytes = vec![0u8; size as usize * BYTES_PER_FRAME];
            let mut running = false;
            let mut started = Some(started);
            let mut eof = false;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let padding = client.GetCurrentPadding().unwrap_or(size);
                let free = (size - padding) as usize;
                if !eof && free > 0 {
                    let n = read_full(&mut reader, &mut bytes[..free * BYTES_PER_FRAME]);
                    let frames = n / BYTES_PER_FRAME;
                    if frames > 0 {
                        if let Ok(p) = render.GetBuffer(frames as u32) {
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, frames * BYTES_PER_FRAME);
                            let _ = render.ReleaseBuffer(frames as u32, 0);
                        }
                    }
                    if n < free * BYTES_PER_FRAME {
                        eof = true;
                    }
                }
                if !running {
                    client.Start().map_err(|e| format!("Start 失敗 ({})", hex(&e)))?;
                    running = true;
                    if let Some(f) = started.take() {
                        f(latency + padding as f64 / SAMPLE_RATE as f64);
                    }
                }
                if eof && client.GetCurrentPadding().unwrap_or(0) == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = client.Stop();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_until_the_end() {
        let data = vec![0u8; 48_000 * BYTES_PER_FRAME / 10];
        let stop = Arc::new(AtomicBool::new(false));
        // 沒有播放裝置的環境（CI）會回傳錯誤，但不應卡住
        let _ = play(std::io::Cursor::new(data), stop, |_| {});
        let mut buf = [0u8; 8];
        assert_eq!(read_full(&mut std::io::Cursor::new(vec![1u8; 5]), &mut buf), 5);
    }
}
