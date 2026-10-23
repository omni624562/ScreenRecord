//! WASAPI 音訊擷取。
//!
//! FFmpeg 在 Windows 只能透過 dshow 錄麥克風，錄不到「電腦正在播放的聲音」，
//! 因此系統聲音（loopback）與麥克風都在這裡自行擷取，統一轉成 48 kHz / 立體聲 / float32，
//! 每個封包附上 QPC 時間戳記，再由 AudioPipe 對齊畫面時間後送進 FFmpeg。

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
    /// 系統聲音只錄這個程式（執行檔名稱，例如 chrome.exe；含它開的子程序）；空字串 = 全部
    pub app: String,
}

/// 開啟擷取來源（可抽換：測試時用假的來源）
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
    Err(crate::tr!("這個平台不支援錄音", "Audio recording isn't supported on this platform").into())
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::{tr, trf};
    use windows::core::Interface;
    use windows::core::PCWSTR;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
        IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
        DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, IAgileObject, IAgileObject_Impl, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};

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
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| trf!("無法建立音訊裝置列舉器 ({})", "Couldn't create the audio device enumerator ({})", hex(&e)))
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
        /// 只錄某個程式時沒有裝置
        _device: Option<IMMDevice>,
    }

    impl Drop for Wasapi {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }

    /// 完成通知（ActivateAudioInterfaceAsync 在別的執行緒呼叫；必須是 agile 物件）
    #[windows::core::implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
    struct Activated(std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
        fn ActivateCompleted(&self, _op: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
            if let Some(tx) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = tx.send(());
            }
            Ok(())
        }
    }

    impl IAgileObject_Impl for Activated_Impl {}

    /// 只錄某個程式（和它開的子程序）播放的聲音（process loopback；較舊的 Windows 不支援）
    fn process_loopback(pid: u32) -> Result<IAudioClient, String> {
        use std::mem::ManuallyDrop;
        use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
        use windows::Win32::System::Com::BLOB;
        use windows::Win32::System::Variant::VT_BLOB;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                    ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS { TargetProcessId: pid, ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE },
                },
            };
            // 參數放在 BLOB 裡（記憶體是我們的，不能讓 PROPVARIANT 去釋放）
            let pv = ManuallyDrop::new(PROPVARIANT {
                Anonymous: PROPVARIANT_0 {
                    Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                        vt: VT_BLOB,
                        wReserved1: 0,
                        wReserved2: 0,
                        wReserved3: 0,
                        Anonymous: PROPVARIANT_0_0_0 { blob: BLOB { cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32, pBlobData: &mut params as *mut _ as *mut u8 } },
                    }),
                },
            });
            let (tx, rx) = std::sync::mpsc::channel();
            let handler: IActivateAudioInterfaceCompletionHandler = Activated(std::sync::Mutex::new(Some(tx))).into();
            let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv as *const PROPVARIANT), &handler)
                .map_err(|e| trf!("無法只錄這個程式的聲音 ({})", "Couldn't capture just this app's audio ({})", hex(&e)))?;
            rx.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| tr!("只錄程式的聲音沒有回應", "Capturing the app's audio didn't respond").to_string())?;
            let mut hr = windows::core::HRESULT(0);
            let mut unk: Option<windows::core::IUnknown> = None;
            op.GetActivateResult(&mut hr, &mut unk).map_err(|e| hex(&e))?;
            hr.ok().map_err(|e| trf!("無法只錄這個程式的聲音 ({})", "Couldn't capture just this app's audio ({})", hex(&e)))?;
            unk.ok_or_else(|| tr!("無法只錄這個程式的聲音", "Couldn't capture just this app's audio").to_string())?.cast::<IAudioClient>().map_err(|e| hex(&e))
        }
    }

    /// 48 kHz / 2ch / float32（搭配 AUTOCONVERTPCM，任何裝置都轉成同一格式）
    fn format() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * BYTES_PER_FRAME as u32,
            nBlockAlign: BYTES_PER_FRAME as u16,
            wBitsPerSample: 32,
            cbSize: 0,
        }
    }

    /// 系統聲音只錄某個程式；做不到（程式沒開、Windows 太舊）時回傳原因，改錄全部系統聲音
    fn open_app(exe: &str) -> Result<Box<dyn Capture>, String> {
        let pid = crate::winui::pid_for_exe(exe).ok_or_else(|| trf!("{exe} 沒有開啟", "{exe} isn't running"))?;
        let client = process_loopback(pid)?;
        unsafe {
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, &format(), None).map_err(|e| trf!("Initialize 失敗 ({})", "Initialize failed ({})", hex(&e)))?;
            let capture: IAudioCaptureClient = client.GetService().map_err(|e| trf!("GetService 失敗 ({})", "GetService failed ({})", hex(&e)))?;
            client.Start().map_err(|e| trf!("Start 失敗 ({})", "Start failed ({})", hex(&e)))?;
            Ok(Box::new(Wasapi { name: trf!("只錄 {exe}", "{exe} only"), client, capture, _device: None }))
        }
    }

    /// loopback = 錄預設播放裝置的聲音（有指定程式時只錄那個程式）；否則錄指定（或預設）麥克風
    pub fn open_wasapi(spec: &AudioSourceSpec) -> Result<Box<dyn Capture>, String> {
        if spec.loopback && !spec.app.is_empty() {
            return match open_app(&spec.app) {
                Ok(c) => Ok(c),
                // 改錄全部系統聲音也失敗：兩個原因都寫出來
                Err(e) => open_device(spec, Some(&e)).map_err(|d| trf!("{d}（{e}）", "{d} ({e})")),
            };
        }
        open_device(spec, None)
    }

    /// fallback = 只錄某個程式失敗的原因（改錄全部系統聲音）
    fn open_device(spec: &AudioSourceSpec, fallback: Option<&str>) -> Result<Box<dyn Capture>, String> {
        let en = enumerator()?;
        unsafe {
            let device: IMMDevice = if spec.loopback {
                en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|_| tr!("找不到播放裝置", "Playback device not found").to_string())?
            } else {
                let picked = if spec.mic_id.is_empty() {
                    None
                } else {
                    let wide: Vec<u16> = spec.mic_id.encode_utf16().chain(std::iter::once(0)).collect();
                    en.GetDevice(PCWSTR(wide.as_ptr())).ok()
                };
                match picked {
                    Some(d) => d,
                    None => en.GetDefaultAudioEndpoint(eCapture, eConsole).map_err(|_| tr!("找不到麥克風", "Microphone not found").to_string())?,
                }
            };
            let name = Some(friendly_name(&device)).filter(|n| !n.is_empty()).unwrap_or_else(|| {
                if spec.loopback {
                    tr!("播放裝置", "Playback device").into()
                } else {
                    tr!("麥克風", "Microphone").into()
                }
            });
            // 只錄某個程式失敗：名稱寫上原因（事件紀錄看得到），錄全部系統聲音
            let name = match fallback {
                Some(e) => trf!("{name}（{e}，改錄全部系統聲音）", "{name} ({e}; recording all system audio)"),
                None => name,
            };
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| trf!("Activate 失敗 ({})", "Activate failed ({})", hex(&e)))?;
            let fmt = format();
            let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            if spec.loopback {
                flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
            }
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, &fmt, None).map_err(|e| trf!("Initialize 失敗 ({})", "Initialize failed ({})", hex(&e)))?;
            let capture: IAudioCaptureClient = client.GetService().map_err(|e| trf!("GetService 失敗 ({})", "GetService failed ({})", hex(&e)))?;
            client.Start().map_err(|e| trf!("Start 失敗 ({})", "Start failed ({})", hex(&e)))?;
            Ok(Box::new(Wasapi { name, client, capture, _device: Some(device) }))
        }
    }

    impl Capture for Wasapi {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn read(&mut self, visit: &mut Visit) -> Result<(), String> {
            unsafe {
                loop {
                    let next = self.capture.GetNextPacketSize().map_err(|e| trf!("音訊裝置中斷 ({})", "Audio device disconnected ({})", hex(&e)))?;
                    if next == 0 {
                        return Ok(());
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
                    self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc)).map_err(|e| trf!("音訊裝置中斷 ({})", "Audio device disconnected ({})", hex(&e)))?;
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
