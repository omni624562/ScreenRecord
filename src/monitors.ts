/**
 * 以 DXGI 列舉所有顯示輸出（bun:ffi 直接呼叫 COM vtable）。
 *
 * 之所以用 DXGI 而不是 GDI：ddagrab 的 output_idx 是「某張顯示卡上的第幾個 output」，
 * 只有 IDXGIFactory1::EnumAdapters1 + IDXGIAdapter::EnumOutputs 能給出一致的
 * (adapter, output) 索引，同時拿到桌面座標供 gdigrab 退回使用。
 */
import { dlopen, FFIType, ptr } from "bun:ffi";
import { guid, release, S_OK, vcall, wstr } from "./com.ts";
import type { MonitorInfo } from "./shared/types.ts";

const DXGI_ERROR_NOT_FOUND = 0x887a0002 | 0; // 轉成 i32

// COM vtable 索引（IUnknown 0-2、IDXGIObject 3-6）
const VT_FACTORY1_ENUM_ADAPTERS1 = 12;
const VT_ADAPTER_ENUM_OUTPUTS = 7;
const VT_ADAPTER_GET_DESC = 8;
const VT_OUTPUT_GET_DESC = 7;

const IID_IDXGIFactory1 = guid("770aae78-f26f-4dba-a829-253c83d1b387");

let libs: ReturnType<typeof openLibs> | undefined;
function openLibs() {
  const dxgi = dlopen("dxgi.dll", {
    CreateDXGIFactory1: { args: [FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
  });
  const user32 = dlopen("user32.dll", {
    SetProcessDpiAwarenessContext: { args: [FFIType.i64], returns: FFIType.i32 },
  });
  return { dxgi, user32 };
}

let dpiAwareSet = false;
/** 設為 Per-Monitor V2，確保拿到的是實體像素座標（與 ddagrab / gdigrab 一致）。 */
function ensureDpiAware() {
  if (dpiAwareSet) return;
  dpiAwareSet = true;
  try {
    libs!.user32.symbols.SetProcessDpiAwarenessContext(-4); // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
  } catch {
    // 舊系統沒有此 API；DXGI 座標本身不受 DPI 虛擬化影響，忽略即可
  }
}
export function enumerateMonitors(): MonitorInfo[] {
  if (process.platform !== "win32") return [];
  libs ??= openLibs();
  ensureDpiAware();

  const out = new BigUint64Array(1);
  const hr = libs.dxgi.symbols.CreateDXGIFactory1(ptr(IID_IDXGIFactory1), ptr(out));
  if (hr !== S_OK) throw new Error(`CreateDXGIFactory1 失敗 (0x${(hr >>> 0).toString(16)})`);
  const factory = Number(out[0]);

  const monitors: MonitorInfo[] = [];
  try {
    for (let a = 0; ; a++) {
      out[0] = 0n;
      const hrA = vcall(factory, VT_FACTORY1_ENUM_ADAPTERS1, [FFIType.u32, FFIType.ptr], a, ptr(out));
      if (hrA === DXGI_ERROR_NOT_FOUND) break;
      if (hrA !== S_OK) break;
      const adapter = Number(out[0]);
      try {
        // DXGI_ADAPTER_DESC：WCHAR Description[128] 開頭，總長 304 bytes
        const adesc = new Uint8Array(304);
        const adapterName = vcall(adapter, VT_ADAPTER_GET_DESC, [FFIType.ptr], ptr(adesc)) === S_OK
          ? wstr(adesc, 0, 128).trim()
          : `Adapter ${a}`;

        for (let o = 0; ; o++) {
          out[0] = 0n;
          const hrO = vcall(adapter, VT_ADAPTER_ENUM_OUTPUTS, [FFIType.u32, FFIType.ptr], o, ptr(out));
          if (hrO !== S_OK) break; // DXGI_ERROR_NOT_FOUND = 沒有更多 output
          const output = Number(out[0]);
          try {
            // DXGI_OUTPUT_DESC：DeviceName[32] WCHAR、RECT@64、AttachedToDesktop@80、Rotation@84、HMONITOR@88
            const d = new Uint8Array(96);
            if (vcall(output, VT_OUTPUT_GET_DESC, [FFIType.ptr], ptr(d)) !== S_OK) continue;
            const v = new DataView(d.buffer);
            const left = v.getInt32(64, true);
            const top = v.getInt32(68, true);
            const right = v.getInt32(72, true);
            const bottom = v.getInt32(76, true);
            const attached = v.getInt32(80, true) !== 0;
            if (!attached) continue;
            const deviceName = wstr(d, 0, 32);
            const num = /DISPLAY(\d+)/i.exec(deviceName)?.[1];
            monitors.push({
              id: `${a}:${o}`,
              adapter: a,
              output: o,
              adapterName,
              deviceName,
              displayNumber: num ? Number(num) : monitors.length + 1,
              x: left,
              y: top,
              width: right - left,
              height: bottom - top,
              primary: left === 0 && top === 0,
              rotation: v.getInt32(84, true),
            });
          } finally {
            release(output);
          }
        }
      } finally {
        release(adapter);
      }
    }
  } finally {
    release(factory);
  }

  // 同一個實體螢幕在混合顯卡環境下可能被多張卡回報，依 DeviceName 去重（保留第一個）
  const seen = new Set<string>();
  return monitors
    .filter((m) => (seen.has(m.deviceName) ? false : (seen.add(m.deviceName), true)))
    .sort((p, q) => Number(q.primary) - Number(p.primary) || p.displayNumber - q.displayNumber);
}
