//! 錄影時開啟 Windows 的勿擾（Windows 10 的「專注輔助」）：只允許鬧鐘，LINE、Outlook 等程式的通知不會跳出來被錄進去；
//! 停止後恢復成原本的設定。Windows 沒有公開的 API，用系統內部的通知狀態（WNF_SHEL_QUIETHOURS_ACTIVE_PROFILE_CHANGED）。
//! 自己畫通知視窗、不經過 Windows 通知的程式（例如部分版本的 LINE）擋不住。

use std::sync::Mutex;

/// 開啟前的設定（None = 沒有由本程式開啟）
static PREVIOUS: Mutex<Option<u32>> = Mutex::new(None);

/// 勿擾的等級：0 = 關、1 = 只有優先通知、2 = 只有鬧鐘
pub const ALARMS_ONLY: u32 = 2;

/// 開啟勿擾（記住原本的設定）；已經開啟過就不重複。回傳是否成功
pub fn begin() -> bool {
    let mut prev = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    if prev.is_some() {
        return true;
    }
    let Some(cur) = imp::get() else { return false };
    if cur == ALARMS_ONLY {
        // 本來就是只允許鬧鐘：不用改，也不用還原
        return true;
    }
    if imp::set(ALARMS_ONLY) {
        *prev = Some(cur);
        // 記在檔案裡：錄影中程式當掉時，下次開啟可以恢復
        let _ = std::fs::write(marker(), cur.to_string());
        crate::info!("[勿擾] 錄影時開啟（原本是 {cur}）");
        true
    } else {
        crate::warn!("[勿擾] 無法開啟");
        false
    }
}

/// 恢復成錄影前的設定（不是由本程式開啟的就不動）
pub fn end() {
    let Some(prev) = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let _ = std::fs::remove_file(marker());
    if imp::set(prev) {
        crate::info!("[勿擾] 已恢復（{prev}）");
    } else {
        crate::warn!("[勿擾] 無法恢復成 {prev}");
    }
}

fn marker() -> std::path::PathBuf {
    crate::paths::data_dir().join("quiet_restore.txt")
}

/// 程式啟動時：上次錄影中當掉、勿擾沒有恢復時恢復（這次自己開啟的不動）
pub fn recover() {
    if PREVIOUS.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return;
    }
    let Ok(v) = std::fs::read_to_string(marker()) else { return };
    let _ = std::fs::remove_file(marker());
    if let Ok(prev) = v.trim().parse::<u32>() {
        if imp::get() == Some(ALARMS_ONLY) && imp::set(prev) {
            crate::info!("[勿擾] 上次錄影沒有正常結束，已恢復（{prev}）");
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use windows::core::{s, w};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

    /// 勿擾的設定改變時的通知狀態（Windows 10 1803 以後）
    const STATE: u64 = 0x0D83_063E_A3BF_1C75;

    type Query = unsafe extern "system" fn(*const u64, *const c_void, *const c_void, *mut u32, *mut c_void, *mut u32) -> i32;
    type Update = unsafe extern "system" fn(*const u64, *const c_void, u32, *const c_void, *const c_void, u32, u32) -> i32;

    fn proc(name: windows::core::PCSTR) -> Option<unsafe extern "system" fn() -> isize> {
        // SAFETY: ntdll 一定載入在每個行程裡；只查函式位址
        unsafe { GetProcAddress(GetModuleHandleW(w!("ntdll.dll")).ok()?, name) }
    }

    pub fn get() -> Option<u32> {
        let f: Query = unsafe { std::mem::transmute(proc(s!("NtQueryWnfStateData"))?) };
        let (mut stamp, mut value, mut size) = (0u32, 0u32, 4u32);
        // SAFETY: 參數照 NtQueryWnfStateData 的定義，緩衝區是 4 bytes
        let r = unsafe { f(&STATE, std::ptr::null(), std::ptr::null(), &mut stamp, &mut value as *mut u32 as *mut c_void, &mut size) };
        (r >= 0).then_some(if size == 0 { 0 } else { value })
    }

    pub fn set(v: u32) -> bool {
        let Some(p) = proc(s!("NtUpdateWnfStateData")) else { return false };
        let f: Update = unsafe { std::mem::transmute(p) };
        // SAFETY: 參數照 NtUpdateWnfStateData 的定義，寫入 4 bytes
        let r = unsafe { f(&STATE, &v as *const u32 as *const c_void, 4, std::ptr::null(), std::ptr::null(), 0, 0) };
        r >= 0
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn get() -> Option<u32> {
        None
    }
    pub fn set(_v: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn end_without_begin_does_nothing() {
        super::end();
        // 非 Windows：讀不到狀態，開啟失敗
        if !cfg!(windows) {
            assert!(!super::begin());
        }
    }
}
