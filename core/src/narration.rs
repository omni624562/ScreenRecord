//! 旁白與點擊音效：剪輯時錄的旁白，以及用錄影時記下的點擊加上「喀」聲。
//! 文字轉語音（speak_to_file）留給步驟教學影片的「唸出說明」；舊版剪輯專案裡用文字轉語音做的旁白照樣播放。
//! 旁白檔（WAV）存在 %LOCALAPPDATA%\ScreenRecorder\voice\；放在原影片的時間，輸出時換成剪輯後的時間再混進聲音。

use crate::edit::{map_time, Part};
use crate::{tr, trf};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一段旁白：從原影片的 at_ms 開始播放 WAV 檔
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceClip {
    /// WAV 檔的完整路徑
    pub file: String,
    pub at_ms: u64,
    /// 長度（毫秒）
    pub ms: u64,
    /// 文字轉語音的內容（錄的旁白是空的）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

impl VoiceClip {
    pub fn at(&self) -> f64 {
        self.at_ms as f64 / 1000.0
    }
    pub fn end(&self) -> f64 {
        (self.at_ms + self.ms) as f64 / 1000.0
    }
    pub fn is_speech(&self) -> bool {
        !self.text.is_empty()
    }
}

/// 旁白與點擊音效的設定
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Narration {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<VoiceClip>,
    /// 旁白的音量（%）
    #[serde(default = "full")]
    pub volume: u32,
    /// 有旁白的時候原本的聲音剩多少（%）：0 = 取代、100 = 不變
    #[serde(default = "duck")]
    pub original: u32,
    /// 點擊音效的音量（%）；0 = 不加
    #[serde(default, skip_serializing_if = "is_zero")]
    pub click_volume: u32,
    /// 點擊的時間（原影片的秒數）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clicks: Vec<f64>,
}

fn full() -> u32 {
    100
}
fn duck() -> u32 {
    20
}
fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl Default for Narration {
    fn default() -> Self {
        Narration { clips: vec![], volume: 100, original: 20, click_volume: 0, clicks: vec![] }
    }
}

impl Narration {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && !self.has_clicks()
    }
    pub fn has_clicks(&self) -> bool {
        self.click_volume > 0 && !self.clicks.is_empty()
    }

    /// 加一段旁白；和原有的重疊時取代原有的（重錄一段）
    pub fn put(&mut self, clip: VoiceClip) {
        let (a, b) = (clip.at_ms, clip.at_ms + clip.ms);
        self.clips.retain(|c| c.at_ms + c.ms <= a || c.at_ms >= b);
        self.clips.push(clip);
        self.clips.sort_by_key(|c| c.at_ms);
    }
}

/// 旁白檔的資料夾
pub fn voice_dir() -> PathBuf {
    crate::paths::data_dir().join("voice")
}

/// 新的旁白檔名
pub fn new_voice_path(prefix: &str) -> PathBuf {
    let dir = voice_dir();
    let _ = std::fs::create_dir_all(&dir);
    crate::paths::unique_path(&dir, &format!("{prefix}_{}", crate::paths::now_ms()), ".wav")
}

/// 輸出時一段旁白：檔案、剪輯後的開始時間、長度
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub file: String,
    pub at: f64,
    pub len: f64,
}

/// 旁白放到剪輯後的時間：開頭落在刪掉的片段裡時從下一段保留的開始；之後都刪掉的不放；檔案不見的略過
pub fn place(parts: &[Part], clips: &[VoiceClip], main_len: f64) -> Vec<Placed> {
    clips
        .iter()
        .filter(|c| Path::new(&c.file).is_file() && c.ms >= 50)
        .filter_map(|c| {
            let at = map_time(parts, c.at()).or_else(|| parts.iter().find(|p| !p.is_hold() && p.a > c.at()).and_then(|p| map_time(parts, p.a)))?;
            (at < main_len - 0.05).then(|| Placed { file: c.file.clone(), at, len: (c.ms as f64 / 1000.0).min(main_len - at) })
        })
        .collect()
}

/// 原本的聲音在旁白時調小：volume 濾鏡的運算式（剪輯後的時間）
pub fn duck_filter(placed: &[Placed], original: u32) -> Option<String> {
    if placed.is_empty() || original >= 100 {
        return None;
    }
    let any = placed.iter().map(|p| format!("between(t,{},{})", crate::format::num(p.at), crate::format::num(p.at + p.len))).collect::<Vec<_>>().join("+");
    Some(format!("volume='if({any},{},1)':eval=frame", crate::format::num(original as f64 / 100.0)))
}

const RATE: u32 = 48000;

/// 點擊音效的聲音檔（48 kHz 單聲道 WAV）：每個點擊（剪輯後的時間）一聲短短的「喀」
pub fn click_track(times: &[f64], total: f64, volume: u32) -> Vec<u8> {
    let n = ((total.max(0.1)) * RATE as f64) as usize;
    let mut buf = vec![0f32; n];
    let gain = (volume.min(200) as f32 / 100.0) * 0.5;
    let len = (RATE as f32 * 0.045) as usize;
    // 固定的雜訊（每次輸出都一樣）
    let mut seed: u32 = 0x9e37_79b9;
    let noise: Vec<f32> = (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect();
    for &t in times {
        let start = (t.max(0.0) * RATE as f64) as usize;
        for (i, nz) in noise.iter().enumerate() {
            let Some(s) = buf.get_mut(start + i) else { break };
            let x = i as f32 / RATE as f32;
            let env = (-x * 110.0).exp();
            let tone = (2.0 * std::f32::consts::PI * 2400.0 * x).sin() * 0.6 + (2.0 * std::f32::consts::PI * 900.0 * x).sin() * 0.3;
            *s += (tone + nz * 0.5) * env * gain;
        }
    }
    wav_mono16(&buf, RATE)
}

/// 16 位元單聲道 WAV
pub fn wav_mono16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    v
}

/// WAV 檔的長度（毫秒）：讀檔頭的 fmt 與 data
pub fn wav_ms(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut byte_rate = 0u32;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().ok()?);
        if id == b"fmt " && i + 20 <= bytes.len() {
            byte_rate = u32::from_le_bytes(bytes[i + 16..i + 20].try_into().ok()?);
        } else if id == b"data" {
            // 錄到一半的檔案：data 的大小可能還沒寫，用實際的長度
            let size = if size == 0 || size as usize > bytes.len() - i - 8 { (bytes.len() - i - 8) as u32 } else { size };
            return (byte_rate > 0).then(|| size as u64 * 1000 / byte_rate as u64);
        }
        i += 8 + size as usize + (size as usize & 1);
    }
    None
}

/// 旁白清單上的說明：「0:12 錄的旁白（5 秒）」
pub fn describe(c: &VoiceClip) -> String {
    let at = crate::format::video_clock(c.at());
    let secs = crate::format::num((c.ms as f64 / 100.0).round() / 10.0);
    if c.is_speech() {
        let short: String = c.text.chars().take(14).collect();
        let more = if c.text.chars().count() > 14 { "…" } else { "" };
        trf!("{at} 語音：{short}{more}（{secs} 秒）", "{at} Speech: {short}{more} ({secs}s)")
    } else {
        trf!("{at} 錄的旁白（{secs} 秒）", "{at} Recorded narration ({secs}s)")
    }
}

/// 錄旁白：開著麥克風，把聲音（轉成單聲道）存在記憶體裡，停止時寫成 WAV
pub struct VoiceRecorder {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shared: std::sync::Arc<std::sync::Mutex<Rec>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Rec {
    samples: Vec<f32>,
    level: f32,
    error: Option<String>,
}

/// 旁白最長錄幾分鐘（約 55 MB 記憶體）
const MAX_MINUTES: usize = 10;

impl VoiceRecorder {
    /// 開始錄：mic_id 空字串 = 預設麥克風
    pub fn start(open: crate::audio::Opener, mic_id: &str) -> VoiceRecorder {
        use std::sync::atomic::Ordering;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Rec::default()));
        let (s, sh, id) = (stop.clone(), shared.clone(), mic_id.to_string());
        let thread = std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || {
                let mut cap = match open(&crate::audio::AudioSourceSpec { loopback: false, mic_id: id, app: String::new() }) {
                    Ok(c) => c,
                    Err(e) => {
                        sh.lock().unwrap().error = Some(trf!("{}：{e}", "{}: {e}", no_mic()));
                        return;
                    }
                };
                let max = MAX_MINUTES * 60 * crate::audio::SAMPLE_RATE as usize;
                while !s.load(Ordering::Relaxed) {
                    let mut got: Vec<f32> = vec![];
                    let r = cap.read(&mut |data, frames, _| match data {
                        Some(d) => got.extend(d.chunks(crate::audio::CHANNELS).map(|c| c.iter().sum::<f32>() / c.len() as f32)),
                        None => got.extend(std::iter::repeat_n(0.0, frames)),
                    });
                    {
                        let mut g = sh.lock().unwrap();
                        if let Err(e) = r {
                            g.error = Some(e);
                            break;
                        }
                        g.level = crate::meter::decay(g.level, crate::meter::peak(&got));
                        let room = max.saturating_sub(g.samples.len());
                        g.samples.extend(got.into_iter().take(room));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            })
            .ok();
        VoiceRecorder { stop, shared, thread }
    }

    /// 目前的音量（0～1）
    pub fn level(&self) -> f32 {
        self.shared.lock().unwrap().level
    }

    /// 錄了幾秒
    pub fn secs(&self) -> f64 {
        self.shared.lock().unwrap().samples.len() as f64 / crate::audio::SAMPLE_RATE as f64
    }

    pub fn error(&self) -> Option<String> {
        self.shared.lock().unwrap().error.clone()
    }

    /// 停止並存成 WAV；回傳長度（毫秒）。太短（不到 0.3 秒）或錄不到時回傳錯誤
    pub fn finish(mut self, path: &Path) -> Result<u64, String> {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let g = self.shared.lock().unwrap();
        if let Some(e) = &g.error {
            return Err(e.clone());
        }
        let ms = g.samples.len() as u64 * 1000 / crate::audio::SAMPLE_RATE as u64;
        if ms < 300 {
            return Err(tr!("旁白太短，沒有存下來", "The narration was too short and wasn't kept").into());
        }
        std::fs::write(path, wav_mono16(&g.samples, crate::audio::SAMPLE_RATE)).map_err(|e| e.to_string())?;
        Ok(ms)
    }
}

impl Drop for VoiceRecorder {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 沒有麥克風時的訊息
pub fn no_mic() -> String {
    tr!("找不到麥克風，無法錄旁白", "No microphone found, so narration can't be recorded").into()
}

/// 文字轉語音可以用的聲音：(ID, 名稱)；台灣中文的排前面
pub fn voices() -> Vec<(String, String)> {
    #[cfg(windows)]
    {
        speech::voices()
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

/// 文字轉語音（Windows 內建的語音，不用網路）：存成 WAV，回傳長度（毫秒）。
/// voice 是 voices() 的 ID，空字串 = 依介面語言選台灣中文或英文；rate：0.5～2
pub fn speak_to_file(text: &str, voice: &str, rate: f64, path: &Path) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(tr!("請輸入要說的話", "Enter the text to speak").into());
    }
    #[cfg(windows)]
    {
        let bytes = speech::synthesize(text, voice, rate)?;
        let ms = wav_ms(&bytes).ok_or_else(|| tr!("產生的語音格式不對", "The generated speech has an unexpected format").to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())?;
        Ok(ms)
    }
    #[cfg(not(windows))]
    {
        let _ = (voice, rate, path);
        Err(tr!("文字轉語音要用 Windows 內建的語音，只支援 Windows", "Text to speech uses the voices built into Windows and is only available on Windows").into())
    }
}

#[cfg(windows)]
mod speech {
    use windows::core::HSTRING;
    use windows::Media::SpeechSynthesis::SpeechSynthesizer;
    use windows::Storage::Streams::{DataReader, InputStreamOptions};

    pub fn voices() -> Vec<(String, String)> {
        let Ok(all) = SpeechSynthesizer::AllVoices() else { return vec![] };
        let mut out: Vec<(String, String, String)> = all
            .into_iter()
            .filter_map(|v| Some((v.Id().ok()?.to_string(), v.DisplayName().ok()?.to_string(), v.Language().ok()?.to_string())))
            .map(|(id, name, lang)| (id, format!("{name}（{lang}）"), lang.to_lowercase()))
            .collect();
        let rank = |l: &str| {
            if l == "zh-tw" {
                0
            } else if l.starts_with("zh") {
                1
            } else if l.starts_with("en") {
                2
            } else {
                3
            }
        };
        out.sort_by(|a, b| rank(&a.2).cmp(&rank(&b.2)).then(a.1.cmp(&b.1)));
        out.into_iter().map(|(id, name, _)| (id, name)).collect()
    }

    pub fn synthesize(text: &str, voice: &str, rate: f64) -> Result<Vec<u8>, String> {
        let err = |e: windows::core::Error| e.message().to_string();
        let synth = SpeechSynthesizer::new().map_err(err)?;
        let want = if voice.is_empty() { None } else { Some(voice.to_string()) };
        let lang = if crate::i18n::is_en() { "en" } else { "zh-tw" };
        if let Ok(all) = SpeechSynthesizer::AllVoices() {
            let pick = all.into_iter().find(|v| match &want {
                Some(id) => v.Id().is_ok_and(|i| i == id.as_str()),
                None => v.Language().is_ok_and(|l| l.to_string().to_lowercase().starts_with(lang)),
            });
            if let Some(v) = pick {
                synth.SetVoice(&v).map_err(err)?;
            }
        }
        if let Ok(o) = synth.Options() {
            let _ = o.SetSpeakingRate(rate.clamp(0.5, 2.0));
        }
        let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text)).map_err(err)?.join().map_err(err)?;
        let size = stream.Size().map_err(err)? as u32;
        let input = stream.GetInputStreamAt(0).map_err(err)?;
        let reader = DataReader::CreateDataReader(&input).map_err(err)?;
        reader.SetInputStreamOptions(InputStreamOptions::None).map_err(err)?;
        let n = reader.LoadAsync(size).map_err(err)?.join().map_err(err)?;
        let mut buf = vec![0u8; n as usize];
        reader.ReadBytes(&mut buf).map_err(err)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(file: &str, at: u64, ms: u64) -> VoiceClip {
        VoiceClip { file: file.into(), at_ms: at, ms, text: String::new() }
    }

    #[test]
    fn re_recording_replaces_overlapping_clips() {
        let mut n = Narration::default();
        n.put(clip("a", 1000, 3000));
        n.put(clip("b", 6000, 2000));
        n.put(clip("c", 3000, 1000));
        assert_eq!(n.clips.iter().map(|c| c.file.as_str()).collect::<Vec<_>>(), vec!["c", "b"]);
        assert!(!n.is_empty());
        let mut m = Narration { clicks: vec![1.0], ..Default::default() };
        assert!(m.is_empty());
        m.click_volume = 60;
        assert!(m.has_clicks());
    }

    #[test]
    fn clips_follow_the_cut() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("v.wav");
        std::fs::write(&f, wav_mono16(&[0.0; 4800], 48000)).unwrap();
        let f = f.to_string_lossy().to_string();
        // 保留 0～4、6～10：4 秒處的旁白從 6 秒（剪輯後 4 秒）開始；11 秒的已經在結尾之後
        let parts = [Part::new(0.0, 4.0, 1.0), Part::new(6.0, 10.0, 1.0)];
        let placed = place(&parts, &[clip(&f, 2000, 1000), clip(&f, 4500, 3000), clip(&f, 11000, 1000), clip("missing.wav", 0, 1000)], 8.0);
        assert_eq!(placed.iter().map(|p| (p.at, p.len)).collect::<Vec<_>>(), vec![(2.0, 1.0), (4.0, 3.0)]);
        let duck = duck_filter(&placed, 20).unwrap();
        assert_eq!(duck, "volume='if(between(t,2,3)+between(t,4,7),0.2,1)':eval=frame");
        assert!(duck_filter(&placed, 100).is_none());
    }

    #[test]
    fn click_track_is_a_wav_of_the_right_length() {
        let w = click_track(&[0.5, 1.0], 2.0, 80);
        assert_eq!(wav_ms(&w), Some(2000));
        // 點擊的地方有聲音、其他地方安靜
        let sample = |t: f64| i16::from_le_bytes([w[44 + (t * 48000.0) as usize * 2], w[45 + (t * 48000.0) as usize * 2]]);
        assert!((0..200).any(|i| sample(0.5 + i as f64 / 48000.0).abs() > 1000));
        assert_eq!(sample(0.2), 0);
        assert_eq!(wav_ms(b"nope"), None);
    }
}
