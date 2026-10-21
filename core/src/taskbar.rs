//! Windows 工作列上程式圖示的進度條：製作、壓縮、傳檔、下載時顯示做到哪裡，切到別的程式也看得到；
//! 做完時程式不在前面就閃一下。只在介面執行緒呼叫（工作列的 COM 物件跟著建立它的執行緒）。

/// 工作列進度條要顯示的
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    None,
    /// 0～1
    Normal(f64),
    /// 在做但不知道做到哪裡（等 Jellyfin 整理）
    Busy,
}

/// 好幾件事同時在做：有比例的一起算（依各自的份量），只有不知道進度的時顯示「忙碌中」
pub fn combine(parts: &[(f64, f64)], busy: bool) -> Progress {
    let total: f64 = parts.iter().map(|(_, w)| w.max(0.0)).sum();
    if total > 0.0 {
        let done: f64 = parts.iter().map(|(f, w)| f.clamp(0.0, 1.0) * w.max(0.0)).sum();
        return Progress::Normal(done / total);
    }
    if !parts.is_empty() || busy {
        Progress::Busy
    } else {
        Progress::None
    }
}

/// 設定進度條（和上次一樣就不動）
pub fn set(p: Progress) {
    #[cfg(windows)]
    win::set(p);
    #[cfg(not(windows))]
    let _ = p;
}

/// 做完了：程式不在最前面時，工作列的圖示閃幾下
pub fn flash() {
    #[cfg(windows)]
    win::flash();
}

#[cfg(windows)]
mod win {
    use super::Progress;
    use std::cell::RefCell;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL};
    use windows::Win32::UI::WindowsAndMessaging::{FlashWindowEx, GetForegroundWindow, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY};

    thread_local! {
        /// 工作列物件（建立失敗時 None，不再試）與上次設定的進度
        static BAR: RefCell<Option<(Option<ITaskbarList3>, Progress)>> = const { RefCell::new(None) };
    }

    fn hwnd() -> Option<HWND> {
        let h = crate::main_window::handle();
        (h != 0).then_some(HWND(h as *mut _))
    }

    pub fn set(p: Progress) {
        let Some(h) = hwnd() else { return };
        BAR.with(|b| {
            let mut b = b.borrow_mut();
            let (bar, last) = b.get_or_insert_with(|| {
                // SAFETY: 在介面執行緒建立工作列物件（STA）；已經初始化過 COM 時回傳的錯誤可以忽略
                let bar = unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok().filter(|t| t.HrInit().is_ok())
                };
                (bar, Progress::None)
            });
            // 比例只差一點點（不到 0.5%）不用重設
            let same = match (*last, p) {
                (Progress::Normal(a), Progress::Normal(b)) => (a - b).abs() < 0.005,
                (a, b) => a == b,
            };
            let Some(bar) = bar.as_ref().filter(|_| !same) else { return };
            *last = p;
            // SAFETY: 設定自己視窗的工作列進度
            unsafe {
                match p {
                    Progress::None => {
                        let _ = bar.SetProgressState(h, TBPF_NOPROGRESS);
                    }
                    Progress::Busy => {
                        let _ = bar.SetProgressState(h, TBPF_INDETERMINATE);
                    }
                    Progress::Normal(f) => {
                        let _ = bar.SetProgressState(h, TBPF_NORMAL);
                        let _ = bar.SetProgressValue(h, (f.clamp(0.0, 1.0) * 1000.0) as u64, 1000);
                    }
                }
            }
        });
    }

    pub fn flash() {
        let Some(h) = hwnd() else { return };
        // SAFETY: 讀前景視窗、閃自己的工作列圖示
        unsafe {
            if GetForegroundWindow() == h {
                return;
            }
            let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd: h, dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 3, dwTimeout: 0 };
            let _ = FlashWindowEx(&info);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_jobs_by_size() {
        assert_eq!(combine(&[], false), Progress::None);
        assert_eq!(combine(&[], true), Progress::Busy);
        // 一個做了一半（份量 1）、一個剛開始（份量 3）：1/8
        assert_eq!(combine(&[(0.5, 1.0), (0.0, 3.0)], false), Progress::Normal(0.125));
        // 不知道份量的：當作忙碌中
        assert_eq!(combine(&[(0.3, 0.0)], false), Progress::Busy);
    }
}
