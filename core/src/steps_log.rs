//! 操作步驟：錄影時記下點了什麼（Windows 的 UI Automation 讀出按鈕、選單的名稱）與按了哪些快速鍵，
//! 錄完可以整理成文字清單（「1. 00:05 在「報價單.xlsx - Excel」點「檔案」（選單）」），貼到文件或信件裡。

use crate::format::short_clock;
use crate::{tr, trf};
use serde::{Deserialize, Serialize};

/// 一個操作
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// 影片的秒數
    pub t: f64,
    /// 0 = 點一下、1 = 按右鍵、2 = 按快速鍵
    #[serde(default)]
    pub kind: u8,
    /// 點到的東西的名稱（按鈕上的字）或按鍵（「Ctrl + S」）
    #[serde(default)]
    pub name: String,
    /// 點到的東西是什麼（按鈕、選單項目…，Windows 依系統語言給的）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// 在哪個視窗（視窗標題）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub window: String,
}

pub const CLICK: u8 = 0;
pub const RIGHT: u8 = 1;
pub const KEY: u8 = 2;

/// 一支錄影最多記幾個
pub const MAX_ACTIONS: usize = 5_000;

/// 太長的名稱截短（例如點到整段文章）
pub fn clip(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        s
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 一行的說明（不含編號與時間）
fn line(a: &Action) -> String {
    match a.kind {
        KEY => trf!("按 {}", "Press {}", a.name),
        k => {
            let verb = if k == RIGHT { tr!("按右鍵點", "Right-click") } else { tr!("點", "Click") };
            match (a.name.is_empty(), a.role.is_empty()) {
                (true, true) => trf!("{verb}一下", "{verb}"),
                (true, false) => trf!("{verb}{}", "{verb} the {}", a.role),
                (false, true) => trf!("{verb}「{}」", "{verb} “{}”", a.name),
                (false, false) => trf!("{verb}「{}」（{}）", "{verb} “{}” ({})", a.name, a.role),
            }
        }
    }
}

/// 整理成文字清單：視窗換了才寫「在…」；連續點同一個東西只算一次；title：清單的標題（錄影的名稱）
pub fn to_text(actions: &[Action], title: &str) -> String {
    let mut out = vec![trf!("操作步驟：{title}", "Steps: {title}"), String::new()];
    let mut window = String::new();
    let mut prev: Option<(u8, String, String)> = None;
    let mut n = 0;
    for a in actions {
        let key = (a.kind, a.name.clone(), a.role.clone());
        if prev.as_ref() == Some(&key) && a.kind != KEY {
            continue;
        }
        prev = Some(key);
        n += 1;
        let mut text = line(a);
        if !a.window.is_empty() && a.window != window {
            window = a.window.clone();
            text = trf!("在「{}」{}", "In “{}”, {}", clip(&a.window, 60), lower_first(&text));
        }
        out.push(format!("{n}. {}  {text}", short_clock(a.t)));
    }
    out.join("\n")
}

/// 英文句子接在「In …,」後面時第一個字母改小寫
fn lower_first(s: &str) -> String {
    if !crate::i18n::is_en() {
        return s.to_string();
    }
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(windows)]
pub use win::describe;

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetWindowTextW, WindowFromPoint, GA_ROOT};

    thread_local! {
        static UIA: Option<IUIAutomation> = unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()
        };
    }

    /// 桌面座標 (x, y) 上的東西：(名稱, 類型, 視窗標題)。讀不到時名稱、類型是空的
    pub fn describe(x: i32, y: i32) -> (String, String, String) {
        let pt = POINT { x, y };
        let window = unsafe {
            let w = GetAncestor(WindowFromPoint(pt), GA_ROOT);
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(w, &mut buf);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        };
        let (name, role) = UIA.with(|u| {
            let Some(u) = u else { return (String::new(), String::new()) };
            unsafe {
                match u.ElementFromPoint(pt) {
                    Ok(e) => (e.CurrentName().map(|s| s.to_string()).unwrap_or_default(), e.CurrentLocalizedControlType().map(|s| s.to_string()).unwrap_or_default()),
                    Err(_) => (String::new(), String::new()),
                }
            }
        });
        (super::clip(&name, 50), role, super::clip(&window, 80))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(t: f64, kind: u8, name: &str, role: &str, window: &str) -> Action {
        Action { t, kind, name: name.into(), role: role.into(), window: window.into() }
    }

    #[test]
    fn steps_become_a_list() {
        let list = [
            a(5.2, CLICK, "檔案", "選單項目", "報價單.xlsx - Excel"),
            a(5.6, CLICK, "檔案", "選單項目", "報價單.xlsx - Excel"),
            a(7.0, CLICK, "另存新檔", "", "報價單.xlsx - Excel"),
            a(9.0, KEY, "Ctrl + S", "", "報價單.xlsx - Excel"),
            a(70.0, RIGHT, "", "", "LINE"),
        ];
        let text = to_text(&list, "Rec_1");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "操作步驟：Rec_1");
        assert_eq!(lines[2], "1. 0:05  在「報價單.xlsx - Excel」點「檔案」（選單項目）");
        // 連續點同一個只算一次；視窗沒換不再寫
        assert_eq!(lines[3], "2. 0:07  點「另存新檔」");
        assert_eq!(lines[4], "3. 0:09  按 Ctrl + S");
        assert_eq!(lines[5], "4. 1:10  在「LINE」按右鍵點一下");
        assert_eq!(lines.len(), 6);
        assert_eq!(clip("  很長\n的   名稱 ", 3), "很長 …");
        // 存檔：空的欄位不寫
        let j = serde_json::to_string(&a(1.0, KEY, "Ctrl + C", "", "")).unwrap();
        assert!(!j.contains("role") && !j.contains("window"));
    }
}
