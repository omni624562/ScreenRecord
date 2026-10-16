//! 開會時提醒錄影：Teams、Zoom、Google Meet（瀏覽器）等開始用麥克風時，問要不要開始錄影；
//! 通話結束（不再用麥克風）時，如果是開會時開始的錄影，問要不要停止。只提醒，不會自己開始或停止。
//! 哪個程式正在用麥克風：Windows 的隱私設定記錄（CapabilityAccessManager\ConsentStore\microphone），
//! LastUsedTimeStop 是 0 的就是正在用。

use crate::tr;

/// 登錄檔的項目名稱 → 會議程式的名稱（不是會議程式時 None）。
/// 一般程式的名稱是路徑（\ 換成 #），例如 C:#Users#x#AppData#Local#Microsoft#Teams#current#Teams.exe；
/// 市集的程式是套件名稱，例如 MSTeams_8wekyb3d8bbwe
pub fn app_name(key: &str) -> Option<&'static str> {
    let k = key.to_ascii_lowercase();
    let exe = k.rsplit(['#', '\\', '/']).next().unwrap_or(&k);
    // 市集的程式（套件名稱）
    if k.starts_with("msteams_") || k.starts_with("microsoftteams_") {
        return Some("Teams");
    }
    if k.starts_with("zoom.") || k.contains("zoomvideocommunications") {
        return Some("Zoom");
    }
    if k.contains("skype") {
        return Some("Skype");
    }
    Some(match exe {
        "teams.exe" | "ms-teams.exe" | "msteams.exe" => "Teams",
        "zoom.exe" | "zoom_launcher.exe" => "Zoom",
        "ciscocollabhost.exe" | "webexhost.exe" | "webex.exe" | "atmgr.exe" | "ptoneclk.exe" => "Webex",
        "skype.exe" => "Skype",
        "slack.exe" => "Slack",
        "discord.exe" => "Discord",
        "line.exe" => "LINE",
        "chrome.exe" | "msedge.exe" | "firefox.exe" | "brave.exe" | "opera.exe" | "vivaldi.exe" => return Some(browser()),
        _ => return None,
    })
}

/// 瀏覽器用麥克風：多半是 Google Meet（也可能是其他網頁會議）
fn browser() -> &'static str {
    tr!("瀏覽器的會議（Google Meet 等）", "a browser meeting (Google Meet, etc.)")
}

/// 要提醒什麼
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// 開始開會了，要不要開始錄影（會議程式的名稱）
    Start(String),
    /// 會議結束了，要不要停止錄影
    Stop(String),
}

/// 連續看到幾次才算數（每 3 秒看一次：約 6 秒），短暫用一下麥克風（例如測試音量）不會提醒
const SEEN: u32 = 2;

/// 會議開始、結束的判斷（每次看完正在用麥克風的程式後呼叫 step）
#[derive(Debug, Default)]
pub struct Watch {
    /// 目前認定在開會的程式
    meeting: Option<String>,
    /// 連續看到（或沒看到）幾次
    count: u32,
    /// 這場會議裡有在錄影（會議結束時才問要不要停止）
    recorded: bool,
}

impl Watch {
    /// using：正在用麥克風的會議程式（第一個）；recording：本程式正在錄影
    pub fn step(&mut self, using: Option<&str>, recording: bool) -> Option<Prompt> {
        match (&self.meeting, using) {
            (None, Some(app)) => {
                self.count += 1;
                if self.count < SEEN {
                    return None;
                }
                self.meeting = Some(app.to_string());
                self.count = 0;
                self.recorded = recording;
                // 已經在錄影：不用問
                (!recording).then(|| Prompt::Start(app.to_string()))
            }
            (None, None) => {
                self.count = 0;
                None
            }
            (Some(_), Some(_)) => {
                self.count = 0;
                self.recorded |= recording;
                None
            }
            (Some(app), None) => {
                self.count += 1;
                if self.count < SEEN {
                    return None;
                }
                let app = app.clone();
                let ask = self.recorded && recording;
                (self.meeting, self.count, self.recorded) = (None, 0, false);
                ask.then_some(Prompt::Stop(app))
            }
        }
    }
}

/// 現在正在用麥克風的會議程式（不含本程式自己）
pub fn using_mic() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = imp::keys_in_use().iter().filter_map(|k| app_name(k)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(windows)]
mod imp {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_QWORD};

    const BASE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn subkeys(path: &str) -> Vec<String> {
        let mut out = vec![];
        unsafe {
            let mut h = HKEY::default();
            let p = wide(path);
            if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), None, KEY_READ, &mut h) != ERROR_SUCCESS {
                return out;
            }
            let mut i = 0;
            loop {
                let mut buf = [0u16; 512];
                let mut len = buf.len() as u32;
                if RegEnumKeyExW(h, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) != ERROR_SUCCESS {
                    break;
                }
                out.push(String::from_utf16_lossy(&buf[..len as usize]));
                i += 1;
            }
            let _ = RegCloseKey(h);
        }
        out
    }

    fn qword(path: &str, name: &str) -> Option<u64> {
        let (p, n) = (wide(path), wide(name));
        let mut v = 0u64;
        let mut size = 8u32;
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), PCWSTR(n.as_ptr()), RRF_RT_REG_QWORD, None, Some((&mut v as *mut u64).cast()), Some(&mut size)) };
        (r == ERROR_SUCCESS).then_some(v)
    }

    fn in_use(path: &str) -> bool {
        qword(path, "LastUsedTimeStart").unwrap_or(0) != 0 && qword(path, "LastUsedTimeStop") == Some(0)
    }

    /// 正在用麥克風的項目名稱（市集的程式與一般程式）
    pub fn keys_in_use() -> Vec<String> {
        let me = std::env::current_exe().ok().map(|p| p.display().to_string().replace('\\', "#").to_ascii_lowercase());
        let mut out = vec![];
        for k in subkeys(BASE).into_iter().filter(|k| k != "NonPackaged") {
            if in_use(&format!(r"{BASE}\{k}")) {
                out.push(k);
            }
        }
        let np = format!(r"{BASE}\NonPackaged");
        for k in subkeys(&np) {
            if me.as_deref() == Some(k.to_ascii_lowercase().as_str()) {
                continue;
            }
            if in_use(&format!(r"{np}\{k}")) {
                out.push(k);
            }
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn keys_in_use() -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_apps_are_recognized() {
        assert_eq!(app_name(r"C:#Users#amy#AppData#Local#Microsoft#Teams#current#Teams.exe"), Some("Teams"));
        assert_eq!(app_name("MSTeams_8wekyb3d8bbwe"), Some("Teams"));
        assert_eq!(app_name(r"C:#Users#amy#AppData#Roaming#Zoom#bin#Zoom.exe"), Some("Zoom"));
        assert_eq!(app_name(r"C:#Program Files#Google#Chrome#Application#chrome.exe"), Some(browser()));
        assert_eq!(app_name(r"C:#Users#amy#AppData#Local#LINE#bin#current#LINE.exe"), Some("LINE"));
        assert_eq!(app_name(r"C:#Program Files#Audacity#Audacity.exe"), None);
        assert_eq!(app_name("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe"), None);
    }

    #[test]
    fn prompts_once_per_meeting() {
        let mut w = Watch::default();
        // 短暫用一下麥克風：不提醒
        assert_eq!(w.step(Some("Teams"), false), None);
        assert_eq!(w.step(None, false), None);
        // 連續兩次：問要不要錄影；之後同一場會議不再問
        assert_eq!(w.step(Some("Teams"), false), None);
        assert_eq!(w.step(Some("Teams"), false), Some(Prompt::Start("Teams".into())));
        assert_eq!(w.step(Some("Teams"), false), None);
        // 開始錄影，會議結束（連續兩次沒看到）：問要不要停止
        assert_eq!(w.step(Some("Teams"), true), None);
        assert_eq!(w.step(None, true), None);
        assert_eq!(w.step(None, true), Some(Prompt::Stop("Teams".into())));
        assert_eq!(w.step(None, true), None);
    }

    #[test]
    fn no_stop_prompt_without_recording_and_no_start_prompt_while_recording() {
        let mut w = Watch::default();
        w.step(Some("Zoom"), false);
        assert_eq!(w.step(Some("Zoom"), false), Some(Prompt::Start("Zoom".into())));
        // 沒有錄：結束時不用問
        w.step(None, false);
        assert_eq!(w.step(None, false), None);
        // 已經在錄影時開會：不問要不要開始；會議結束時已經停了：不問要不要停止
        w.step(Some("Zoom"), true);
        assert_eq!(w.step(Some("Zoom"), true), None);
        w.step(None, false);
        assert_eq!(w.step(None, false), None);
    }
}
