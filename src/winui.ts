/**
 * 操作視窗的縮小 / 還原：開始擷取時把擋到擷取範圍的操作視窗縮到工作列（避免錄到它），停止後再還原，
 * 讓使用者馬上看到「錄影已儲存」。以視窗標題（「螢幕錄影 v…」）辨識。
 */
import { dlopen, FFIType, JSCallback, ptr, type Pointer } from "bun:ffi";
import { intersects } from "./args.ts";
import type { Rect } from "./shared/types.ts";

const { i32, i64, u32, ptr: P } = FFIType;
let user32: ReturnType<typeof load> | undefined;
function load() {
  return dlopen("user32.dll", {
    EnumWindows: { args: [P, i64], returns: i32 },
    GetWindowTextW: { args: [P, P, i32], returns: i32 },
    IsWindowVisible: { args: [P], returns: i32 },
    IsIconic: { args: [P], returns: i32 },
    ShowWindow: { args: [P, i32], returns: i32 },
    GetWindowRect: { args: [P, P], returns: i32 },
  });
}
let dwmapi: ReturnType<typeof loadDwm> | null | undefined;
function loadDwm() {
  return dlopen("dwmapi.dll", {
    DwmGetWindowAttribute: { args: [P, u32, P, u32], returns: i32 },
  });
}

const SW_MINIMIZE = 6;
/** 還原但不搶焦點：自動停止（最長時間、磁碟不足）時不要跳到使用者正在用的程式前面 */
const SW_SHOWNOACTIVATE = 4;
const TITLE_PREFIX = "螢幕錄影 v";

/** 目前看得到、而且沒有縮小的操作視窗 */
function findUiWindows(prefix: string): Pointer[] {
  user32 ??= load();
  const U = user32.symbols;
  const found: Pointer[] = [];
  const buf = new Uint16Array(256);
  const cb = new JSCallback(
    (hwnd: Pointer) => {
      if (U.IsWindowVisible(hwnd) && !U.IsIconic(hwnd)) {
        const n = U.GetWindowTextW(hwnd, ptr(buf), buf.length);
        if (n > 0 && String.fromCharCode(...buf.subarray(0, n)).startsWith(prefix)) found.push(hwnd);
      }
      return 1; // 繼續列舉
    },
    { args: [P, i64], returns: i32 },
  );
  try {
    U.EnumWindows(cb.ptr, 0);
  } finally {
    cb.close();
  }
  return found;
}

const DWMWA_EXTENDED_FRAME_BOUNDS = 9;

/**
 * 視窗看得到的範圍（實體像素）。優先用 DWM 的外框：GetWindowRect 含不可見的縮放邊框，
 * 最大化的視窗會多出約 8px 到隔壁螢幕上，被誤判成擋到擷取範圍。
 */
function windowRect(hwnd: Pointer): Rect | undefined {
  const r = new Int32Array(4); // left, top, right, bottom
  if (dwmapi === undefined) {
    try {
      dwmapi = loadDwm();
    } catch {
      dwmapi = null;
    }
  }
  const ok = dwmapi?.symbols.DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, ptr(r), r.byteLength) === 0
    || user32!.symbols.GetWindowRect(hwnd, ptr(r)) !== 0;
  if (!ok) return undefined;
  return { x: r[0]!, y: r[1]!, width: r[2]! - r[0]!, height: r[3]! - r[1]! };
}

/** 與 area 重疊的操作視窗（拿不到位置的視窗算重疊，寧可多縮也不要錄到它） */
function windowsIn(area: Rect | undefined, prefix: string): Pointer[] {
  return findUiWindows(prefix).filter((h) => {
    if (!area) return true;
    const r = windowRect(h);
    return !r || intersects(r, area);
  });
}

/** 是否有看得到的操作視窗在 area 內（倒數時決定要不要蓋上全畫面倒數）；無法判斷時回傳 true */
export function uiInArea(area: Rect, prefix = TITLE_PREFIX): boolean {
  if (process.platform !== "win32") return true;
  try {
    return windowsIn(area, prefix).length > 0;
  } catch {
    return true;
  }
}

let minimized: Pointer[] = [];

/**
 * 縮小操作視窗；回傳是否有縮小任何視窗。
 * 指定 area 時只縮小與它重疊的視窗（拿不到位置的視窗一律縮小，寧可多縮也不要錄到它）。
 */
export function minimizeUi(area?: Rect, prefix = TITLE_PREFIX): boolean {
  if (process.platform !== "win32") return false;
  try {
    const list = windowsIn(area, prefix);
    for (const h of list) user32!.symbols.ShowWindow(h, SW_MINIMIZE);
    minimized = list;
    return list.length > 0;
  } catch {
    return false;
  }
}

/** 還原先前由 minimizeUi 縮小的視窗（使用者自己又打開的就不動） */
export function restoreUi() {
  if (!minimized.length || !user32) return;
  try {
    for (const h of minimized) if (user32.symbols.IsIconic(h)) user32.symbols.ShowWindow(h, SW_SHOWNOACTIVATE);
  } catch {
    // 視窗已關閉
  }
  minimized = [];
}
