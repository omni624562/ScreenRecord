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
    let names = display_names();
    let mut monitors = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| crate::trf!("CreateDXGIFactory1 失敗 ({e})", "CreateDXGIFactory1 failed ({e})"))?;
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
                        let (key, name) = names.get(&device_name.to_lowercase()).cloned().unwrap_or_default();
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
                            key,
                            name,
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

/// 每個 \\.\DISPLAYn（小寫）接的實體螢幕：裝置路徑（穩定的識別）與型號名稱
#[cfg(windows)]
fn display_names() -> std::collections::HashMap<String, (String, String)> {
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
    };
    let mut map = std::collections::HashMap::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).is_err() {
            return map;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).is_err() {
            return map;
        }
        for p in paths.iter().take(np as usize) {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            map.entry(wide(&src.viewGdiDeviceName).to_lowercase()).or_insert((wide(&tgt.monitorDevicePath), wide(&tgt.monitorFriendlyDeviceName).trim().to_string()));
        }
    }
    map
}

/// 目前的螢幕配置（每個螢幕的名稱與位置、大小）：很快，可以常常比對有沒有變（接上、拔掉、改解析度、改排列）
#[cfg(windows)]
pub fn layout_signature() -> String {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
    unsafe extern "system" fn visit(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
        let list = &mut *(data.0 as *mut Vec<String>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(m, &mut info.monitorInfo as *mut _).as_bool() {
            let r = info.monitorInfo.rcMonitor;
            list.push(format!("{} {} {} {} {} {}", wide(&info.szDevice), r.left, r.top, r.right, r.bottom, info.monitorInfo.dwFlags));
        }
        true.into()
    }
    ensure_dpi_aware();
    let mut list: Vec<String> = vec![];
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(visit), LPARAM(&mut list as *mut Vec<String> as isize));
    }
    list.sort();
    list.join("|")
}

/// 啟動時檢查顯示環境並寫進記錄檔：DPI 模式有沒有設成 Per-Monitor V2、每個螢幕的解析度與縮放比例。
/// 游標光暈、波紋、錄影外框等小視窗錯位時，看記錄檔就知道是哪種環境
#[cfg(windows)]
pub fn log_environment() {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
    use windows::Win32::UI::HiDpi::{AreDpiAwarenessContextsEqual, GetDpiForMonitor, GetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
    unsafe extern "system" fn visit(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
        let list = &mut *(data.0 as *mut Vec<(String, RECT, u32, bool)>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(m, &mut info.monitorInfo as *mut _).as_bool() {
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            list.push((wide(&info.szDevice), info.monitorInfo.rcMonitor, dx, info.monitorInfo.dwFlags & 1 != 0));
        }
        true.into()
    }
    ensure_dpi_aware();
    let v2 = unsafe { AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).as_bool() };
    if v2 {
        crate::info!("顯示環境：DPI 模式 Per-Monitor V2");
    } else {
        crate::warn!("顯示環境：DPI 模式不是 Per-Monitor V2，縮放不是 100% 的螢幕上游標光暈、點擊波紋可能和游標錯開");
    }
    let mut list: Vec<(String, RECT, u32, bool)> = vec![];
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(visit), LPARAM(&mut list as *mut _ as isize));
    }
    let names = display_names();
    for (dev, r, dpi, primary) in list {
        let model = names.get(&dev.to_lowercase()).map(|(_, n)| n.as_str()).filter(|n| !n.is_empty()).unwrap_or("?");
        crate::info!("顯示環境：{dev}（{model}）{}×{} 位置 ({}, {}) 縮放 {}%{}", r.right - r.left, r.bottom - r.top, r.left, r.top, (dpi * 100 + 48) / 96, if primary { " 主螢幕" } else { "" });
    }
}

#[cfg(not(windows))]
pub fn layout_signature() -> String {
    String::new()
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
            key: String::new(),
            name: String::new(),
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
