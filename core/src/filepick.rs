//! 開啟檔案的視窗（Windows 的「開啟」對話框）。會等到使用者選好或取消，請在背景執行緒呼叫。

use std::path::PathBuf;

/// filters：(說明, 副檔名樣式，例如 "*.png;*.jpg")
/// 在新的執行緒開啟：對話框要在 STA 執行緒，而共用的背景執行緒可能已被其他功能（文字辨識）設成 MTA
#[cfg(windows)]
pub fn pick(title: &str, filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    let title = title.to_string();
    let filters: Vec<(String, String)> = filters.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    std::thread::Builder::new()
        .name("file-dialog".into())
        .spawn(move || {
            let f: Vec<(&str, &str)> = filters.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            pick_sta(&title, &f)
        })
        .map_err(|e| e.to_string())?
        .join()
        .unwrap_or_else(|_| Err(crate::tr!("無法開啟選擇檔案的視窗", "Couldn't open the file picker").into()))
}

#[cfg(windows)]
fn pick_sta(title: &str, filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
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
        r.map_err(|e| crate::trf!("無法開啟選擇檔案的視窗：{}", "Couldn't open the file picker: {}", e.message()))
    }
}

#[cfg(not(windows))]
pub fn pick(_title: &str, _filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    Err(crate::tr!("這個系統不支援選擇檔案的視窗", "The file picker isn't supported on this system").into())
}
