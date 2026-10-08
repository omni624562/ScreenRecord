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
        Error::Other(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// 回傳 Config 錯誤：`bail!("訊息 {}", x)`
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::Error::config(format!($($arg)*)))
    };
}
