//! 系統匣圖示與全域快速鍵（Windows）。
//!
//! TrackPopupMenu 在選單開著的期間會卡住所在的執行緒，所以圖示、選單與快速鍵都放在獨立的執行緒，
//! 操作視窗與錄影不會跟著停住。這裡只負責畫圖示與選單，使用者選了什麼就交給 TrayController 處理。

use crate::icon::{icon_resource, IconState};
use crate::tray::{TrayCommand, TrayState, TrayUi};
use crate::types::{HotkeyStatus, Hotkeys, RecorderState};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, NOTIFY_ICON_DATA_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
    KillTimer, PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer, TrackPopupMenu, TranslateMessage, HICON, HMENU,
    LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY,
    WM_HOTKEY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW,
};

const WM_TRAY: u32 = WM_APP + 1;
/// 有其他執行緒送來的要求（狀態、通知、結束）
const WM_WAKE: u32 = WM_APP + 2;
const NIN_BALLOONUSERCLICK: u32 = 0x405;
/// 圖示加不上去時重試的計時器
const RETRY_TIMER: usize = 7;
/// 全域快速鍵的 id：1 錄影、2 暫停、3 截圖、4 框選截圖、5 加標記、6 螢幕畫筆（與 Hotkeys::all 的順序相同）
const HOTKEY_IDS: [i32; crate::types::HOTKEY_COUNT] = [1, 2, 3, 4, 5, 6];

/// 登記全域快速鍵（先取消舊的）；被其他程式占用時登記失敗，停用的視為成功
fn register_hotkeys(hwnd: HWND, keys: &Hotkeys) -> HotkeyStatus {
    let ok: Vec<bool> = keys
        .all()
        .iter()
        .zip(HOTKEY_IDS)
        .map(|(k, id)| unsafe {
            let _ = UnregisterHotKey(Some(hwnd), id);
            let Some(k) = k else { return true };
            let mut m: HOT_KEY_MODIFIERS = MOD_NOREPEAT;
            for (on, f) in [(k.ctrl, MOD_CONTROL), (k.alt, MOD_ALT), (k.shift, MOD_SHIFT), (k.win, MOD_WIN)] {
                if on {
                    m |= f;
                }
            }
            RegisterHotKey(Some(hwnd), id, m, k.key).is_ok()
        })
        .collect();
    HotkeyStatus::from_list(&ok)
}

/// 選單右側顯示的快速鍵（停用時不顯示）
fn tab(k: &str) -> String {
    if k.is_empty() {
        String::new()
    } else {
        format!("\t{k}")
    }
}

enum Req {
    State(Box<TrayState>),
    Balloon(String, String, bool),
    Dispose(mpsc::Sender<()>),
    Hotkeys(Hotkeys, mpsc::Sender<HotkeyStatus>),
}

struct Handle {
    hwnd: isize,
    queue: Arc<Mutex<VecDeque<Req>>>,
}

impl Handle {
    fn send(&self, r: Req) {
        self.queue.lock().unwrap().push_back(r);
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

impl TrayUi for Handle {
    fn set_state(&self, s: TrayState) {
        self.send(Req::State(Box::new(s)));
    }
    fn balloon(&self, title: &str, text: &str, warn: bool) {
        self.send(Req::Balloon(title.into(), text.into(), warn));
    }
    fn dispose(&self) {
        let (tx, rx) = mpsc::channel();
        self.send(Req::Dispose(tx));
        let _ = rx.recv_timeout(Duration::from_secs(1));
    }
    fn set_hotkeys(&self, k: &Hotkeys) -> Option<HotkeyStatus> {
        let (tx, rx) = mpsc::channel();
        self.send(Req::Hotkeys(*k, tx));
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }
}

struct Tray {
    hwnd: HWND,
    state: Option<TrayState>,
    icons: HashMap<IconState, HICON>,
    icon_size: i32,
    added: bool,
    taskbar_created: u32,
    queue: Arc<Mutex<VecDeque<Req>>>,
    cmd: UnboundedSender<TrayCommand>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 寫入固定長度的 UTF-16 欄位（保留結尾的 0）
fn put_str(dst: &mut [u16], s: &str) {
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(max)) {
        *d = c;
    }
}

fn icon_kind(rec: Option<RecorderState>) -> IconState {
    match rec {
        Some(RecorderState::Recording) | Some(RecorderState::Stopping) => IconState::Recording,
        Some(RecorderState::Paused) => IconState::Paused,
        Some(RecorderState::Countdown) => IconState::Countdown,
        _ => IconState::Idle,
    }
}

impl Tray {
    fn icon(&mut self, kind: IconState) -> HICON {
        if let Some(h) = self.icons.get(&kind) {
            return *h;
        }
        let res = icon_resource(self.icon_size as usize, kind);
        let h = unsafe { CreateIconFromResourceEx(&res, true, 0x0003_0000, self.icon_size, self.icon_size, LR_DEFAULTCOLOR) }.unwrap_or_default();
        self.icons.insert(kind, h);
        h
    }

    fn nid(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: 1, uFlags: flags, uCallbackMessage: WM_TRAY, ..Default::default() }
    }

    fn icon_data(&mut self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        let kind = icon_kind(self.state.as_ref().map(|s| s.rec));
        let icon = self.icon(kind);
        let mut d = self.nid(flags);
        d.hIcon = icon;
        put_str(&mut d.szTip, self.state.as_ref().map(|s| s.tip.as_str()).unwrap_or("螢幕錄影"));
        d
    }

    fn balloon_data(&self, title: &str, text: &str, warn: bool) -> NOTIFYICONDATAW {
        let mut d = self.nid(NIF_INFO);
        put_str(&mut d.szInfo, text);
        put_str(&mut d.szInfoTitle, title);
        d.dwInfoFlags = if warn { NIIF_WARNING } else { NIIF_INFO };
        d
    }
}

/// 把圖示加到系統匣（還沒加上時）；回傳現在有沒有在系統匣上。借用 TRAY 時不呼叫 Shell_NotifyIcon
fn ensure_icon() -> bool {
    let Some(d) = TRAY.with(|t| t.borrow_mut().as_mut().filter(|t| !t.added).map(|t| t.icon_data(NIF_MESSAGE | NIF_ICON | NIF_TIP))) else {
        return TRAY.with(|t| t.borrow().as_ref().is_some_and(|t| t.added));
    };
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &d).as_bool() };
    TRAY.with(|t| {
        if let Some(t) = t.borrow_mut().as_mut() {
            t.added = ok;
        }
    });
    ok
}

/// 處理其他執行緒送來的要求
fn drain() {
    let queue = TRAY.with(|t| t.borrow().as_ref().map(|t| t.queue.clone()));
    let Some(queue) = queue else { return };
    loop {
        let Some(req) = queue.lock().unwrap().pop_front() else { break };
        match req {
            // Shell_NotifyIcon 會等檔案總管回應，等的時候這條執行緒可能又收到訊息（進到 wnd_proc）：
            // 所以借用 TRAY 時只準備資料，放開之後才呼叫，避免重複借用而整個程式結束
            Req::State(s) => {
                let d = TRAY.with(|t| {
                    let mut b = t.borrow_mut();
                    let t = b.as_mut()?;
                    t.state = Some(*s);
                    t.added.then(|| t.icon_data(NIF_ICON | NIF_TIP))
                });
                if let Some(d) = d {
                    unsafe {
                        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
                    }
                }
            }
            Req::Balloon(title, text, warn) => {
                let d = TRAY.with(|t| t.borrow().as_ref().filter(|t| t.added).map(|t| t.balloon_data(&title, &text, warn)));
                if let Some(d) = d {
                    unsafe {
                        let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
                    }
                }
            }
            Req::Dispose(done) => {
                let info = TRAY.with(|t| {
                    let mut b = t.borrow_mut();
                    let t = b.as_mut()?;
                    let added = std::mem::replace(&mut t.added, false);
                    let icons: Vec<HICON> = t.icons.drain().map(|(_, h)| h).collect();
                    Some((t.hwnd, added.then(|| t.nid(NOTIFY_ICON_DATA_FLAGS(0))), icons))
                });
                if let Some((hwnd, nid, icons)) = info {
                    unsafe {
                        if let Some(d) = nid {
                            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
                        }
                        for id in HOTKEY_IDS {
                            let _ = UnregisterHotKey(Some(hwnd), id);
                        }
                        for h in icons {
                            let _ = DestroyIcon(h);
                        }
                        let _ = DestroyWindow(hwnd);
                    }
                }
                let _ = done.send(());
            }
            Req::Hotkeys(keys, done) => {
                if let Some(hwnd) = TRAY.with(|t| t.borrow().as_ref().map(|t| t.hwnd)) {
                    let _ = done.send(register_hotkeys(hwnd, &keys));
                }
            }
        }
    }
}

fn send_cmd(cmd: TrayCommand) {
    TRAY.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            let _ = t.cmd.send(cmd);
        }
    });
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let ev = (lparam.0 as u32) & 0xffff;
            if ev == WM_RBUTTONUP {
                show_menu();
            } else if ev == WM_LBUTTONUP {
                send_cmd(TrayCommand::Open);
            } else if ev == NIN_BALLOONUSERCLICK {
                send_cmd(TrayCommand::BalloonClick);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            match wparam.0 as i32 {
                1 => send_cmd(TrayCommand::HotkeyRecord),
                2 => send_cmd(TrayCommand::HotkeyPause),
                3 => send_cmd(TrayCommand::Screenshot),
                4 => send_cmd(TrayCommand::ScreenshotSelect),
                5 => send_cmd(TrayCommand::Mark),
                // 畫筆直接在這裡開關（不用等）；視窗在自己的執行緒
                6 => crate::screen_pen_win::toggle(),
                _ => {}
            }
            LRESULT(0)
        }
        WM_WAKE => {
            drain();
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == RETRY_TIMER => {
            // 開機登入時工作列還沒好、圖示加不上去：每 3 秒再試，加上了就停
            if ensure_icon() {
                let _ = KillTimer(Some(hwnd), RETRY_TIMER);
                crate::info!("系統匣圖示已加上");
            }
            LRESULT(0)
        }
        _ => {
            // 用 try_borrow：萬一其他地方正借用著（不應該發生），也不要讓整個程式結束
            let taskbar = TRAY.with(|t| t.try_borrow().ok().and_then(|b| b.as_ref().map(|t| t.taskbar_created)).unwrap_or(0));
            if taskbar != 0 && msg == taskbar {
                // 檔案總管重新啟動後圖示會消失，要重新加入
                TRAY.with(|t| {
                    if let Some(t) = t.borrow_mut().as_mut() {
                        t.added = false;
                    }
                });
                ensure_icon();
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

struct Menu {
    ids: Vec<TrayCommand>,
}

impl Menu {
    fn add(&mut self, menu: HMENU, label: &str, cmd: TrayCommand, disabled: bool, checked: bool) {
        self.ids.push(cmd);
        let mut flags = MF_STRING;
        if disabled {
            flags |= MF_GRAYED;
        }
        if checked {
            flags |= MF_CHECKED;
        }
        let w = wide(label);
        unsafe {
            let _ = AppendMenuW(menu, flags, self.ids.len(), PCWSTR(w.as_ptr()));
        }
    }
    fn sep(menu: HMENU) {
        unsafe {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
    }
    fn sub(menu: HMENU, label: &str, child: HMENU, disabled: bool) {
        let w = wide(label);
        let mut flags = MF_POPUP;
        if disabled {
            flags |= MF_GRAYED;
        }
        unsafe {
            let _ = AppendMenuW(menu, flags, child.0 as usize, PCWSTR(w.as_ptr()));
        }
    }
}

fn show_menu() {
    // 先取出需要的資料再顯示選單：選單開著時仍會處理其他訊息（狀態更新）
    let Some((st, hwnd)) = TRAY.with(|t| t.borrow().as_ref().and_then(|t| t.state.clone().map(|s| (s, t.hwnd)))) else {
        return;
    };
    let mut m = Menu { ids: Vec::new() };
    let idle = st.rec == RecorderState::Idle;
    unsafe {
        let Ok(root) = CreatePopupMenu() else { return };
        m.add(root, "開啟操作視窗(&O)", TrayCommand::Open, false, false);
        let _ = SetMenuDefaultItem(root, 0, 1);
        if let Some(u) = &st.update {
            m.add(root, &format!("★ 有新版本 v{u}（開啟視窗更新）"), TrayCommand::OpenUpdate, false, false);
        }
        Menu::sep(root);
        // 「\t」後的文字顯示在選單右側（快速鍵提示）
        m.add(root, &format!("開始錄影(&R)　{}{}", st.last_source, tab(&st.keys[0])), TrayCommand::StartLast, !idle || !st.can_record, false);
        // 錄其他範圍：與「截圖」相同的選法（框選範圍或視窗、全螢幕、重複上次框選）
        if let Ok(pick) = CreatePopupMenu() {
            m.add(pick, "框選範圍或視窗(&A)…", TrayCommand::StartSelect, false, false);
            if let Ok(full) = CreatePopupMenu() {
                for mon in &st.monitors {
                    m.add(full, &mon.label, TrayCommand::StartMonitor(mon.id.clone()), false, false);
                }
                if st.monitors.len() > 1 {
                    Menu::sep(full);
                    m.add(full, "所有螢幕（整個延伸桌面）", TrayCommand::StartAll, false, false);
                }
                Menu::sub(pick, "全螢幕(&F)", full, st.monitors.is_empty());
            }
            Menu::sep(pick);
            m.add(pick, "重複上次框選(&R)", TrayCommand::StartLastSnip, !st.has_last_snip, false);
            Menu::sub(root, "錄製其他範圍(&M)", pick, !idle || !st.can_record);
        }
        if st.rec == RecorderState::Paused {
            m.add(root, &format!("繼續錄影(&C){}", tab(&st.keys[1])), TrayCommand::Resume, false, false);
        } else {
            m.add(root, &format!("暫停(&P){}", tab(&st.keys[1])), TrayCommand::Pause, st.rec != RecorderState::Recording, false);
        }
        if st.rec == RecorderState::Countdown {
            m.add(root, "取消倒數(&S)", TrayCommand::Stop, false, false);
        } else {
            m.add(root, &format!("停止並儲存(&S){}", tab(&st.keys[0])), TrayCommand::Stop, idle || st.rec == RecorderState::Stopping, false);
        }
        m.add(root, &format!("加標記(&K){}", tab(&st.keys[4])), TrayCommand::Mark, !matches!(st.rec, RecorderState::Recording | RecorderState::Paused), false);
        m.add(root, &format!("螢幕畫筆(&D){}", tab(&st.keys[5])), TrayCommand::Pen, false, crate::screen_pen_win::active());
        Menu::sep(root);
        // 截圖：框選範圍或點選視窗、全螢幕、固定範圍（主畫面的錄影範圍）、重複上次框選
        if let Ok(shot) = CreatePopupMenu() {
            m.add(shot, &format!("框選範圍或視窗(&A)…{}", tab(&st.keys[3])), TrayCommand::ScreenshotSelect, false, false);
            if let Ok(delay) = CreatePopupMenu() {
                for sec in [3u64, 5, 10] {
                    m.add(delay, &format!("{sec} 秒後"), TrayCommand::ScreenshotDelay(sec), false, false);
                }
                Menu::sub(shot, "延遲框選(&D)", delay, false);
            }
            m.add(shot, "長截圖（捲動）(&L)…", TrayCommand::ScreenshotScroll, false, false);
            m.add(shot, "讀取 QR 碼(&Q)…", TrayCommand::ScreenshotQr, false, false);
            m.add(shot, "取色器(&C)…", TrayCommand::ScreenColor, false, false);
            m.add(shot, "尺規（量距離）(&R)…", TrayCommand::ScreenRuler, false, false);
            m.add(shot, "步驟截圖（做成教學文件）(&P)", TrayCommand::StepsStart, st.steps.is_some(), false);
            if let Ok(full) = CreatePopupMenu() {
                for mon in &st.monitors {
                    m.add(full, &mon.label, TrayCommand::ScreenshotMonitor(mon.id.clone()), false, false);
                }
                if st.monitors.len() > 1 {
                    Menu::sep(full);
                    m.add(full, "所有螢幕（整個延伸桌面）", TrayCommand::ScreenshotAll, false, false);
                }
                Menu::sub(shot, "全螢幕(&F)", full, st.monitors.is_empty());
            }
            m.add(shot, &format!("固定範圍(&X)：{}{}", st.last_source, tab(&st.keys[2])), TrayCommand::Screenshot, false, false);
            Menu::sep(shot);
            m.add(shot, "重複上次框選(&R)", TrayCommand::ScreenshotLast, !st.has_last_snip, false);
            Menu::sep(shot);
            m.add(shot, "編輯上次截圖(&E)…", TrayCommand::EditLastShot, !st.has_shot, false);
            Menu::sub(root, "截圖(&T)", shot, !st.can_shot);
        }
        // 步驟截圖進行中：放在最上層，隨時可以完成
        if let Some(n) = st.steps {
            m.add(root, &format!("完成步驟截圖（{n} 步）(&G)"), TrayCommand::StepsFinish, false, false);
        }
        Menu::sep(root);
        if let Ok(audio) = CreatePopupMenu() {
            m.add(audio, "系統聲音", TrayCommand::ToggleSystem, false, st.audio_system);
            m.add(audio, "麥克風", TrayCommand::ToggleMic, false, st.audio_mic);
            Menu::sub(root, "錄製聲音(&A)", audio, !idle);
        }
        Menu::sep(root);
        m.add(root, "開啟儲存資料夾(&F)", TrayCommand::OpenFolder, false, false);
        m.add(root, "播放最近的錄影(&L)", TrayCommand::PlayLast, st.last_result.is_none(), false);
        Menu::sep(root);
        m.add(root, "開機時自動啟動", TrayCommand::Autostart, st.autostart.is_none(), st.autostart == Some(true));
        m.add(root, &format!("更新說明（v{}）", st.version), TrayCommand::Changelog, false, false);
        m.add(root, "結束(&X)", TrayCommand::Quit, false, false);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // SetForegroundWindow：否則點選單外面時選單不會關閉
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(root, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None).0;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(root); // 連同子選單一起釋放
        if chosen > 0 {
            if let Some(cmd) = m.ids.get(chosen as usize - 1) {
                send_cmd(cmd.clone());
            }
        }
    }
}

/// 讓選單跟著 Windows 的深色 / 淺色模式（uxtheme 未公開 API；失敗就維持預設外觀）
fn allow_dark_menus() {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        // SetPreferredAppMode（序號 135）、FlushMenuThemes（序號 136）
        if let Some(set_mode) = GetProcAddress(ux, PCSTR(135usize as *const u8)) {
            let f: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            f(1); // AllowDark
        }
        if let Some(flush) = GetProcAddress(ux, PCSTR(136usize as *const u8)) {
            let f: unsafe extern "system" fn() = std::mem::transmute(flush);
            f();
        }
    }
}

/// 啟動系統匣執行緒；圖示加入成功後回傳控制介面與快速鍵登記結果
pub fn start(cmd: UnboundedSender<TrayCommand>, keys: Hotkeys) -> Result<(Arc<dyn TrayUi>, HotkeyStatus), String> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(isize, HotkeyStatus), String>>();
    let queue: Arc<Mutex<VecDeque<Req>>> = Arc::default();
    let q = queue.clone();
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || unsafe {
            allow_dark_menus();
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let class = w!("ScreenRecorderTray");
            let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wnd_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
            RegisterClassExW(&wc);
            let hwnd = match CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("螢幕錄影"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(hinst.into()), None) {
                Ok(h) => h,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("無法建立系統匣視窗：{e}")));
                    return;
                }
            };
            let icon_size = GetSystemMetrics(SM_CXSMICON).max(16);
            let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
            TRAY.with(|t| {
                *t.borrow_mut() = Some(Tray { hwnd, state: None, icons: HashMap::new(), icon_size, added: false, taskbar_created, queue: q, cmd });
            });
            // 先處理已送來的狀態，圖示一開始就顯示正確的提示
            drain();
            // 快速鍵登記在這個執行緒的視窗上（WM_HOTKEY 會送到這裡）；被其他程式占用時登記失敗。
            // 先回報準備好（不等圖示）：加圖示要等檔案總管，開機登入時可能很慢
            let _ = ready_tx.send(Ok((hwnd.0 as isize, register_hotkeys(hwnd, &keys))));
            // 圖示加不上去（工作列還沒準備好）：快速鍵照樣能用，每 3 秒再試著加圖示
            if !ensure_icon() {
                crate::warn!("系統匣圖示暫時加不上去（工作列可能還沒準備好），稍後會再試");
                SetTimer(Some(hwnd), RETRY_TIMER, 3000, None);
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            TRAY.with(|t| *t.borrow_mut() = None);
        })
        .map_err(|e| e.to_string())?;
    let (hwnd, hotkeys) = ready_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "系統匣沒有回應".to_string())??;
    Ok((Arc::new(Handle { hwnd, queue }), hotkeys))
}
