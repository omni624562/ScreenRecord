//! 步驟截圖（Windows）：用低階滑鼠攔截得知左鍵按下，截下那個螢幕、在點的位置畫一圈紅框，存成 step_NN.png。
//! 點到本程式自己的視窗（操作視窗、系統匣選單）不算。

use crate::steps::Step;
use crate::types::Rect;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetAncestor, GetWindowThreadProcessId, InternalGetWindowText, PeekMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT,
    MSG, MSLLHOOKSTRUCT, PM_REMOVE, WH_MOUSE_LL, WM_LBUTTONDOWN,
};

thread_local! {
    static CLICKS: RefCell<Vec<POINT>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_LBUTTONDOWN {
        let s = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        CLICKS.with(|c| c.borrow_mut().push(s.pt));
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// 進行中的步驟截圖
pub struct Session {
    stop: Arc<AtomicBool>,
    count: Arc<AtomicU32>,
    thread: Option<std::thread::JoinHandle<Vec<Step>>>,
}

impl Session {
    /// 開始：圖存在 dir/folder/ 裡，文件裡的路徑是 folder/step_NN.png
    pub fn start(dir: PathBuf, folder: String) -> std::io::Result<Session> {
        std::fs::create_dir_all(dir.join(&folder))?;
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicU32::new(0));
        let (s, c) = (stop.clone(), count.clone());
        let thread = std::thread::Builder::new().name("steps".into()).spawn(move || unsafe { run(&dir, &folder, &s, &c) })?;
        Ok(Session { stop, count, thread: Some(thread) })
    }

    /// 已經截了幾步
    pub fn count(&self) -> u32 {
        self.count.load(Ordering::Relaxed)
    }

    /// 停止並取回所有步驟
    pub fn finish(mut self) -> Vec<Step> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().and_then(|t| t.join().ok()).unwrap_or_default()
    }
}

unsafe fn run(dir: &std::path::Path, folder: &str, stop: &AtomicBool, count: &AtomicU32) -> Vec<Step> {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst.into()), 0) {
        Ok(h) => h,
        Err(e) => {
            crate::warn!("步驟截圖：無法攔截滑鼠點擊：{e}");
            return vec![];
        }
    };
    let mut steps: Vec<Step> = vec![];
    let mut last: Option<(POINT, Instant)> = None;
    let mut msg = MSG::default();
    while !stop.load(Ordering::Relaxed) {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let clicks = CLICKS.with(|c| std::mem::take(&mut *c.borrow_mut()));
        for pt in clicks {
            // 連點（雙擊）只算一次
            if last.is_some_and(|(p, t)| t.elapsed() < Duration::from_millis(450) && (p.x - pt.x).abs() < 6 && (p.y - pt.y).abs() < 6) {
                continue;
            }
            last = Some((pt, Instant::now()));
            let root = GetAncestor(WindowFromPoint(pt), GA_ROOT);
            let mut pid = 0u32;
            GetWindowThreadProcessId(root, Some(&mut pid));
            if pid == std::process::id() {
                continue;
            }
            let mut buf = [0u16; 256];
            let n = InternalGetWindowText(root, &mut buf);
            let window = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                continue;
            }
            let m = mi.rcMonitor;
            let area = Rect { x: m.left, y: m.top, width: m.right - m.left, height: m.bottom - m.top };
            let Some(rgba) = crate::scroll_win::grab(area) else { continue };
            let n = steps.len() + 1;
            let name = format!("step_{n:02}.png");
            if let Err(e) = save(&rgba, area, pt, &dir.join(folder).join(&name)) {
                crate::warn!("步驟截圖：無法儲存第 {n} 步：{e}");
                continue;
            }
            steps.push(Step { time: chrono::Local::now().format("%H:%M:%S").to_string(), window, image: format!("{folder}/{name}") });
            count.store(steps.len() as u32, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let _ = UnhookWindowsHookEx(hook);
    steps
}

/// 在點的位置畫一圈紅框後存檔
fn save(rgba: &[u8], area: Rect, pt: POINT, path: &std::path::Path) -> Result<(), String> {
    let mut pm = Pixmap::new(area.width as u32, area.height as u32).ok_or("圖片太大")?;
    pm.data_mut().copy_from_slice(rgba);
    let (cx, cy) = ((pt.x - area.x) as f32, (pt.y - area.y) as f32);
    let r = (area.width.min(area.height) as f32 * 0.025).clamp(18.0, 48.0);
    if let Some(circle) = PathBuilder::from_circle(cx, cy, r) {
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(Color::from_rgba8(229, 72, 77, 50));
        pm.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), None);
        paint.set_color(Color::from_rgba8(229, 72, 77, 255));
        pm.stroke_path(&circle, &paint, &Stroke { width: (r / 6.0).max(3.0), ..Default::default() }, Transform::identity(), None);
    }
    pm.save_png(path).map_err(|e| e.to_string())
}
