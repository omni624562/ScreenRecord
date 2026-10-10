//! 以 DXGI 列舉所有顯示輸出。
//!
//! 用 DXGI 而不是 GDI：ddagrab 的 output_idx 是「某張顯示卡上的第幾個 output」，
//! 只有 IDXGIFactory1::EnumAdapters1 + IDXGIAdapter::EnumOutputs 能給出一致的 (adapter, output) 索引，
//! 同時拿到桌面座標供 gdigrab 退回使用。

use crate::types::MonitorInfo;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

static DISPLAY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)DISPLAY(\d+)").unwrap());

/// 從 \\.\DISPLAY2 取出螢幕編號
pub fn display_number(device_name: &str) -> Option<u32> {
    DISPLAY_RE.captures(device_name).and_then(|c| c[1].parse().ok())
}

/// 同一個實體螢幕在混合顯示卡環境下可能被多張卡回報，依 DeviceName 去重（保留第一個）；主螢幕在前，其餘依編號
pub fn finalize(monitors: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    let mut seen = HashSet::new();
    let mut out: Vec<MonitorInfo> = monitors.into_iter().filter(|m| seen.insert(m.device_name.clone())).collect();
    out.sort_by(|p, q| q.primary.cmp(&p.primary).then(p.display_number.cmp(&q.display_number)));
    out
}

#[cfg(windows)]
fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// 設為 Per-Monitor V2，確保拿到的是實體像素座標（與 ddagrab / gdigrab 一致）
#[cfg(windows)]
pub fn ensure_dpi_aware() {
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        // 已經設定過（例如視窗程式庫或 manifest）時會失敗，沒關係
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

#[cfg(windows)]
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, String> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    ensure_dpi_aware();
    let mut monitors = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1 失敗 ({e})"))?;
        let mut a = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let adapter_name = adapter.GetDesc1().map(|d| wide(&d.Description).trim().to_string()).unwrap_or_else(|_| format!("Adapter {a}"));
            let mut o = 0u32;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if let Ok(d) = output.GetDesc() {
                    if d.AttachedToDesktop.as_bool() {
                        let r = d.DesktopCoordinates;
                        let device_name = wide(&d.DeviceName);
                        let display_number = display_number(&device_name).unwrap_or(monitors.len() as u32 + 1);
                        monitors.push(MonitorInfo {
                            id: format!("{a}:{o}"),
                            adapter: a,
                            output: o,
                            adapter_name: adapter_name.clone(),
                            device_name,
                            display_number,
                            x: r.left,
                            y: r.top,
                            width: r.right - r.left,
                            height: r.bottom - r.top,
                            primary: r.left == 0 && r.top == 0,
                            rotation: d.Rotation.0 as u32,
                        });
                    }
                }
                o += 1;
            }
            a += 1;
        }
    }
    Ok(finalize(monitors))
}

#[cfg(not(windows))]
pub fn enumerate_monitors() -> Result<Vec<MonitorInfo>, String> {
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(device: &str, display: u32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id: format!("0:{display}"),
            adapter: 0,
            output: display,
            adapter_name: "GPU".into(),
            device_name: device.into(),
            display_number: display,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary,
            rotation: 1,
        }
    }

    #[test]
    fn dedupe_and_order() {
        let list = finalize(vec![m("\\\\.\\DISPLAY3", 3, false), m("\\\\.\\DISPLAY2", 2, true), m("\\\\.\\DISPLAY3", 3, false), m("\\\\.\\DISPLAY1", 1, false)]);
        let names: Vec<u32> = list.iter().map(|m| m.display_number).collect();
        assert_eq!(names, vec![2, 1, 3]);
        assert_eq!(display_number("\\\\.\\DISPLAY12"), Some(12));
        assert_eq!(display_number("weird"), None);
    }
}
