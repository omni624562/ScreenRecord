//! 錄影前的音量表：開著系統聲音（loopback）與麥克風，算出目前的音量，讓人在開始錄影前就知道有沒有收到聲音。

use crate::audio::{AudioSourceSpec, Opener};
use crate::{tr, trf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 目前的音量（0～1，峰值、慢慢降下來）；None = 沒有開這個來源
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Levels {
    pub system: Option<f32>,
    pub mic: Option<f32>,
    /// 打不開的來源與原因
    pub error: Option<String>,
}

pub struct Meter {
    stop: Arc<AtomicBool>,
    levels: Arc<Mutex<Levels>>,
    /// 開的是哪些來源（設定改了要重開）
    pub key: (bool, Option<String>),
}

/// 一批取樣的峰值
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |m, s| m.max(s.abs())).min(1.0)
}

/// 新的顯示值：上升立刻跟上，下降慢慢降
pub fn decay(prev: f32, now: f32) -> f32 {
    if now >= prev {
        now
    } else {
        (prev * 0.82).max(now)
    }
}

impl Meter {
    /// 開始量：system = 系統聲音、mic = 麥克風（Some("") = 預設麥克風）
    pub fn start(open: Opener, system: bool, mic: Option<String>) -> Meter {
        let stop = Arc::new(AtomicBool::new(false));
        let levels = Arc::new(Mutex::new(Levels { system: system.then_some(0.0), mic: mic.as_ref().map(|_| 0.0), error: None }));
        let (s, l, m) = (stop.clone(), levels.clone(), mic.clone());
        let _ = std::thread::Builder::new().name("meter".into()).spawn(move || {
            let mut errors = vec![];
            let mut sys = if system {
                match open(&AudioSourceSpec { loopback: true, mic_id: String::new() }) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        errors.push(trf!("系統聲音：{e}", "System audio: {e}"));
                        None
                    }
                }
            } else {
                None
            };
            let mut mic = match &m {
                Some(id) => match open(&AudioSourceSpec { loopback: false, mic_id: id.clone() }) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        errors.push(trf!("麥克風：{e}", "Microphone: {e}"));
                        None
                    }
                },
                None => None,
            };
            if !errors.is_empty() {
                l.lock().unwrap().error = Some(errors.join(tr!("；", "; ")));
            }
            let (mut ls, mut lm) = (0.0f32, 0.0f32);
            while !s.load(Ordering::Relaxed) {
                let read = |c: &mut Option<Box<dyn crate::audio::Capture>>, level: &mut f32| {
                    let mut p = 0.0f32;
                    if let Some(cap) = c {
                        if cap
                            .read(&mut |data, _, _| {
                                if let Some(d) = data {
                                    p = p.max(peak(d));
                                }
                            })
                            .is_err()
                        {
                            *c = None;
                        }
                    }
                    *level = decay(*level, p);
                };
                read(&mut sys, &mut ls);
                read(&mut mic, &mut lm);
                {
                    let mut g = l.lock().unwrap();
                    if g.system.is_some() {
                        g.system = Some(ls);
                    }
                    if g.mic.is_some() {
                        g.mic = Some(lm);
                    }
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        Meter { stop, levels, key: (system, mic) }
    }

    pub fn levels(&self) -> Levels {
        self.levels.lock().unwrap().clone()
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_and_decay() {
        assert_eq!(peak(&[0.1, -0.6, 0.3]), 0.6);
        assert_eq!(peak(&[2.0]), 1.0);
        assert_eq!(decay(0.2, 0.5), 0.5);
        let d = decay(0.5, 0.0);
        assert!(d < 0.5 && d > 0.3);
    }

    #[test]
    fn reports_open_errors() {
        let open: Opener = Arc::new(|_: &AudioSourceSpec| Err("找不到裝置".to_string()));
        let m = Meter::start(open, true, Some(String::new()));
        std::thread::sleep(Duration::from_millis(100));
        let l = m.levels();
        assert!(l.error.unwrap().contains("系統聲音：找不到裝置"));
        assert_eq!((l.system, l.mic), (Some(0.0), Some(0.0)));
    }
}
