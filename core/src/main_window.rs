//! 操作視窗的位置與大小：開啟時放進螢幕上可以用的範圍（扣掉工作列、標題列），放不下就最大化；
//! 記住上次的位置；螢幕改變（改解析度、縮放比例、拔掉外接螢幕、工作列變高）時把視窗拉回看得到的地方。
//! 計算都在這裡（任何平台都能測試）；Windows 上實際搬動視窗的部分在下面的 `win`。

use serde::{Deserialize, Serialize};

/// 範圍（實際像素、螢幕座標）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// 和另一個範圍重疊的面積
    pub fn overlap(&self, o: &Area) -> i64 {
        let w = (self.right().min(o.right()) - self.x.max(o.x)).max(0) as i64;
        let h = (self.bottom().min(o.bottom()) - self.y.max(o.y)).max(0) as i64;
        w * h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// 一個螢幕：整個範圍、可以用的範圍（扣掉工作列）、縮放（DPI，96 = 100%）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub area: Area,
    pub work: Area,
    pub dpi: u32,
}

/// 視窗外框比內容多出來的寬度（標題列、邊框；實際像素，依 DPI）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Frame {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Frame {
    /// 左、右、下的邊框大部分看不見（Windows 10、11 只畫 1 像素，其餘是拉動大小用的透明區域），
    /// 超出可用範圍也沒關係（和 Windows 把視窗貼齊螢幕邊緣時一樣）
    fn hidden(&self) -> i32 {
        (self.l - 1).max(0)
    }
}

/// 視窗的位置（外框，含看不見的邊框）與狀態；記在設定裡，下次開啟時用
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// 最大化（x、y、w、h 是還原後的位置）
    #[serde(default)]
    pub max: bool,
    /// 當時那個螢幕的縮放；改了縮放比例就不沿用大小
    pub dpi: u32,
}

impl Placement {
    pub fn area(&self) -> Area {
        Area { x: self.x, y: self.y, w: self.w, h: self.h }
    }

    /// 設定檔裡讀到的值合不合理
    pub fn valid(&self) -> bool {
        (100..=100_000).contains(&self.w) && (100..=100_000).contains(&self.h) && (48..=960).contains(&self.dpi) && self.x.abs() < 1_000_000 && self.y.abs() < 1_000_000
    }
}

/// 視窗可以放的範圍：可用範圍，左、右、下再加上看不見的邊框
pub fn usable(s: &Screen, f: &Frame) -> Area {
    let e = f.hidden();
    Area { x: s.work.x - e, y: s.work.y, w: s.work.w + 2 * e, h: s.work.h + e }
}

/// 放進範圍裡：太大就縮小，超出就移回來
pub fn fit_into(r: Area, room: Area) -> Area {
    let w = r.w.min(room.w).max(1);
    let h = r.h.min(room.h).max(1);
    let x = r.x.min(room.right() - w).max(room.x);
    let y = r.y.min(room.bottom() - h).max(room.y);
    Area { x, y, w, h }
}

/// 和這個範圍重疊最多的螢幕（完全沒有重疊時 None）
pub fn screen_of<'a>(screens: &'a [Screen], r: &Area) -> Option<&'a Screen> {
    screens.iter().map(|s| (s, s.area.overlap(r))).filter(|(_, o)| *o > 0).max_by_key(|(_, o)| *o).map(|(s, _)| s)
}

/// 這一點所在的螢幕；不在任何螢幕上時用主螢幕（左上角是 0, 0 的那個），再沒有就第一個
pub fn screen_at(screens: &[Screen], x: i32, y: i32) -> Option<&Screen> {
    screens.iter().find(|s| s.area.contains(x, y)).or_else(|| screens.iter().find(|s| s.area.x == 0 && s.area.y == 0)).or(screens.first())
}

/// 開啟時的位置。記住的位置還在螢幕上就用（太大縮小、超出移回來）；
/// 沒有記住、那個螢幕已經不在、或縮放比例改了：開在滑鼠所在的螢幕，放得下 `want`（內容的大小，以 100% 縮放計）
/// 就在可用範圍置中，放不下就最大化（還原後是放得下的最大大小）
pub fn plan(screens: &[Screen], cursor: (i32, i32), saved: Option<Placement>, want: (f32, f32), frame: impl Fn(u32) -> Frame) -> Option<Placement> {
    let saved = saved.filter(Placement::valid);
    let saved_on = saved.and_then(|p| screen_of(screens, &p.area()).map(|s| (p, s)));
    if let Some((p, s)) = saved_on {
        if s.dpi == p.dpi {
            let r = fit_into(p.area(), usable(s, &frame(s.dpi)));
            return Some(Placement { x: r.x, y: r.y, w: r.w, h: r.h, max: p.max, dpi: s.dpi });
        }
    }
    let s = saved_on.map(|(_, s)| s).or_else(|| screen_at(screens, cursor.0, cursor.1))?;
    let f = frame(s.dpi);
    let scale = s.dpi as f32 / 96.0;
    let w = (want.0 * scale).round() as i32 + f.l + f.r;
    let h = (want.1 * scale).round() as i32 + f.t + f.b;
    let room = usable(s, &f);
    let fits = w <= room.w && h <= room.h;
    // 在可用範圍（不含看不見的邊框）置中，再放進可以放的範圍
    let centered = Area { x: s.work.x + (s.work.w - w) / 2, y: s.work.y + (s.work.h - h) / 2, w, h };
    let r = fit_into(centered, room);
    Some(Placement { x: r.x, y: r.y, w: r.w, h: r.h, max: !fits, dpi: s.dpi })
}

/// 最大化時內容的大小（以 100% 縮放計）：可用範圍扣掉標題列
pub fn max_content(s: &Screen, f: &Frame) -> (f32, f32) {
    let scale = s.dpi.max(1) as f32 / 96.0;
    (s.work.w as f32 / scale, (s.work.h - (f.t - f.b)).max(0) as f32 / scale)
}

/// 螢幕配置的摘要：有變（接上、拔掉、改解析度或縮放、工作列變了）時要檢查視窗還看不看得到
pub fn signature(screens: &[Screen]) -> String {
    let mut v: Vec<String> = screens.iter().map(|s| format!("{:?} {:?} {}", s.area, s.work, s.dpi)).collect();
    v.sort();
    v.join("|")
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::Mutex;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindowVisible, IsZoomed, SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, SWP_NOACTIVATE, SWP_NOZORDER, SW_MAXIMIZE,
        WINDOW_EX_STYLE, WINDOW_STYLE,
    };

    static MAIN: AtomicIsize = AtomicIsize::new(0);
    /// 最後一次看到的還原（不是最大化、縮小）時的位置：最大化時記這個
    static NORMAL: Mutex<Option<Area>> = Mutex::new(None);

    fn hwnd() -> Option<HWND> {
        let h = MAIN.load(Ordering::Relaxed);
        (h != 0).then_some(HWND(h as *mut _))
    }

    fn area(r: RECT) -> Area {
        Area { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    }

    fn info(m: HMONITOR) -> Option<Screen> {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: 只查詢螢幕資訊
        unsafe {
            if !GetMonitorInfoW(m, &mut mi).as_bool() {
                return None;
            }
            let (mut dx, mut dy) = (96u32, 96u32);
            if GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_err() || dx == 0 {
                dx = 96;
            }
            Some(Screen { area: area(mi.rcMonitor), work: area(mi.rcWork), dpi: dx })
        }
    }

    pub fn screens() -> Vec<Screen> {
        unsafe extern "system" fn visit(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
            let list = &mut *(data.0 as *mut Vec<Screen>);
            if let Some(s) = info(m) {
                list.push(s);
            }
            true.into()
        }
        let mut list: Vec<Screen> = vec![];
        // SAFETY: visit 只在這個呼叫期間使用 list
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(visit), LPARAM(&mut list as *mut Vec<Screen> as isize));
        }
        list
    }

    /// 這個縮放下視窗外框比內容多出來的寬度（依操作視窗實際的樣式）
    pub fn frame(dpi: u32) -> Frame {
        let mut r = RECT::default();
        // SAFETY: 只讀取視窗樣式、計算外框
        unsafe {
            let (style, ex) = match hwnd() {
                Some(h) => (WINDOW_STYLE(GetWindowLongPtrW(h, GWL_STYLE) as u32), WINDOW_EX_STYLE(GetWindowLongPtrW(h, GWL_EXSTYLE) as u32)),
                None => (windows::Win32::UI::WindowsAndMessaging::WS_OVERLAPPEDWINDOW, WINDOW_EX_STYLE(0)),
            };
            if AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi).is_err() {
                let s = dpi as i32;
                return Frame { l: 8 * s / 96, t: 31 * s / 96, r: 8 * s / 96, b: 8 * s / 96 };
            }
        }
        Frame { l: -r.left, t: -r.top, r: r.right, b: r.bottom }
    }

    fn cursor() -> (i32, i32) {
        let mut p = POINT::default();
        // SAFETY: 只讀取游標位置
        let _ = unsafe { GetCursorPos(&mut p) };
        (p.x, p.y)
    }

    fn rect_of(h: HWND) -> Option<Area> {
        let mut r = RECT::default();
        // SAFETY: 只讀取視窗位置
        unsafe { GetWindowRect(h, &mut r) }.ok().map(|_| area(r))
    }

    fn move_to(h: HWND, r: Area) {
        // 移到縮放比例不同的螢幕時 Windows 會依新的縮放改大小：再設一次，大小才是算好的
        for _ in 0..2 {
            // SAFETY: 搬動自己的視窗
            unsafe {
                let _ = SetWindowPos(h, None, r.x, r.y, r.w, r.h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            if rect_of(h) == Some(r) {
                break;
            }
        }
    }

    pub fn attach(h: isize) {
        MAIN.store(h, Ordering::Relaxed);
    }

    pub fn place_at_start(saved: Option<Placement>, want: (f32, f32)) -> bool {
        let Some(h) = hwnd() else { return false };
        let list = screens();
        let Some(p) = plan(&list, cursor(), saved, want, frame) else { return false };
        let on = screen_of(&list, &p.area()).map(|s| format!("{}×{}（可用 {}×{}）、縮放 {}%", s.area.w, s.area.h, s.work.w, s.work.h, s.dpi * 100 / 96)).unwrap_or_default();
        crate::info!(
            "操作視窗開啟位置：{},{} {}×{}{}；螢幕 {on}{}",
            p.x,
            p.y,
            p.w,
            p.h,
            if p.max { "、最大化" } else { "" },
            if saved.is_some_and(|s| s.area() == p.area()) { "（上次的位置）" } else { "" }
        );
        move_to(h, p.area());
        *NORMAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.area());
        p.max
    }

    pub fn current() -> Option<Placement> {
        let h = hwnd()?;
        // SAFETY: 只查詢視窗狀態
        let (shown, min, max) = unsafe { (IsWindowVisible(h).as_bool(), IsIconic(h).as_bool(), IsZoomed(h).as_bool()) };
        if !shown || min {
            return None;
        }
        let mut normal = NORMAL.lock().unwrap_or_else(|e| e.into_inner());
        if !max {
            *normal = rect_of(h);
        }
        let r = (*normal)?;
        // SAFETY: 只查詢視窗所在的螢幕
        let dpi = info(unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) })?.dpi;
        Some(Placement { x: r.x, y: r.y, w: r.w, h: r.h, max, dpi })
    }

    pub fn keep_on_screen() {
        let Some(h) = hwnd() else { return };
        // 最大化的視窗 Windows 會自己處理；縮小的等還原時再檢查
        // SAFETY: 只查詢視窗狀態
        if unsafe { IsIconic(h).as_bool() || IsZoomed(h).as_bool() } {
            return;
        }
        let Some(r) = rect_of(h) else { return };
        let list = screens();
        let Some(s) = screen_of(&list, &r).or_else(|| {
            let c = cursor();
            screen_at(&list, c.0, c.1)
        }) else {
            return;
        };
        let room = usable(s, &frame(s.dpi));
        let fit = fit_into(r, room);
        if fit == r {
            return;
        }
        crate::info!("操作視窗超出螢幕，移回看得到的地方：{},{} {}×{} → {},{} {}×{}", r.x, r.y, r.w, r.h, fit.x, fit.y, fit.w, fit.h);
        move_to(h, fit);
        // 比最小大小還小，放不下：最大化
        if rect_of(h).is_some_and(|now| now.w > room.w || now.h > room.h) {
            // SAFETY: 最大化自己的視窗
            unsafe {
                let _ = ShowWindow(h, SW_MAXIMIZE);
            }
        }
    }

    pub fn max_content_here() -> Option<(f32, f32)> {
        let h = hwnd()?;
        // SAFETY: 只查詢視窗所在的螢幕
        let s = info(unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) })?;
        Some(max_content(&s, &frame(s.dpi)))
    }

    pub fn screens_signature() -> String {
        signature(&screens())
    }
}

/// 記下操作視窗（建立後、第一次顯示前呼叫）
pub fn attach(hwnd: isize) {
    #[cfg(windows)]
    win::attach(hwnd);
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// 把還沒顯示的操作視窗放到開啟的位置；回傳要不要最大化（第一個畫面顯示之後再最大化）
pub fn place_at_start(saved: Option<Placement>, want: (f32, f32)) -> bool {
    #[cfg(windows)]
    return win::place_at_start(saved, want);
    #[cfg(not(windows))]
    {
        let _ = (saved, want);
        false
    }
}

/// 目前的位置（要記下來的）；隱藏、縮小時 None
pub fn current() -> Option<Placement> {
    #[cfg(windows)]
    return win::current();
    #[cfg(not(windows))]
    None
}

/// 視窗超出螢幕上可用的範圍（螢幕改變了、從系統匣叫回來時）：縮小、移回來；還是放不下就最大化
pub fn keep_on_screen() {
    #[cfg(windows)]
    win::keep_on_screen();
}

/// 視窗所在的螢幕最大化時內容的大小（以 100% 縮放計）；不知道時 None
pub fn max_content_here() -> Option<(f32, f32)> {
    #[cfg(windows)]
    return win::max_content_here();
    #[cfg(not(windows))]
    None
}

/// 目前螢幕配置的摘要（比對有沒有改變）
pub fn screens_signature() -> String {
    #[cfg(windows)]
    return win::screens_signature();
    #[cfg(not(windows))]
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 96 DPI 時 Windows 10、11 一般視窗的外框
    fn frame(dpi: u32) -> Frame {
        let s = |v: i32| v * dpi as i32 / 96;
        Frame { l: s(8), t: s(31), r: s(8), b: s(8) }
    }

    /// 工作列在下面（48 像素 × 縮放）
    fn screen(x: i32, w: i32, h: i32, dpi: u32) -> Screen {
        let bar = 48 * dpi as i32 / 96;
        Screen { area: Area { x, y: 0, w, h }, work: Area { x, y: 0, w, h: h - bar }, dpi }
    }

    const WANT: (f32, f32) = (1280.0, 900.0);

    #[test]
    fn big_screen_centers_in_work_area() {
        let s = [screen(0, 1920, 1080, 96)];
        let p = plan(&s, (500, 500), None, WANT, frame).unwrap();
        assert!(!p.max);
        assert_eq!((p.w, p.h), (1280 + 16, 900 + 39));
        assert_eq!(p.x, (1920 - 1296) / 2);
        // 整個視窗（含標題列）都在工作列上面
        assert!(p.y >= 0 && p.y + p.h <= 1032);
    }

    #[test]
    fn small_screen_maximizes_and_never_goes_under_taskbar() {
        // 1920×1080、150%：可用 1920×1008（實際像素），1280×900 放不下
        let s = [screen(0, 1920, 1080, 144)];
        let p = plan(&s, (10, 10), None, WANT, frame).unwrap();
        assert!(p.max);
        let room = usable(&s[0], &frame(144));
        assert!(p.x >= room.x && p.right() <= room.right() && p.y >= 0 && p.bottom() <= room.bottom());
        // 看得到的部分（扣掉看不見的邊框）在工作列上面
        assert!(p.bottom() - (frame(144).l - 1) <= 1008);
    }

    impl Placement {
        fn right(&self) -> i32 {
            self.x + self.w
        }
        fn bottom(&self) -> i32 {
            self.y + self.h
        }
    }

    #[test]
    fn opens_on_the_screen_with_the_mouse() {
        let s = [screen(0, 1920, 1080, 96), screen(1920, 2560, 1440, 96)];
        let p = plan(&s, (2500, 300), None, WANT, frame).unwrap();
        assert!(p.x >= 1920 && p.right() <= 1920 + 2560);
        // 滑鼠不在任何螢幕上：主螢幕
        let p = plan(&s, (-5000, 300), None, WANT, frame).unwrap();
        assert!(p.right() <= 1920);
    }

    #[test]
    fn remembered_place_is_reused_and_kept_on_screen() {
        let s = [screen(0, 1920, 1080, 96), screen(1920, 2560, 1440, 96)];
        let saved = Placement { x: 2100, y: 100, w: 1500, h: 1000, max: true, dpi: 96 };
        // 滑鼠在別的螢幕也用記住的位置
        assert_eq!(plan(&s, (10, 10), Some(saved), WANT, frame), Some(saved));
        // 有一半跑到下面去了：移回來，大小不變
        let off = Placement { y: 900, ..saved };
        let p = plan(&s, (10, 10), Some(off), WANT, frame).unwrap();
        assert_eq!((p.w, p.h), (1500, 1000));
        assert!(p.bottom() <= 1392 + 7);
        // 比螢幕還大：縮小
        let big = Placement { x: 0, y: 0, w: 3000, h: 2000, max: false, dpi: 96 };
        let p = plan(&s[..1], (10, 10), Some(big), WANT, frame).unwrap();
        assert!(p.w <= 1920 + 14 && p.h <= 1032 + 7);
    }

    #[test]
    fn remembered_place_on_unplugged_screen_or_new_scale_starts_fresh() {
        let one = [screen(0, 1920, 1080, 96)];
        let saved = Placement { x: 2100, y: 100, w: 1500, h: 1000, max: false, dpi: 96 };
        let p = plan(&one, (10, 10), Some(saved), WANT, frame).unwrap();
        assert_eq!((p.w, p.h), (1296, 939));
        assert!(p.right() <= 1920);
        // 同一個螢幕改成 150%：用那個螢幕重新計算（放不下，最大化）
        let scaled = [screen(0, 1920, 1080, 144)];
        let p = plan(&scaled, (10, 10), Some(Placement { x: 100, ..saved }), WANT, frame).unwrap();
        assert!(p.max);
        assert_eq!(p.dpi, 144);
        // 讀到奇怪的值：不用
        let bad = Placement { w: 0, ..saved };
        assert!(!bad.valid());
        assert_eq!(plan(&one, (10, 10), Some(bad), WANT, frame), plan(&one, (10, 10), None, WANT, frame));
    }

    #[test]
    fn no_screens_means_no_plan() {
        assert_eq!(plan(&[], (0, 0), None, WANT, frame), None);
    }

    #[test]
    fn fit_into_shrinks_and_moves() {
        let room = Area { x: 0, y: 0, w: 1000, h: 700 };
        assert_eq!(fit_into(Area { x: 900, y: 650, w: 400, h: 300 }, room), Area { x: 600, y: 400, w: 400, h: 300 });
        assert_eq!(fit_into(Area { x: -50, y: -10, w: 2000, h: 300 }, room), Area { x: 0, y: 0, w: 1000, h: 300 });
        assert_eq!(fit_into(Area { x: 10, y: 10, w: 100, h: 100 }, room), Area { x: 10, y: 10, w: 100, h: 100 });
    }

    #[test]
    fn max_content_leaves_out_the_title_bar() {
        let s = screen(0, 1920, 1080, 144);
        let (w, h) = max_content(&s, &frame(144));
        assert_eq!(w, 1280.0);
        // 1008 - 標題列 (46 - 12 = 34) = 974 → 649.3
        assert!((h - 974.0 / 1.5).abs() < 0.01);
    }

    #[test]
    fn signature_changes_with_work_area() {
        let a = [screen(0, 1920, 1080, 96)];
        let mut b = a;
        b[0].work.h -= 20;
        assert_ne!(signature(&a), signature(&b));
        assert_eq!(signature(&a), signature(&a.clone()));
    }
}
