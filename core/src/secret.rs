//! 設定檔裡的密碼、金鑰加密保存：Windows 用 DPAPI（綁這個 Windows 帳號，別的帳號、別台電腦拿到檔案也解不開）。
//! 其他平台（開發、測試用）沒有對應的系統功能：照原樣保存。

/// 加密成可以放進 JSON 的文字；沒辦法加密時 None（照原樣保存）
pub fn protect(plain: &str) -> Option<String> {
    #[cfg(windows)]
    {
        win::crypt(plain.as_bytes(), true).map(|b| crate::steps::base64(&b))
    }
    #[cfg(not(windows))]
    {
        let _ = plain;
        None
    }
}

/// 解開 protect 的結果；解不開（換了帳號、檔案壞了）時 None
pub fn unprotect(text: &str) -> Option<String> {
    let bytes = unbase64(text)?;
    #[cfg(windows)]
    {
        win::crypt(&bytes, false).and_then(|b| String::from_utf8(b).ok())
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        None
    }
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let t = text.trim().trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(t.len() * 3 / 4);
    for chunk in t.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        match chunk.len() {
            4 => out.extend_from_slice(&bytes),
            3 => out.extend_from_slice(&bytes[..2]),
            2 => out.push(bytes[0]),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    pub fn crypt(data: &[u8], encrypt: bool) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input 指向 data（呼叫期間有效）；out 由系統配置，複製後用 LocalFree 釋放
        unsafe {
            let r = if encrypt {
                CryptProtectData(&input, windows::core::w!("ScreenRecorder"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            };
            if r.is_err() || out.pbData.is_null() {
                return None;
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(Some(HLOCAL(out.pbData as _)));
            Some(v)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trip() {
        for s in ["", "a", "ab", "abc", "abcd", "金鑰 key 1234567890"] {
            assert_eq!(super::unbase64(&crate::steps::base64(s.as_bytes())).as_deref(), Some(s.as_bytes()));
        }
        assert_eq!(super::unbase64("@@@"), None);
    }
}
