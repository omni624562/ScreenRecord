//! 建置時：
//! - 版本號以 package.json 為唯一來源（與發佈流程一致）
//! - 用 Bun 把網頁介面（src/ui）打包成靜態檔，內嵌進執行檔
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let core = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = core.parent().unwrap().to_path_buf();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let pkg = std::fs::read_to_string(root.join("package.json")).expect("讀不到 package.json");
    let version = pkg
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"version\":").map(|v| v.trim().trim_end_matches(',').trim_matches('"').to_string()))
        .expect("package.json 沒有 version");
    println!("cargo:rustc-env=APP_VERSION={version}");
    println!("cargo:rerun-if-changed={}", root.join("package.json").display());
    println!("cargo:rerun-if-changed={}", root.join("CHANGELOG.md").display());
    println!("cargo:rerun-if-changed={}", root.join("src/ui").display());
    println!("cargo:rerun-if-changed={}", root.join("src/shared").display());
    println!("cargo:rerun-if-env-changed=SCREENRECORDER_UI_DIST");

    let ui = out.join("ui");
    let _ = std::fs::remove_dir_all(&ui);
    std::fs::create_dir_all(&ui).unwrap();
    // 可指定已打包好的資料夾（例如 CI 先打包一次）；否則呼叫 bun 打包
    let dist = match std::env::var("SCREENRECORDER_UI_DIST") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => {
            let bun = if cfg!(windows) { "bun.exe" } else { "bun" };
            let ok = Command::new(bun)
                .current_dir(&root)
                .args(["build", "./src/ui/index.html", "--minify", "--outdir"])
                .arg(&ui)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                println!("cargo:warning=無法用 bun 打包介面（需要安裝 Bun），執行檔內只會有提示頁");
                std::fs::write(ui.join("index.html"), "<!doctype html><meta charset=utf-8><title>螢幕錄影</title><p>建置時沒有打包介面：請安裝 Bun 後重新建置。</p>").unwrap();
            }
            ui.clone()
        }
    };

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dist).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    let mut code = String::from("/// 內嵌的介面檔案：（網址路徑, Content-Type, 內容）\npub static UI_FILES: &[(&str, &str, &[u8])] = &[\n");
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy();
        code.push_str(&format!("    ({:?}, {:?}, include_bytes!({:?})),\n", name, mime(f), f.display().to_string()));
    }
    code.push_str("];\n");
    std::fs::write(out.join("ui_files.rs"), code).unwrap();
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        "map" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}
