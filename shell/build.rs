//! Windows：把程式圖示（與系統匣待命圖示相同，由 core/src/icon.rs 繪製）與版本資訊寫進 exe。

#[allow(dead_code)]
#[path = "../core/src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=../core/src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    #[cfg(windows)]
    {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let ico = out.join("icon.ico");
        std::fs::write(&ico, icon::ico_file(icon::IconState::Idle, &[16, 24, 32, 48, 64, 256])).unwrap();
        let version = std::env::var("CARGO_PKG_VERSION").unwrap();
        let mut res = winresource::WindowsResource::new();
        res.set_icon(&ico.display().to_string())
            .set("ProductName", "螢幕錄影 ScreenRecorder")
            .set("FileDescription", "螢幕錄影 ScreenRecorder")
            .set("OriginalFilename", "ScreenRecorder.exe")
            .set("ProductVersion", &version)
            .set("FileVersion", &version)
            .set("LegalCopyright", "Copyright (c) 2026 omni624562 (MIT)");
        res.compile().expect("無法寫入 exe 圖示與版本資訊");
    }
}
