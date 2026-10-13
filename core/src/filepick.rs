//! 開啟檔案、選擇資料夾、另存新檔的視窗（Windows 的「開啟」「另存新檔」對話框）。會等到使用者選好或取消，請在背景執行緒呼叫。
//! 其他系統（開發測試用）：環境變數 SCREENRECORDER_FAKE_PICK 有值時當作選了那個路徑（"cancel" = 取消），否則回傳不支援。

use std::path::PathBuf;

/// filters：(說明, 副檔名樣式，例如 "*.png;*.jpg")
#[cfg(windows)]
pub fn pick(title: &str, filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    let filters: Vec<(String, String)> = filters.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    run(title, filters, false, None).map_err(|e| crate::trf!("無法開啟選擇檔案的視窗：{}", "Couldn't open the file picker: {}", e))
}

/// 選擇資料夾；start：一開始顯示的資料夾（不存在時由 Windows 決定）
#[cfg(windows)]
pub fn pick_folder(title: &str, start: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    run(title, vec![], true, start).map_err(|e| crate::trf!("無法開啟選擇資料夾的視窗：{}", "Couldn't open the folder picker: {}", e))
}

/// 另存新檔：dir 一開始的資料夾、name 預設檔名（含副檔名）、filters 檔案類型（第一個是預設）。
/// 已有同名檔案時 Windows 會先問要不要取代。回傳選好的路徑與選的類型（filters 的第幾個）；取消時 None
#[cfg(windows)]
pub fn save_file(title: &str, dir: Option<PathBuf>, name: &str, filters: &[(&str, &str)]) -> Result<Option<(PathBuf, usize)>, String> {
    let (title, name) = (title.to_string(), name.to_string());
    let filters: Vec<(String, String)> = filters.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    std::thread::Builder::new()
        .name("file-dialog".into())
        .spawn(move || {
            let f: Vec<(&str, &str)> = filters.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            save_sta(&title, dir.as_deref(), &name, &f).map_err(|e| e.message())
        })
        .map_err(|e| e.to_string())?
        .join()
        .unwrap_or_else(|_| Err(crate::tr!("視窗意外關閉", "The window closed unexpectedly").into()))
        .map_err(|e| crate::trf!("無法開啟另存新檔的視窗：{}", "Couldn't open the Save As window: {}", e))
}

#[cfg(windows)]
fn save_sta(title: &str, dir: Option<&std::path::Path>, name: &str, filters: &[(&str, &str)]) -> windows::core::Result<Option<(PathBuf, usize)>> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, SIGDN_FILESYSPATH};
    let names: Vec<HSTRING> = filters.iter().map(|f| HSTRING::from(f.0)).collect();
    let specs: Vec<HSTRING> = filters.iter().map(|f| HSTRING::from(f.1)).collect();
    let list: Vec<COMDLG_FILTERSPEC> = names.iter().zip(&specs).map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) }).collect();
    // 預設副檔名：第一個類型的第一個樣式（"*.png;*.jpg" → png）
    let ext = filters.first().and_then(|f| f.1.split(';').next()).map(|s| s.trim_start_matches("*.").to_string()).filter(|e| !e.contains('*')).unwrap_or_default();
    let (title, name, ext) = (HSTRING::from(title), HSTRING::from(name), HSTRING::from(ext));
    unsafe {
        let inited = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let r = (|| -> windows::core::Result<Option<(PathBuf, usize)>> {
            let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            if !list.is_empty() {
                dlg.SetFileTypes(&list)?;
                dlg.SetFileTypeIndex(1)?;
            }
            if !ext.is_empty() {
                dlg.SetDefaultExtension(&ext)?;
            }
            dlg.SetTitle(&title)?;
            dlg.SetFileName(&name)?;
            dlg.SetOptions(dlg.GetOptions()? | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM)?;
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None::<&IBindCtx>) {
                    let _ = dlg.SetFolder(&item);
                }
            }
            if dlg.Show(None).is_err() {
                return Ok(None);
            }
            let kind = dlg.GetFileTypeIndex().map(|i| i.saturating_sub(1) as usize).unwrap_or(0);
            let item = dlg.GetResult()?;
            let shown = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PCWSTR(shown.0).to_string().unwrap_or_default();
            CoTaskMemFree(Some(shown.0 as *const _));
            Ok(Some((PathBuf::from(path), kind)))
        })();
        if inited {
            CoUninitialize();
        }
        r
    }
}

/// 在新的執行緒開啟：對話框要在 STA 執行緒，而共用的背景執行緒可能已被其他功能（文字辨識）設成 MTA
#[cfg(windows)]
fn run(title: &str, filters: Vec<(String, String)>, folder: bool, start: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    let title = title.to_string();
    std::thread::Builder::new()
        .name("file-dialog".into())
        .spawn(move || {
            let f: Vec<(&str, &str)> = filters.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            show_sta(&title, &f, folder, start.as_deref()).map_err(|e| e.message())
        })
        .map_err(|e| e.to_string())?
        .join()
        .unwrap_or_else(|_| Err(crate::tr!("視窗意外關閉", "The window closed unexpectedly").into()))
}

#[cfg(windows)]
fn show_sta(title: &str, filters: &[(&str, &str)], folder: bool, start: Option<&std::path::Path>) -> windows::core::Result<Option<PathBuf>> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
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
            let kind = if folder { FOS_PICKFOLDERS } else { FOS_FILEMUSTEXIST };
            dlg.SetOptions(dlg.GetOptions()? | kind | FOS_FORCEFILESYSTEM)?;
            // 一開始顯示的資料夾（不存在就算了）
            if let Some(dir) = start.filter(|d| d.is_dir()) {
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None::<&IBindCtx>) {
                    let _ = dlg.SetFolder(&item);
                }
            }
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
        r
    }
}

/// 開發測試用：SCREENRECORDER_FAKE_PICK 當作選好的路徑（"cancel" = 取消）
#[cfg(not(windows))]
fn fake() -> Option<Option<PathBuf>> {
    let v = std::env::var("SCREENRECORDER_FAKE_PICK").ok().filter(|v| !v.is_empty())?;
    Some((v != "cancel").then(|| PathBuf::from(v)))
}

#[cfg(not(windows))]
pub fn pick(_title: &str, _filters: &[(&str, &str)]) -> Result<Option<PathBuf>, String> {
    fake().ok_or_else(|| crate::tr!("這個系統不支援選擇檔案的視窗", "The file picker isn't supported on this system").into())
}

#[cfg(not(windows))]
pub fn pick_folder(_title: &str, _start: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    fake().ok_or_else(|| crate::tr!("這個系統不支援選擇資料夾的視窗", "The folder picker isn't supported on this system").into())
}

#[cfg(not(windows))]
pub fn save_file(_title: &str, _dir: Option<PathBuf>, _name: &str, filters: &[(&str, &str)]) -> Result<Option<(PathBuf, usize)>, String> {
    let picked = fake().ok_or_else(|| crate::tr!("這個系統不支援另存新檔的視窗", "The Save As window isn't supported on this system").to_string())?;
    // 依副檔名判斷選的類型
    Ok(picked.map(|p| {
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let kind = filters.iter().position(|f| f.1.to_lowercase().split(';').any(|s| s.trim() == format!("*.{ext}"))).unwrap_or(0);
        (p, kind)
    }))
}
