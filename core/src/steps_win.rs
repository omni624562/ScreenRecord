//! 步驟截圖（Windows）：用低階滑鼠攔截得知左鍵按下，截下那個螢幕、在點的位置畫一圈紅框，存成 step_NN.png。
//! 點到本程式自己的視窗（操作視窗、系統匣選單、下方的小工具列）不算。
//! 截圖時畫面下方有小工具列「步驟截圖中・已截 N 步 [完成] [取消]」（不會被截進去）。

use crate::bubble_win::{Bubble, Options};
use crate::llhook_win::{Hook, HookEvent};
use crate::steps::{Step, BAR_CANCEL, BAR_DISCARD, BAR_FINISH, BAR_KEEP};
use crate::types::Rect;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetWindowThreadProcessId, InternalGetWindowText, WindowFromPoint, GA_ROOT};

/// 小工具列上按了什麼（在小工具列的執行緒上呼叫）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarCmd {
    Finish,
    Discard,
}

pub type OnBar = Arc<dyn Fn(BarCmd) + Send + Sync>;

/// 進行中的步驟截圖
pub struct Session {
    stop: Arc<AtomicBool>,
    count: Arc<AtomicU32>,
    thread: Option<std::thread::JoinHandle<Vec<Step>>>,
}

impl Session {
    /// 開始：圖存在 dir/folder/ 裡，步驟記的路徑是 folder/step_NN.png；小工具列按了完成或放棄時呼叫 on_bar
    pub fn start(dir: PathBuf, folder: String, on_bar: OnBar) -> std::io::Result<Session> {
        std::fs::create_dir_all(dir.join(&folder))?;
        let stop = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicU32::new(0));
        let (s, c) = (stop.clone(), count.clone());
        let thread = std::thread::Builder::new().name("steps".into()).spawn(move || unsafe { run(&dir, &folder, &s, c, on_bar) })?;
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

/// 攔截在專用執行緒（llhook_win）：這裡截圖、存檔花多久都不會讓滑鼠變卡
unsafe fn run(dir: &std::path::Path, folder: &str, stop: &AtomicBool, count: Arc<AtomicU32>, on_bar: OnBar) -> Vec<Step> {
    let Some((hook, rx)) = Hook::start(false) else {
        crate::warn!("步驟截圖：無法攔截滑鼠點擊");
        return vec![];
    };
    // 小工具列：按「取消」時已經有步驟就先問（放棄／繼續截圖），內容在這個迴圈裡更新
    let confirm = Arc::new(AtomicBool::new(false));
    let (cf, n) = (confirm.clone(), count.clone());
    let bar = Bubble::show(
        crate::steps::bar(0, false),
        Options {
            clickable: true,
            esc: false,
            near: None,
            bottom: true,
            on: Some(Box::new(move |id| match id {
                BAR_FINISH => on_bar(BarCmd::Finish),
                BAR_CANCEL if n.load(Ordering::Relaxed) == 0 => on_bar(BarCmd::Discard),
                BAR_CANCEL => cf.store(true, Ordering::Relaxed),
                BAR_DISCARD => on_bar(BarCmd::Discard),
                BAR_KEEP => cf.store(false, Ordering::Relaxed),
                _ => {}
            })),
        },
    );
    let mut steps: Vec<Step> = vec![];
    let mut last: Option<(POINT, Instant)> = None;
    while !stop.load(Ordering::Relaxed) {
        bar.set(crate::steps::bar(steps.len() as u32, confirm.load(Ordering::Relaxed)));
        let pt = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(HookEvent::MouseDown { x, y, right: false }) => POINT { x, y },
            Ok(_) | Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        // 連點（按兩下）只算一次
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
        steps.push(Step { time: chrono::Local::now().format("%H:%M:%S").to_string(), window, image: format!("{folder}/{name}"), text: String::new() });
        count.store(steps.len() as u32, Ordering::Relaxed);
    }
    drop(bar);
    drop(hook);
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
