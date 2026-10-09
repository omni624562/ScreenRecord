//! 把檔案移到資源回收筒：SHFileOperationW + FOF_ALLOWUNDO，可從資源回收筒還原。

use crate::error::{Error, Result};

#[cfg(windows)]
pub fn move_to_recycle_bin(paths: &[String]) -> Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    if paths.is_empty() {
        return Ok(());
    }
    // pFrom：每個路徑以 \0 分隔，最後再多一個 \0
    let mut from: Vec<u16> = Vec::new();
    for p in paths {
        from.extend(p.encode_utf16());
        from.push(0);
    }
    from.push(0);
    let mut op = SHFILEOPSTRUCTW { wFunc: FO_DELETE, pFrom: PCWSTR(from.as_ptr()), fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16, ..Default::default() };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 {
        return Err(Error::config(format!("無法移到資源回收筒（代碼 0x{:x}），檔案可能正在使用中", r as u32)));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err(Error::config("刪除動作被中止"));
    }
    Ok(())
}

/// 其他平台（開發與測試）：直接刪除
#[cfg(not(windows))]
pub fn move_to_recycle_bin(paths: &[String]) -> Result<()> {
    for p in paths {
        std::fs::remove_file(p).map_err(|e| Error::config(format!("無法刪除 {p}：{e}")))?;
    }
    Ok(())
}
