//! 開啟檔案的視窗（Windows 的「開啟」對話框）。會等到使用者選好或取消，請在背景執行緒呼叫。

use std::path::PathBuf;

/// filters：(說明, 副檔名樣式，例如 "*.png;*.jpg")
#[cfg(windows)]
pub fn pick(title: &str, filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};
    // 字串要活到對話框關閉
    let names: Vec<HSTRING> = filters.iter().map(|f| HSTRING::from(f.0)).collect();
    let specs: Vec<HSTRING> = filters.iter().map(|f| HSTRING::from(f.1)).collect();
    let list: Vec<COMDLG_FILTERSPEC> = names.iter().zip(&specs).map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) }).collect();
    let title = HSTRING::from(title);
    unsafe {
        let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let r = (|| -> windows::core::Result<Option<PathBuf>> {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            if !list.is_empty() {
                dlg.SetFileTypes(&list)?;
            }
            dlg.SetTitle(&title)?;
            dlg.SetOptions(dlg.GetOptions()? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
            // 取消時回傳錯誤（HRESULT_FROM_WIN32(ERROR_CANCELLED)）
            if dlg.Show(None).is_err() {
                return Ok(None);
            }
            let item = dlg.GetResult()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PCWSTR(name.0).to_string().unwrap_or_default();
            CoTaskMemFree(Some(name.0 as *const _));
            Ok(Some(PathBuf::from(path)))
        })();
        if inited {
            CoUninitialize();
        }
        r.map_err(|e| format!("無法開啟選擇檔案的視窗：{}", e.message()))
    }
}

#[cfg(not(windows))]
pub fn pick(_title: &str, _filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    Err("這個系統不支援選擇檔案的視窗".into())
}
