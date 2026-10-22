//! 把檔案移到資源回收筒：SHFileOperationW + FOF_ALLOWUNDO，可從資源回收筒還原。
//! 網路資料夾（NAS）沒有資源回收筒，用 delete_for_sure：移不進去時直接刪掉，刪完確認檔案真的不見了。

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
        return Err(Error::config(crate::trf!("無法移到資源回收筒（代碼 0x{:x}），檔案可能正在使用中", "Couldn't move to the Recycle Bin (code 0x{:x}). The file may be in use", r as u32)));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err(Error::config(crate::tr!("刪除動作被中止", "Delete was canceled")));
    }
    Ok(())
}

/// 其他平台（開發與測試）：直接刪除
#[cfg(not(windows))]
pub fn move_to_recycle_bin(paths: &[String]) -> Result<()> {
    for p in paths {
        std::fs::remove_file(p).map_err(|e| Error::config(crate::trf!("無法刪除 {p}：{e}", "Couldn't delete {p}: {e}")))?;
    }
    Ok(())
}

/// 刪掉這些檔案（NAS 等網路資料夾上的也會刪）：先試著移到資源回收筒；網路資料夾沒有資源回收筒，
/// Windows 有時回答成功卻沒有刪，所以之後檢查還在的直接刪掉；還刪不掉時回報是哪些、為什麼
pub fn delete_for_sure(paths: &[String]) -> Result<()> {
    if let Err(e) = move_to_recycle_bin(paths) {
        crate::info!("[刪除] 移到資源回收筒失敗，改成直接刪除：{}", e.message());
    }
    let mut failed = vec![];
    for p in paths.iter().filter(|p| std::path::Path::new(p).exists()) {
        crate::info!("[刪除] 移到資源回收筒後還在，直接刪除：{p}");
        if let Err(e) = std::fs::remove_file(p) {
            failed.push(format!("{p}（{e}）"));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(Error::config(crate::trf!("刪不掉：{}", "Couldn't delete: {}", failed.join("、"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_for_sure_removes_and_reports() {
        let dir = std::env::temp_dir().join(format!("sr-recycle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.mp4");
        std::fs::write(&f, b"x").unwrap();
        delete_for_sure(&[f.display().to_string()]).unwrap();
        assert!(!f.exists());
        // 不存在的檔案：當作已經刪了
        delete_for_sure(&[dir.join("gone.mp4").display().to_string()]).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
