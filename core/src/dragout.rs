//! 把檔案從清單拖到其他程式（檔案總管、LINE、Teams、Word、瀏覽器上傳……）：
//! 用 Windows 殼層幫檔案做好「拖曳資料」（和在檔案總管裡拖一樣），再交給系統的拖曳。
//! 只給「複製」：拖到別的資料夾是複製一份，原檔留在清單裡。
//!
//! 要在介面執行緒呼叫；拖曳期間這個函式不會回來（系統自己跑訊息迴圈），放開滑鼠才回來。

/// 開始拖曳這些檔案；放到某個地方回傳 true，取消（按 Esc、放回原處）回傳 false
#[cfg(windows)]
pub fn drag_files(paths: &[String]) -> Result<bool, String> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::{IDropSource, DROPEFFECT_COPY, DROPEFFECT_NONE};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, BHID_DataObject};

    if paths.is_empty() {
        return Ok(false);
    }
    unsafe {
        let mut pidls: Vec<*const ITEMIDLIST> = vec![];
        for p in paths {
            let pidl = ILCreateFromPathW(PCWSTR(HSTRING::from(p.as_str()).as_ptr()));
            if !pidl.is_null() {
                pidls.push(pidl);
            }
        }
        let result = (|| {
            if pidls.is_empty() {
                return Err("找不到要拖曳的檔案".to_string());
            }
            let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&pidls).map_err(|e| e.to_string())?;
            let data: IDataObject = items.BindToHandler(None, &BHID_DataObject).map_err(|e| e.to_string())?;
            // 不給拖曳來源：殼層用預設的（Esc 取消、放開滑鼠放下）
            let effect = SHDoDragDrop(None, &data, None::<&IDropSource>, DROPEFFECT_COPY).map_err(|e| e.to_string())?;
            Ok(effect != DROPEFFECT_NONE)
        })();
        for p in pidls {
            ILFree(Some(p));
        }
        result
    }
}

#[cfg(not(windows))]
pub fn drag_files(_paths: &[String]) -> Result<bool, String> {
    Err("只有 Windows 可以把檔案拖到其他程式".to_string())
}
