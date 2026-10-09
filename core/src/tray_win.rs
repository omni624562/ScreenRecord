//! 系統匣圖示與全域快捷鍵（Windows）。
//!
//! TrackPopupMenu 在選單開著的期間會卡住所在的執行緒，所以圖示、選單與快捷鍵都放在獨立的執行緒，
//! 操作視窗與錄影不會跟著停住。這裡只負責畫圖示與選單，使用者選了什麼就交給 TrayController 處理。

use crate::icon::{icon_resource, IconState};
use crate::tray::{TrayCommand, TrayState, TrayUi};
use crate::types::{HotkeyStatus, RecorderState};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, NOTIFY_ICON_DATA_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, TrackPopupMenu, TranslateMessage, HICON, HMENU, LR_DEFAULTCOLOR, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP, WNDCLASSEXW,
};

const WM_TRAY: u32 = WM_APP + 1;
/// 有其他執行緒送來的要求（狀態、通知、結束）
const WM_WAKE: u32 = WM_APP + 2;
const NIN_BALLOONUSERCLICK: u32 = 0x405;
/// 全域快捷鍵：id、按鍵、指令（與介面顯示的 Ctrl+Alt+R / Ctrl+Alt+P 一致）
const HOTKEYS: [(i32, u32); 3] = [(1, 0x52 /* R */), (2, 0x50 /* P */), (3, 0x53 /* S */)];

enum Req {
    State(TrayState),
    Balloon(String, String, bool),
    Dispose(mpsc::Sender<()>),
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
        self.send(Req::State(s));
    }
    fn balloon(&self, title: &str, text: &str, warn: bool) {
        self.send(Req::Balloon(title.into(), text.into(), warn));
    }
    fn dispose(&self) {
        let (tx, rx) = mpsc::channel();
        self.send(Req::Dispose(tx));
        let _ = rx.recv_timeout(Duration::from_secs(1));
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

    fn add_icon(&mut self) -> bool {
        let d = self.icon_data(NIF_MESSAGE | NIF_ICON | NIF_TIP);
        unsafe { Shell_NotifyIconW(NIM_ADD, &d).as_bool() }
    }

    fn update_icon(&mut self) {
        let d = self.icon_data(NIF_ICON | NIF_TIP);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    fn balloon(&self, title: &str, text: &str, warn: bool) {
        let mut d = self.nid(NIF_INFO);
        put_str(&mut d.szInfo, text);
        put_str(&mut d.szInfoTitle, title);
        d.dwInfoFlags = if warn { NIIF_WARNING } else { NIIF_INFO };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    fn remove_icon(&self) {
        let d = self.nid(NOTIFY_ICON_DATA_FLAGS(0));
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }
}

/// 處理其他執行緒送來的要求
fn drain() {
    let queue = TRAY.with(|t| t.borrow().as_ref().map(|t| t.queue.clone()));
    let Some(queue) = queue else { return };
    loop {
        let Some(req) = queue.lock().unwrap().pop_front() else { break };
        match req {
            Req::State(s) => TRAY.with(|t| {
                if let Some(t) = t.borrow_mut().as_mut() {
                    t.state = Some(s);
                    if t.added {
                        t.update_icon();
                    }
                }
            }),
            Req::Balloon(title, text, warn) => TRAY.with(|t| {
                if let Some(t) = t.borrow().as_ref() {
                    if t.added {
                        t.balloon(&title, &text, warn);
                    }
                }
            }),
            Req::Dispose(done) => {
                let hwnd = TRAY.with(|t| {
                    let mut b = t.borrow_mut();
                    let t = b.as_mut()?;
                    if t.added {
                        t.remove_icon();
                        t.added = false;
                    }
                    for (id, _) in HOTKEYS {
                        unsafe {
                            let _ = UnregisterHotKey(Some(t.hwnd), id);
                        }
                    }
                    for h in t.icons.values() {
                        unsafe {
                            let _ = DestroyIcon(*h);
                        }
                    }
                    t.icons.clear();
                    Some(t.hwnd)
                });
                if let Some(h) = hwnd {
                    unsafe {
                        let _ = DestroyWindow(h);
                    }
                }
                let _ = done.send(());
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
            } else if ev == WM_LBUTTONUP || ev == NIN_BALLOONUSERCLICK {
                send_cmd(TrayCommand::Open);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            match wparam.0 as i32 {
                1 => send_cmd(TrayCommand::HotkeyRecord),
                2 => send_cmd(TrayCommand::HotkeyPause),
                3 => send_cmd(TrayCommand::Screenshot),
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
        _ => {
            let taskbar = TRAY.with(|t| t.borrow().as_ref().map(|t| t.taskbar_created).unwrap_or(0));
            if taskbar != 0 && msg == taskbar {
                // 檔案總管重新啟動後圖示會消失，要重新加入
                TRAY.with(|t| {
                    if let Some(t) = t.borrow_mut().as_mut() {
                        t.added = t.add_icon();
                    }
                });
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
        // 「\t」後的文字顯示在選單右側（快捷鍵提示）
        m.add(root, &format!("開始錄影(&R)　{}\tCtrl+Alt+R", st.last_source), TrayCommand::StartLast, !idle || !st.can_record, false);
        if let Ok(pick) = CreatePopupMenu() {
            for mon in &st.monitors {
                m.add(pick, &mon.label, TrayCommand::StartMonitor(mon.id.clone()), false, false);
            }
            if st.monitors.len() > 1 {
                Menu::sep(pick);
                m.add(pick, "所有螢幕（整個延伸桌面）", TrayCommand::StartAll, false, false);
            }
            Menu::sub(root, "錄製指定螢幕(&M)", pick, !idle || !st.can_record);
        }
        if st.rec == RecorderState::Paused {
            m.add(root, "繼續錄影(&C)\tCtrl+Alt+P", TrayCommand::Resume, false, false);
        } else {
            m.add(root, "暫停(&P)\tCtrl+Alt+P", TrayCommand::Pause, st.rec != RecorderState::Recording, false);
        }
        if st.rec == RecorderState::Countdown {
            m.add(root, "取消倒數(&S)", TrayCommand::Stop, false, false);
        } else {
            m.add(root, "停止並儲存(&S)\tCtrl+Alt+R", TrayCommand::Stop, idle || st.rec == RecorderState::Stopping, false);
        }
        Menu::sep(root);
        m.add(root, "截圖(&T)\tCtrl+Alt+S", TrayCommand::Screenshot, !st.can_shot, false);
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

/// 啟動系統匣執行緒；圖示加入成功後回傳控制介面與快捷鍵登記結果
pub fn start(cmd: UnboundedSender<TrayCommand>) -> Result<(Arc<dyn TrayUi>, HotkeyStatus), String> {
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
            let added = TRAY.with(|t| {
                t.borrow_mut().as_mut().map(|t| {
                    t.added = t.add_icon();
                    t.added
                })
            });
            if added != Some(true) {
                let _ = ready_tx.send(Err("Shell_NotifyIcon 失敗".into()));
                let _ = DestroyWindow(hwnd);
                return;
            }
            // 快捷鍵登記在這個執行緒的視窗上（WM_HOTKEY 會送到這裡）；被其他程式占用時登記失敗
            let ok: Vec<bool> = HOTKEYS.iter().map(|(id, vk)| RegisterHotKey(Some(hwnd), *id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, *vk).is_ok()).collect();
            let _ = ready_tx.send(Ok((hwnd.0 as isize, HotkeyStatus { record: ok[0], pause: ok[1], shot: ok[2] })));
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
