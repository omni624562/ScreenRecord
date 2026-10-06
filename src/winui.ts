/**
 * 操作視窗的縮小 / 還原：開始擷取時把操作視窗縮到工作列（避免錄到它），停止後再還原，
 * 讓使用者馬上看到「錄影已儲存」。以視窗標題（「螢幕錄影 v…」）辨識。
 */
import { dlopen, FFIType, JSCallback, ptr, type Pointer } from "bun:ffi";

const { i32, i64, ptr: P } = FFIType;
let user32: ReturnType<typeof load> | undefined;
function load() {
  return dlopen("user32.dll", {
    EnumWindows: { args: [P, i64], returns: i32 },
    GetWindowTextW: { args: [P, P, i32], returns: i32 },
    IsWindowVisible: { args: [P], returns: i32 },
    IsIconic: { args: [P], returns: i32 },
    ShowWindow: { args: [P, i32], returns: i32 },
  });
}

const SW_MINIMIZE = 6;
const SW_RESTORE = 9;
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

let minimized: Pointer[] = [];

/** 縮小所有操作視窗；回傳是否有縮小任何視窗 */
export function minimizeUi(prefix = TITLE_PREFIX): boolean {
  if (process.platform !== "win32") return false;
  try {
    const list = findUiWindows(prefix);
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
    for (const h of minimized) if (user32.symbols.IsIconic(h)) user32.symbols.ShowWindow(h, SW_RESTORE);
  } catch {
    // 視窗已關閉
  }
  minimized = [];
}
