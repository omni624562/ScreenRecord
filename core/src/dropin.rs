//! 從其他程式拖進操作視窗的東西（Windows）：檔案與文字。
//! winit 的拖放只收檔案，換成自己的 IDropTarget：檔案照樣交給介面（egui 的 hovered_files / dropped_files），
//! 文字（瀏覽器、記事本、Word 選好的字）在介面上看放在哪個輸入框，貼進去

use std::path::PathBuf;
use std::sync::Mutex;

/// 放下來的東西（座標是視窗內的實體像素）
pub enum Dropped {
    Files(Vec<PathBuf>),
    Text { text: String, x: f32, y: f32 },
}

#[derive(Default)]
struct Shared {
    /// 拖著檔案經過視窗
    hover_files: Vec<PathBuf>,
    /// 拖著文字經過視窗的位置
    hover_text: Option<(f32, f32)>,
    /// 介面說滑鼠下面是輸入框（可以放文字）
    accept_text: bool,
    dropped: Vec<Dropped>,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    f(SHARED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Shared::default))
}

/// 這一格要交給介面的（放下的檔案、文字）
pub fn take() -> Vec<Dropped> {
    with(|s| std::mem::take(&mut s.dropped))
}

/// 正拖著經過視窗的檔案
pub fn hovered_files() -> Vec<PathBuf> {
    with(|s| s.hover_files.clone())
}

/// 正拖著文字經過視窗的位置（實體像素）
pub fn text_hover() -> Option<(f32, f32)> {
    with(|s| s.hover_text)
}

/// 介面看完滑鼠下面是不是輸入框：不是的話游標顯示不能放
pub fn set_accept_text(on: bool) {
    with(|s| s.accept_text = on);
}

/// 接手操作視窗的拖放（取代 winit 的）；wake 讓介面馬上畫下一格
pub fn install(hwnd: isize, wake: impl Fn() + Send + Sync + 'static) {
    #[cfg(windows)]
    win::install(hwnd, Box::new(wake));
    #[cfg(not(windows))]
    let _ = (hwnd, wake);
}

#[cfg(windows)]
mod win {
    use super::{with, Dropped};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use windows::core::{implement, Ref, Result};
    use windows::Win32::Foundation::{HWND, POINT, POINTL};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE};
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        None,
        Files,
        Text,
    }

    #[implement(IDropTarget)]
    struct Target {
        hwnd: isize,
        wake: Box<dyn Fn() + Send + Sync>,
        kind: Mutex<Kind>,
    }

    pub fn install(hwnd: isize, wake: Box<dyn Fn() + Send + Sync>) {
        let h = HWND(hwnd as *mut _);
        let target: IDropTarget = Target { hwnd, wake, kind: Mutex::new(Kind::None) }.into();
        // SAFETY: winit 已經在這個執行緒初始化 OLE、登記了它的拖放；換成我們的（RegisterDragDrop 會 AddRef）
        unsafe {
            let _ = RevokeDragDrop(h);
            if let Err(e) = RegisterDragDrop(h, &target) {
                crate::info!("[拖放] 無法接手拖放（只收檔案）：{e}");
            }
        }
    }

    fn format(cf: u16) -> FORMATETC {
        FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
    }

    fn has(data: &IDataObject, cf: u16) -> bool {
        // SAFETY: 只查詢有沒有這種格式
        unsafe { data.QueryGetData(&format(cf)).is_ok() }
    }

    fn kind_of(data: Option<&IDataObject>) -> Kind {
        match data {
            Some(d) if has(d, CF_HDROP.0) => Kind::Files,
            Some(d) if has(d, CF_UNICODETEXT.0) => Kind::Text,
            _ => Kind::None,
        }
    }

    fn files(data: &IDataObject) -> Vec<PathBuf> {
        let mut out = vec![];
        // SAFETY: CF_HDROP 的 HGLOBAL 就是 HDROP；用完交給 ReleaseStgMedium 釋放
        unsafe {
            let Ok(mut m) = data.GetData(&format(CF_HDROP.0)) else { return out };
            let drop = HDROP(m.u.hGlobal.0);
            let n = DragQueryFileW(drop, u32::MAX, None);
            for i in 0..n {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                let got = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got.min(len)])));
            }
            ReleaseStgMedium(&mut m);
        }
        out
    }

    fn text(data: &IDataObject) -> Option<String> {
        // SAFETY: CF_UNICODETEXT 是以 \0 結尾的 UTF-16；只讀到 GlobalSize 為止
        unsafe {
            let mut m = data.GetData(&format(CF_UNICODETEXT.0)).ok()?;
            let h = m.u.hGlobal;
            let p = GlobalLock(h) as *const u16;
            let s = (!p.is_null()).then(|| {
                let max = GlobalSize(h) / 2;
                let words = std::slice::from_raw_parts(p, max);
                let end = words.iter().position(|&c| c == 0).unwrap_or(max);
                String::from_utf16_lossy(&words[..end])
            });
            let _ = GlobalUnlock(h);
            ReleaseStgMedium(&mut m);
            s.filter(|t| !t.is_empty())
        }
    }

    impl Target {
        fn client(&self, pt: &POINTL) -> (f32, f32) {
            let mut p = POINT { x: pt.x, y: pt.y };
            // SAFETY: 只換算座標
            unsafe {
                let _ = ScreenToClient(HWND(self.hwnd as *mut _), &mut p);
            }
            (p.x as f32, p.y as f32)
        }

        fn effect(&self, pt: &POINTL) -> DROPEFFECT {
            let kind = *self.kind.lock().unwrap_or_else(|e| e.into_inner());
            match kind {
                Kind::Files => DROPEFFECT_COPY,
                Kind::Text => {
                    let at = self.client(pt);
                    let ok = with(|s| {
                        s.hover_text = Some(at);
                        s.accept_text
                    });
                    (self.wake)();
                    if ok {
                        DROPEFFECT_COPY
                    } else {
                        DROPEFFECT_NONE
                    }
                }
                Kind::None => DROPEFFECT_NONE,
            }
        }

        fn leave(&self) {
            *self.kind.lock().unwrap_or_else(|e| e.into_inner()) = Kind::None;
            with(|s| {
                s.hover_files.clear();
                s.hover_text = None;
                s.accept_text = false;
            });
            (self.wake)();
        }
    }

    impl IDropTarget_Impl for Target_Impl {
        fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let data = data.as_ref();
            let kind = kind_of(data);
            *self.kind.lock().unwrap_or_else(|e| e.into_inner()) = kind;
            if let (Kind::Files, Some(d)) = (kind, data) {
                let list = files(d);
                with(|s| s.hover_files = list);
                (self.wake)();
            }
            // SAFETY: effect 是 OLE 給的輸出參數
            unsafe { *effect = self.effect(pt) };
            Ok(())
        }

        fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            // SAFETY: 同上
            unsafe { *effect = self.effect(pt) };
            Ok(())
        }

        fn DragLeave(&self) -> Result<()> {
            self.leave();
            Ok(())
        }

        fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            let kind = *self.kind.lock().unwrap_or_else(|e| e.into_inner());
            let accept = with(|s| s.accept_text);
            let got = match (kind, data.as_ref()) {
                (Kind::Files, Some(d)) => Some(Dropped::Files(files(d))),
                (Kind::Text, Some(d)) if accept => text(d).map(|t| {
                    let (x, y) = self.client(pt);
                    Dropped::Text { text: t, x, y }
                }),
                _ => None,
            };
            // SAFETY: 同上
            unsafe { *effect = if got.is_some() { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
            self.leave();
            if let Some(d) = got {
                with(|s| s.dropped.push(d));
                (self.wake)();
            }
            Ok(())
        }
    }
}
