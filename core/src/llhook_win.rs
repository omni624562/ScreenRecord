//! 低階滑鼠／鍵盤攔截（Windows）：放在只處理訊息的專用執行緒，攔截函式只把事件丟進通道就傳回。
//!
//! 低階攔截只有在「裝攔截的那條執行緒」取訊息時才會被呼叫。那條執行緒如果同時在做別的事
//! （截圖、存檔、畫畫面、等鎖、sleep），整台電腦的滑鼠鍵盤都要等它，游標會卡；
//! 超過 LowLevelHooksTimeout 還會被 Windows 默默移除，之後就再也收不到點擊。
//! 所以攔截一律放在這裡的專用執行緒，使用的那一方從通道收事件，愛做多久都不影響輸入。

use crate::types::shown_keys;
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN,
};

/// 攔截到的事件
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    /// 滑鼠按下（桌面座標）；right = 右鍵
    MouseDown { x: i32, y: i32, right: bool },
    /// 要顯示的按鍵組合（例如「Ctrl + C」；一般打字不會有）
    Key(String),
}

thread_local! {
    static SENDER: RefCell<Option<Sender<HookEvent>>> = const { RefCell::new(None) };
}

fn send(ev: HookEvent) {
    SENDER.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.send(ev);
        }
    });
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let m = wparam.0 as u32;
        if m == WM_LBUTTONDOWN || m == WM_RBUTTONDOWN {
            let s = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            send(HookEvent::MouseDown { x: s.pt.x, y: s.pt.y, right: m == WM_RBUTTONDOWN });
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
        let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let down = |vk: i32| (GetAsyncKeyState(vk) as u16 & 0x8000) != 0;
        let (ctrl, alt, shift, win) = (down(0x11), down(0x12), down(0x10), down(0x5B) || down(0x5C));
        if let Some(label) = shown_keys(k.vkCode, ctrl, alt, shift, win) {
            send(HookEvent::Key(label));
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// 進行中的攔截；丟掉時停止（送 WM_QUIT 給攔截執行緒並等它結束）
pub struct Hook {
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Hook {
    /// 開始攔截滑鼠（keys = 也攔截鍵盤）；失敗時回傳 None（已寫進記錄檔）
    pub fn start(keys: bool) -> Option<(Hook, Receiver<HookEvent>)> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Option<u32>>();
        let join = std::thread::Builder::new()
            .name("input-hook".into())
            .spawn(move || unsafe {
                // 先建立這條執行緒的訊息佇列，之後才能用 PostThreadMessage 叫它結束
                let mut msg = MSG::default();
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                SENDER.with(|s| *s.borrow_mut() = Some(tx));
                let hinst = GetModuleHandleW(None).unwrap_or_default();
                let mouse = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst.into()), 0) {
                    Ok(h) => h,
                    Err(e) => {
                        crate::warn!("無法攔截滑鼠點擊：{e}");
                        let _ = ready_tx.send(None);
                        return;
                    }
                };
                let key = if keys { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), Some(hinst.into()), 0).map_err(|e| crate::warn!("無法攔截按鍵：{e}")).ok() } else { None };
                let _ = ready_tx.send(Some(GetCurrentThreadId()));
                // 只處理訊息：攔截函式在這裡被呼叫
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = UnhookWindowsHookEx(mouse);
                if let Some(k) = key {
                    let _ = UnhookWindowsHookEx(k);
                }
                SENDER.with(|s| *s.borrow_mut() = None);
            })
            .ok()?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Some(thread_id)) => Some((Hook { thread_id, join: Some(join) }, rx)),
            _ => None,
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}
