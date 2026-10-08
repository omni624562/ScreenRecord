//! WASAPI 音訊擷取（對應 src/audio.ts）。
//!
//! FFmpeg 在 Windows 只能透過 dshow 錄麥克風，錄不到「電腦正在播放的聲音」，
//! 因此系統聲音（loopback）與麥克風都在這裡自行擷取，統一轉成 48 kHz / 立體聲 / float32，
//! 每個封包附上 QPC 時間戳，再由 AudioPipe 對齊畫面時間後送進 FFmpeg。

use crate::types::AudioDevice;

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const BYTES_PER_FRAME: usize = CHANNELS * 4;

/// 封包處理函式：交錯排列的 float32 立體聲（None = 靜音）、取樣數、第一個取樣的 QPC 時間（100ns，無效時 None）
pub type Visit<'a> = dyn FnMut(Option<&[f32]>, usize, Option<i64>) + 'a;

/// 一個擷取來源。visit 收到：交錯排列的 float32 立體聲（None = 靜音）、取樣數、第一個取樣的 QPC 時間（100ns，無效時 None）
pub trait Capture {
    fn name(&self) -> String;
    /// 依序處理目前所有可讀的封包；裝置失效（拔除、切換）時回傳錯誤
    fn read(&mut self, visit: &mut Visit) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioSourceSpec {
    pub loopback: bool,
    /// 麥克風裝置 ID；空字串 = 預設麥克風
    pub mic_id: String,
}

/// 開啟擷取來源（可替換：測試時用假的來源）
pub type Opener = std::sync::Arc<dyn Fn(&AudioSourceSpec) -> Result<Box<dyn Capture>, String> + Send + Sync>;

/// 列出可錄音的裝置（麥克風等）與預設播放裝置名稱
pub struct AudioDevices {
    pub render: Option<String>,
    pub captures: Vec<AudioDevice>,
}

#[cfg(windows)]
pub use win::{list_audio_devices, open_wasapi};

#[cfg(not(windows))]
pub fn list_audio_devices() -> Result<AudioDevices, String> {
    Ok(AudioDevices { render: None, captures: vec![] })
}

#[cfg(not(windows))]
pub fn open_wasapi(_spec: &AudioSourceSpec) -> Result<Box<dyn Capture>, String> {
    Err("這個平台不支援錄音".into())
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};

    const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x2;
    const AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR: u32 = 0x4;
    /// 2 秒緩衝：擷取執行緒偶爾停頓也不會掉資料
    const BUFFER_100NS: i64 = 20_000_000;

    fn hex(e: &windows::core::Error) -> String {
        format!("0x{:08x}", e.code().0 as u32)
    }

    fn enumerator() -> Result<IMMDeviceEnumerator, String> {
        unsafe {
            // 這個執行緒可能已經初始化過 COM（任何模式都可以用 MMDevice API）
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("無法建立音訊裝置列舉器 ({})", hex(&e)))
        }
    }

    fn device_id(dev: &IMMDevice) -> String {
        unsafe {
            match dev.GetId() {
                Ok(p) => {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as _));
                    s
                }
                Err(_) => String::new(),
            }
        }
    }

    fn friendly_name(dev: &IMMDevice) -> String {
        unsafe {
            let Ok(store) = dev.OpenPropertyStore(STGM_READ) else { return String::new() };
            let Ok(pv) = store.GetValue(&PKEY_Device_FriendlyName) else { return String::new() };
            pv.to_string()
        }
    }

    pub fn list_audio_devices() -> Result<AudioDevices, String> {
        let en = enumerator()?;
        unsafe {
            let default_id = en.GetDefaultAudioEndpoint(eCapture, eConsole).map(|d| device_id(&d)).unwrap_or_default();
            let render = en.GetDefaultAudioEndpoint(eRender, eConsole).ok().map(|d| friendly_name(&d)).filter(|n| !n.is_empty());
            let mut captures = Vec::new();
            if let Ok(coll) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
                for i in 0..coll.GetCount().unwrap_or(0) {
                    let Ok(dev) = coll.Item(i) else { continue };
                    let id = device_id(&dev);
                    let name = friendly_name(&dev);
                    captures.push(AudioDevice { name: if name.is_empty() { id.clone() } else { name }, is_default: id == default_id, id });
                }
            }
            captures.sort_by_key(|d| !d.is_default);
            Ok(AudioDevices { render, captures })
        }
    }

    struct Wasapi {
        name: String,
        client: IAudioClient,
        capture: IAudioCaptureClient,
        _device: IMMDevice,
    }

    impl Drop for Wasapi {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }

    /// loopback = 錄預設播放裝置的聲音；否則錄指定（或預設）麥克風
    pub fn open_wasapi(spec: &AudioSourceSpec) -> Result<Box<dyn Capture>, String> {
        let en = enumerator()?;
        unsafe {
            let device: IMMDevice = if spec.loopback {
                en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| "找不到播放裝置".to_string())?
            } else {
                let picked = if spec.mic_id.is_empty() {
                    None
                } else {
                    let wide: Vec<u16> = spec.mic_id.encode_utf16().chain(std::iter::once(0)).collect();
                    en.GetDevice(PCWSTR(wide.as_ptr())).ok()
                };
                match picked {
                    Some(d) => d,
                    None => en.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|_| "找不到麥克風".to_string())?,
                }
            };
            let name = Some(friendly_name(&device)).filter(|n| !n.is_empty()).unwrap_or_else(|| if spec.loopback { "播放裝置".into() } else { "麥克風".into() });
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| format!("Activate 失敗 ({})", hex(&e)))?;
            // 48 kHz / 2ch / float32（搭配 AUTOCONVERTPCM，任何裝置都轉成同一格式）
            let fmt = WAVEFORMATEX {
                wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
                nChannels: CHANNELS as u16,
                nSamplesPerSec: SAMPLE_RATE,
                nAvgBytesPerSec: SAMPLE_RATE * BYTES_PER_FRAME as u32,
                nBlockAlign: BYTES_PER_FRAME as u16,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            if spec.loopback {
                flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
            }
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, &fmt, None).map_err(|e| format!("Initialize 失敗 ({})", hex(&e)))?;
            let capture: IAudioCaptureClient = client.GetService().map_err(|e| format!("GetService 失敗 ({})", hex(&e)))?;
            client.Start().map_err(|e| format!("Start 失敗 ({})", hex(&e)))?;
            Ok(Box::new(Wasapi { name, client, capture, _device: device }))
        }
    }

    impl Capture for Wasapi {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn read(&mut self, visit: &mut Visit) -> Result<(), String> {
            unsafe {
                loop {
                    let next = self.capture.GetNextPacketSize().map_err(|e| format!("音訊裝置中斷 ({})", hex(&e)))?;
                    if next == 0 {
                        return Ok(());
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
                    self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc)).map_err(|e| format!("音訊裝置中斷 ({})", hex(&e)))?;
                    let ts = (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR == 0).then_some(qpc as i64);
                    if frames > 0 {
                        if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                            visit(None, frames as usize, ts);
                        } else {
                            let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * CHANNELS);
                            visit(Some(samples), frames as usize, ts);
                        }
                    }
                    let _ = self.capture.ReleaseBuffer(frames);
                }
            }
        }
    }
}
