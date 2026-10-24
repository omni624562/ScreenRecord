//! 錯誤型別：Config = 使用者能理解並修正的錯誤（訊息直接顯示在介面上，HTTP 400）；Other = 內部錯誤（HTTP 500）。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Config(String),
    Other(String),
}

impl Error {
    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
    pub fn is_config(&self) -> bool {
        matches!(self, Error::Config(_))
    }
    pub fn message(&self) -> &str {
        match self {
            Error::Config(m) | Error::Other(m) => m,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Other(plain(&e))
    }
}

/// 檔案、資料夾的錯誤講白話（原本的系統訊息寫進記錄檔）
pub fn plain(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    let text = match (e.kind(), e.raw_os_error()) {
        // Windows：ERROR_SHARING_VIOLATION、ERROR_LOCK_VIOLATION
        (_, Some(32 | 33)) => crate::tr!("檔案正被其他程式使用中，關掉那個程式再試一次", "The file is in use by another program. Close it and try again"),
        // ERROR_HANDLE_DISK_FULL、ERROR_DISK_FULL
        (_, Some(39 | 112)) | (StorageFull, _) => crate::tr!("磁碟空間不夠，清出一些空間再試一次", "The disk is full. Free up some space and try again"),
        // ERROR_BAD_NETPATH、ERROR_BAD_NET_NAME、ERROR_NETNAME_DELETED
        (_, Some(53 | 67 | 64)) => crate::tr!("連不到網路上的位置（NAS 可能沒開、或不在同一個網路）", "Couldn't reach the network location (the NAS may be off or on another network)"),
        (PermissionDenied, _) => crate::tr!(
            "沒有權限（檔案可能是唯讀、被其他程式開著，或這個資料夾不能寫入）",
            "Permission denied (the file may be read-only or open in another program, or the folder can't be written to)"
        ),
        (NotFound, _) => crate::tr!("找不到檔案或資料夾（可能被移走、重新命名或刪掉了）", "The file or folder wasn't found (it may have been moved, renamed or deleted)"),
        (AlreadyExists, _) => crate::tr!("已經有同名的檔案", "A file with that name already exists"),
        (TimedOut, _) => crate::tr!("等太久沒有回應（網路磁碟可能斷線了）", "No response for too long (a network drive may have disconnected)"),
        (ReadOnlyFilesystem, _) => crate::tr!("這個磁碟是唯讀的，不能寫入", "This drive is read-only"),
        (InvalidFilename, _) => crate::tr!("檔名或路徑不能用（太長或有不能用的字元）", "The file name or path can't be used (too long or has invalid characters)"),
        _ => return e.to_string(),
    };
    crate::info!("[錯誤] {text}（{e}）");
    text.to_string()
}

pub type Result<T> = std::result::Result<T, Error>;

/// 回傳 Config 錯誤：`bail!("訊息 {}", x)`
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::Error::config(format!($($arg)*)))
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn io_errors_are_explained() {
        use std::io::{Error, ErrorKind};
        assert!(super::plain(&Error::from(ErrorKind::PermissionDenied)).contains("權限") || super::plain(&Error::from(ErrorKind::PermissionDenied)).contains("Permission"));
        assert!(super::plain(&Error::from_raw_os_error(112)).contains("磁碟") || super::plain(&Error::from_raw_os_error(112)).contains("disk"));
        let odd = Error::other("odd");
        assert_eq!(super::plain(&odd), "odd");
    }
}
