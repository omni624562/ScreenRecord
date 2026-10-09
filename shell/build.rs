//! exe 圖示（assets/icon.ico）由 scripts/make-icon.ts 產生（與系統匣待命圖示相同的設計）；
//! 沒有時先產生，tauri-build 會把它和版本資訊（package.json 的版本）寫進 exe。
use std::path::Path;

fn main() {
    let icon = Path::new("../assets/icon.ico");
    if !icon.exists() {
        let bun = if cfg!(windows) { "bun.exe" } else { "bun" };
        let ok = std::process::Command::new(bun).current_dir("..").args(["run", "scripts/make-icon.ts"]).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            panic!("找不到 assets/icon.ico，且無法執行 bun run scripts/make-icon.ts 產生（需要安裝 Bun）");
        }
    }
    println!("cargo:rerun-if-changed=../assets/icon.ico");
    println!("cargo:rerun-if-changed=../src/icon.ts");
    tauri_build::build()
}
