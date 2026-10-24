//! 自動更新時檢查新版 exe 的程式碼簽章（Authenticode）。發佈時簽章是選用的：
//! 目前這一版有簽章，新版就要有完好的簽章、而且是同一張憑證（或兩邊都是系統信任的同一個發行者）；
//! 目前這一版沒有簽章時不要求（不然沒設定簽章的發佈永遠更新不了），但簽章壞掉（檔案被改過）的一律不用。

use crate::tr;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signer {
    Unsigned,
    Signed {
        /// 簽章憑證的 SHA-256
        thumb: [u8; 32],
        /// 憑證上的名稱（發行者）
        subject: String,
        /// 憑證鏈連到系統信任的根憑證
        trusted: bool,
    },
    /// 有簽章但檔案內容和簽章對不起來（被改過）、或憑證被明確停用
    Broken,
}

/// 新版可以拿來取代目前這一版嗎
pub fn update_allowed(current: &Signer, new: &Signer) -> Result<(), String> {
    if *new == Signer::Broken {
        return Err(tr!("新版檔案的數位簽章不正確（檔案可能被改過），已刪除", "The new version's digital signature is invalid (the file may have been modified) and was deleted").into());
    }
    let Signer::Signed { thumb, subject, trusted } = current else { return Ok(()) };
    match new {
        Signer::Signed { thumb: t, subject: s, trusted: tr } if t == thumb || (*trusted && *tr && s == subject) => Ok(()),
        _ => {
            Err(tr!("新版檔案不是同一個發行者簽署的，已刪除。請到下載頁面手動更新", "The new version isn't signed by the same publisher and was deleted. Update manually from the download page")
                .into())
        }
    }
}

pub fn signer(path: &Path) -> Signer {
    #[cfg(windows)]
    {
        win::signer(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Signer::Unsigned
    }
}

#[allow(dead_code)]
fn thumb(der: &[u8]) -> [u8; 32] {
    Sha256::digest(der).into()
}

#[cfg(windows)]
mod win {
    use super::Signer;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE};
    use windows::Win32::Security::WinTrust::*;

    const TRUST_E_NOSIGNATURE: u32 = 0x800B_0100;
    const TRUST_E_SUBJECT_FORM_UNKNOWN: u32 = 0x800B_0003;
    const TRUST_E_PROVIDER_UNKNOWN: u32 = 0x800B_0001;
    /// 簽章本身是好的，只是憑證不被這台電腦信任（自己做的憑證）、過期或連不到上層
    const UNTRUSTED: [u32; 4] = [0x800B_0109, 0x800B_010A, 0x800B_010D, 0x800B_0101];

    pub fn signer(path: &std::path::Path) -> Signer {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut file = WINTRUST_FILE_INFO { cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32, pcwszFilePath: windows::core::PCWSTR(wide.as_ptr()), ..Default::default() };
        let mut data = WINTRUST_DATA {
            cbStruct: size_of::<WINTRUST_DATA>() as u32,
            dwUIChoice: WTD_UI_NONE,
            fdwRevocationChecks: WTD_REVOKE_NONE,
            dwUnionChoice: WTD_CHOICE_FILE,
            dwStateAction: WTD_STATEACTION_VERIFY,
            // 不上網查憑證撤銷（離線也能更新；檔案內容和簽章對不對照樣檢查）
            dwProvFlags: WTD_REVOCATION_CHECK_NONE | WTD_CACHE_ONLY_URL_RETRIEVAL,
            ..Default::default()
        };
        data.Anonymous.pFile = &mut file;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        // SAFETY: data、file、wide 在兩次呼叫期間都有效；狀態用 WTD_STATEACTION_CLOSE 釋放
        unsafe {
            let r = WinVerifyTrust(HWND(-1isize as _), &mut action, &mut data as *mut _ as _) as u32;
            let out = if r == TRUST_E_NOSIGNATURE || r == TRUST_E_SUBJECT_FORM_UNKNOWN || r == TRUST_E_PROVIDER_UNKNOWN {
                Signer::Unsigned
            } else if r == 0 || UNTRUSTED.contains(&r) {
                cert_of(data.hWVTStateData, r == 0).unwrap_or(Signer::Broken)
            } else {
                crate::warn!("[更新] 數位簽章檢查失敗：0x{r:08X}");
                Signer::Broken
            };
            data.dwStateAction = WTD_STATEACTION_CLOSE;
            WinVerifyTrust(HWND(-1isize as _), &mut action, &mut data as *mut _ as _);
            out
        }
    }

    unsafe fn cert_of(state: HANDLE, trusted: bool) -> Option<Signer> {
        // SAFETY: state 是 WinVerifyTrust 剛給的；指標都由系統管理，關閉狀態前有效
        unsafe {
            let prov = WTHelperProvDataFromStateData(state);
            if prov.is_null() {
                return None;
            }
            let sgnr = WTHelperGetProvSignerFromChain(prov, 0, false, 0);
            if sgnr.is_null() || (*sgnr).csCertChain == 0 || (*sgnr).pasCertChain.is_null() {
                return None;
            }
            let cert = (*(*sgnr).pasCertChain).pCert;
            if cert.is_null() {
                return None;
            }
            let der = std::slice::from_raw_parts((*cert).pbCertEncoded, (*cert).cbCertEncoded as usize);
            let mut name = [0u16; 512];
            let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut name)) as usize;
            let subject = String::from_utf16_lossy(&name[..n.saturating_sub(1).min(name.len())]);
            Some(Signer::Signed { thumb: super::thumb(der), subject, trusted })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(t: u8, subject: &str, trusted: bool) -> Signer {
        Signer::Signed { thumb: [t; 32], subject: subject.into(), trusted }
    }

    #[test]
    fn same_publisher_is_required_once_signed() {
        // 沒簽章的版本：新版有沒有簽都可以，但簽章壞的不行
        assert!(update_allowed(&Signer::Unsigned, &Signer::Unsigned).is_ok());
        assert!(update_allowed(&Signer::Unsigned, &signed(1, "A", false)).is_ok());
        assert!(update_allowed(&Signer::Unsigned, &Signer::Broken).is_err());
        // 有簽章：同一張憑證才可以
        assert!(update_allowed(&signed(1, "A", false), &signed(1, "A", false)).is_ok());
        assert!(update_allowed(&signed(1, "A", false), &Signer::Unsigned).is_err());
        // 自己做的憑證：名稱一樣也不算（誰都能做同名的憑證）
        assert!(update_allowed(&signed(1, "A", false), &signed(2, "A", false)).is_err());
        // 系統信任的發行者換了新憑證：名稱一樣就可以
        assert!(update_allowed(&signed(1, "A", true), &signed(2, "A", true)).is_ok());
        assert!(update_allowed(&signed(1, "A", true), &signed(2, "B", true)).is_err());
    }

    #[test]
    fn unsigned_file_reads_as_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.exe");
        std::fs::write(&f, b"MZ not really a program").unwrap();
        assert!(matches!(signer(&f), Signer::Unsigned | Signer::Broken));
    }
}
