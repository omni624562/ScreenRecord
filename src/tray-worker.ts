/**
 * 系統匣圖示（在獨立的 Worker 執行緒）。
 *
 * TrackPopupMenu 在選單開著的期間會卡住所在的執行緒；放在 Worker 裡，
 * 主執行緒的網頁伺服器、錄音資料讀取才不會跟著停住。
 * 這裡只負責畫圖示與選單，使用者選了什麼就用 postMessage 交給主執行緒處理。
 */
import { CFunction, dlopen, FFIType, JSCallback, ptr, type Pointer } from "bun:ffi";
import { iconResource, type IconState } from "./icon.ts";
import type { TrayCommand, TrayState, TrayToMain, MainToTray } from "./tray-protocol.ts";

declare const self: Worker;

const { u16, u32, i32, i64, ptr: P } = FFIType;
const user32 = dlopen("user32.dll", {
  RegisterClassExW: { args: [P], returns: u16 },
  CreateWindowExW: { args: [u32, P, P, u32, i32, i32, i32, i32, P, P, P, P], returns: P },
  DefWindowProcW: { args: [P, u32, i64, i64], returns: i64 },
  PeekMessageW: { args: [P, P, u32, u32, u32], returns: i32 },
  TranslateMessage: { args: [P], returns: i32 },
  DispatchMessageW: { args: [P], returns: i64 },
  CreatePopupMenu: { args: [], returns: P },
  AppendMenuW: { args: [P, u32, P, P], returns: i32 },
  SetMenuDefaultItem: { args: [P, u32, u32], returns: i32 },
  TrackPopupMenu: { args: [P, u32, i32, i32, i32, P, P], returns: i32 },
  DestroyMenu: { args: [P], returns: i32 },
  SetForegroundWindow: { args: [P], returns: i32 },
  GetCursorPos: { args: [P], returns: i32 },
  PostMessageW: { args: [P, u32, i64, i64], returns: i32 },
  DestroyWindow: { args: [P], returns: i32 },
  CreateIconFromResourceEx: { args: [P, u32, i32, u32, i32, i32, u32], returns: P },
  DestroyIcon: { args: [P], returns: i32 },
  RegisterWindowMessageW: { args: [P], returns: u32 },
  GetSystemMetrics: { args: [i32], returns: i32 },
  GetForegroundWindow: { args: [], returns: P },
  GetWindowThreadProcessId: { args: [P, P], returns: u32 },
  AttachThreadInput: { args: [u32, u32, i32], returns: i32 },
  MsgWaitForMultipleObjects: { args: [u32, P, i32, u32, u32], returns: u32 },
  RegisterHotKey: { args: [P, i32, u32, u32], returns: i32 },
  UnregisterHotKey: { args: [P, i32], returns: i32 },
});
const shell32 = dlopen("shell32.dll", { Shell_NotifyIconW: { args: [u32, P], returns: i32 } });
const kernel32 = dlopen("kernel32.dll", {
  GetModuleHandleW: { args: [P], returns: P },
  GetLastError: { args: [], returns: u32 },
  GetCurrentThreadId: { args: [], returns: u32 },
  LoadLibraryW: { args: [P], returns: P },
  GetProcAddress: { args: [P, P], returns: P },
});
const U = user32.symbols;

const wide = (s: string) => {
  const a = new Uint16Array(s.length + 1);
  for (let i = 0; i < s.length; i++) a[i] = s.charCodeAt(i);
  return a;
};

const WM_NULL = 0;
const WM_CLOSE = 0x10;
const WM_LBUTTONUP = 0x202;
const WM_RBUTTONUP = 0x205;
const WM_TRAY = 0x8001; // WM_APP + 1
const WM_HOTKEY = 0x0312;
const MOD_ALT = 1, MOD_CONTROL = 2, MOD_NOREPEAT = 0x4000;
/** 全域快捷鍵：id → 指令（按鍵與 shared/types.ts 的 HOTKEY_LABELS 一致） */
const HOTKEYS = [
  { id: 1, vk: 0x52 /* R */, cmd: "hotkey-record" },
  { id: 2, vk: 0x50 /* P */, cmd: "hotkey-pause" },
] as const;
const NIN_BALLOONUSERCLICK = 0x405;
const NIM_ADD = 0, NIM_MODIFY = 1, NIM_DELETE = 2;
const NIF_MESSAGE = 1, NIF_ICON = 2, NIF_TIP = 4, NIF_INFO = 0x10;
const NIIF_INFO = 1, NIIF_WARNING = 2;
const MF_STRING = 0, MF_GRAYED = 1, MF_CHECKED = 8, MF_POPUP = 0x10, MF_SEPARATOR = 0x800;
const TPM_RIGHTBUTTON = 2, TPM_NONOTIFY = 0x80, TPM_RETURNCMD = 0x100;
const SM_CXSMICON = 49;

/** 讓選單跟著 Windows 的深色 / 淺色模式（uxtheme 未公開 API；失敗就維持預設外觀） */
function allowDarkMenus() {
  try {
    const ux = kernel32.symbols.LoadLibraryW(ptr(wide("uxtheme.dll")));
    if (!ux) return;
    const setMode = kernel32.symbols.GetProcAddress(ux, 135 as unknown as Pointer); // SetPreferredAppMode
    const flush = kernel32.symbols.GetProcAddress(ux, 136 as unknown as Pointer); // FlushMenuThemes
    if (setMode) CFunction({ ptr: setMode, args: [i32], returns: i32 })(1); // AllowDark
    if (flush) CFunction({ ptr: flush, args: [], returns: FFIType.void })();
  } catch {
    // 舊版 Windows 沒有這兩個函式
  }
}

// ───────────── 圖示 ─────────────

const iconSize = Math.max(16, U.GetSystemMetrics(SM_CXSMICON) || 16);
const icons = new Map<IconState, Pointer>();
function icon(state: IconState): Pointer {
  let h = icons.get(state);
  if (!h) {
    const res = iconResource(iconSize, state);
    h = U.CreateIconFromResourceEx(ptr(res), res.length, 1, 0x00030000, iconSize, iconSize, 0) as Pointer;
    icons.set(state, h);
  }
  return h;
}

// ───────────── 視窗與系統匣 ─────────────

let hwnd: Pointer | null = null;
let state: TrayState | undefined;
const className = wide("ScreenRecorderTray");
const titleW = wide("螢幕錄影");
const taskbarCreated = U.RegisterWindowMessageW(ptr(wide("TaskbarCreated")));

/** NOTIFYICONDATAW（x64，976 bytes） */
function nid(flags: number, extra?: (v: DataView, b: Uint8Array) => void): Uint8Array {
  const b = new Uint8Array(976);
  const v = new DataView(b.buffer);
  v.setUint32(0, 976, true);
  v.setBigUint64(8, BigInt(hwnd ?? 0), true);
  v.setUint32(16, 1, true); // uID
  v.setUint32(20, flags, true);
  v.setUint32(24, WM_TRAY, true);
  extra?.(v, b);
  return b;
}
function putStr(b: Uint8Array, offset: number, maxChars: number, s: string) {
  const u = new Uint16Array(b.buffer, offset, maxChars);
  const t = s.slice(0, maxChars - 1);
  for (let i = 0; i < t.length; i++) u[i] = t.charCodeAt(i);
}
function iconData(flags: number) {
  const st = state;
  return nid(flags, (v, b) => {
    const kind: IconState = st?.rec === "recording" || st?.rec === "stopping" ? "recording" : st?.rec === "paused" ? "paused" : st?.rec === "countdown" ? "countdown" : "idle";
    v.setBigUint64(32, BigInt(icon(kind) as unknown as number), true);
    putStr(b, 40, 128, st?.tip ?? "螢幕錄影");
  });
}
function addIcon() {
  return shell32.symbols.Shell_NotifyIconW(NIM_ADD, ptr(iconData(NIF_MESSAGE | NIF_ICON | NIF_TIP))) !== 0;
}
function updateIcon() {
  shell32.symbols.Shell_NotifyIconW(NIM_MODIFY, ptr(iconData(NIF_ICON | NIF_TIP)));
}
function balloon(title: string, text: string, warn: boolean) {
  const b = nid(NIF_INFO, (v, buf) => {
    putStr(buf, 304, 256, text);
    putStr(buf, 820, 64, title);
    v.setUint32(948, warn ? NIIF_WARNING : NIIF_INFO, true);
  });
  shell32.symbols.Shell_NotifyIconW(NIM_MODIFY, ptr(b));
}
function removeIcon() {
  shell32.symbols.Shell_NotifyIconW(NIM_DELETE, ptr(nid(0)));
}

const send = (m: TrayToMain) => self.postMessage(m);

const wndProc = new JSCallback(
  (h: Pointer, msg: number, wParam: bigint | number, lParam: bigint | number) => {
    if (msg === WM_TRAY) {
      const ev = Number(BigInt(lParam) & 0xffffn);
      if (process.env.SR_TRAY_DEBUG) send({ type: "log", text: `tray event 0x${ev.toString(16)}` });
      if (ev === WM_RBUTTONUP) showMenu();
      else if (ev === WM_LBUTTONUP || ev === NIN_BALLOONUSERCLICK) send({ type: "cmd", cmd: "open" });
      return 0;
    }
    if (msg === WM_HOTKEY) {
      const hk = HOTKEYS.find((k) => k.id === Number(wParam));
      if (hk) send({ type: "cmd", cmd: hk.cmd });
      return 0;
    }
    if (msg === taskbarCreated && taskbarCreated) {
      addIcon(); // 檔案總管重新啟動後圖示會消失，要重新加入
      return 0;
    }
    return U.DefWindowProcW(h, msg, wParam, lParam);
  },
  { args: [P, u32, i64, i64], returns: i64 },
);

// ───────────── 選單 ─────────────

function showMenu() {
  const st = state;
  if (!st || !hwnd) return;
  const ids: TrayCommand[] = [];
  const add = (menu: Pointer, label: string, cmd: TrayCommand, opts: { disabled?: boolean; checked?: boolean } = {}) => {
    ids.push(cmd);
    U.AppendMenuW(menu, MF_STRING | (opts.disabled ? MF_GRAYED : 0) | (opts.checked ? MF_CHECKED : 0), ids.length as unknown as Pointer, ptr(wide(label)));
  };
  const sep = (menu: Pointer) => U.AppendMenuW(menu, MF_SEPARATOR, null, null);
  const sub = (menu: Pointer, label: string, child: Pointer, disabled = false) =>
    U.AppendMenuW(menu, MF_POPUP | (disabled ? MF_GRAYED : 0), child, ptr(wide(label)));

  const idle = st.rec === "idle";
  const m = U.CreatePopupMenu() as Pointer;
  add(m, "開啟操作視窗(&O)", "open");
  U.SetMenuDefaultItem(m, 0, 1);
  if (st.update) add(m, `★ 有新版本 v${st.update}（下載）`, "open-update");
  sep(m);
  // 「\t」後的文字顯示在選單右側（快捷鍵提示）
  add(m, `開始錄影(&R)　${st.lastSource}\tCtrl+Alt+R`, "start-last", { disabled: !idle || !st.canRecord });
  const pick = U.CreatePopupMenu() as Pointer;
  for (const mon of st.monitors) add(pick, mon.label, `start-monitor:${mon.id}`);
  if (st.monitors.length > 1) {
    sep(pick);
    add(pick, "所有螢幕（整個延伸桌面）", "start-all");
  }
  sub(m, "錄製指定螢幕(&M)", pick, !idle || !st.canRecord);
  if (st.rec === "paused") add(m, "繼續錄影(&C)\tCtrl+Alt+P", "resume");
  else add(m, "暫停(&P)\tCtrl+Alt+P", "pause", { disabled: st.rec !== "recording" });
  if (st.rec === "countdown") add(m, "取消倒數(&S)", "stop");
  else add(m, "停止並儲存(&S)\tCtrl+Alt+R", "stop", { disabled: idle || st.rec === "stopping" });
  sep(m);
  const audio = U.CreatePopupMenu() as Pointer;
  add(audio, "系統聲音", "toggle-system", { checked: st.audio.system });
  add(audio, "麥克風", "toggle-mic", { checked: st.audio.mic });
  sub(m, "錄製聲音(&A)", audio, !idle);
  sep(m);
  add(m, "開啟儲存資料夾(&F)", "open-folder");
  add(m, "播放最近的錄影(&L)", "play-last", { disabled: !st.lastResult });
  sep(m);
  add(m, "開機時自動啟動", "autostart", { checked: !!st.autostart, disabled: st.autostart === null });
  add(m, `更新說明（v${st.version}）`, "changelog");
  add(m, "結束(&X)", "quit");

  const pt = new Int32Array(2);
  U.GetCursorPos(ptr(pt));
  // SetForegroundWindow：否則點選單外面時選單不會關閉
  const fg = U.SetForegroundWindow(hwnd);
  const chosen = U.TrackPopupMenu(m, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, pt[0]!, pt[1]!, 0, hwnd, null);
  if (process.env.SR_TRAY_DEBUG) send({ type: "log", text: `menu returned at ${Date.now() % 100000} at ${pt[0]},${pt[1]} fg=${fg} result=${chosen} err=${kernel32.symbols.GetLastError()}` });
  U.PostMessageW(hwnd, WM_NULL, 0, 0);
  U.DestroyMenu(m); // 連同子選單一起釋放
  if (chosen > 0 && ids[chosen - 1]) send({ type: "cmd", cmd: ids[chosen - 1]! });
}

// ───────────── 啟動 ─────────────

function init() {
  allowDarkMenus();
  const hInst = kernel32.symbols.GetModuleHandleW(null);
  const wc = new Uint8Array(80);
  const v = new DataView(wc.buffer);
  v.setUint32(0, 80, true);
  v.setBigUint64(8, BigInt(wndProc.ptr as unknown as number), true);
  v.setBigUint64(24, BigInt((hInst ?? 0) as unknown as number), true);
  v.setBigUint64(64, BigInt(ptr(className) as unknown as number), true);
  U.RegisterClassExW(ptr(wc));
  hwnd = U.CreateWindowExW(0, ptr(className), ptr(titleW), 0, 0, 0, 0, 0, null, null, hInst, null) as Pointer | null;
  if (!hwnd) throw new Error("無法建立系統匣視窗");
}

// 訊息迴圈：用 PeekMessage 取出視窗訊息，Worker 的事件迴圈才能同時收主執行緒傳來的狀態
const msgBuf = new Uint8Array(48);
function pump() {
  while (U.PeekMessageW(ptr(msgBuf), null, 0, 0, 1 /* PM_REMOVE */)) {
    U.TranslateMessage(ptr(msgBuf));
    U.DispatchMessageW(ptr(msgBuf));
  }
}

let added = false;
self.onmessage = (e: MessageEvent<MainToTray>) => {
  const m = e.data;
  if (m.type === "state") {
    const first = !state;
    state = m.state;
    if (first) {
      added = addIcon();
      send(added ? { type: "ready" } : { type: "error", message: "Shell_NotifyIcon 失敗" });
    } else if (added) updateIcon();
  } else if (m.type === "balloon") {
    if (added) balloon(m.title, m.text, !!m.warn);
  } else if (m.type === "dispose") {
    if (added) removeIcon();
    if (hwnd) for (const hk of HOTKEYS) U.UnregisterHotKey(hwnd, hk.id);
    added = false;
    if (hwnd) U.PostMessageW(hwnd, WM_CLOSE, 0, 0);
    pump();
    for (const h of icons.values()) U.DestroyIcon(h);
    send({ type: "disposed" });
  } else if (m.type === "debug-menu") {
    // 測試用：模擬在圖示上按右鍵。真正點圖示時檔案總管會把前景權交給本程式；
    // 模擬時沒有，所以暫時連結前景視窗的輸入佇列以取得前景權，選單才會顯示
    if (!hwnd) return;
    const fgWin = U.GetForegroundWindow();
    const fgThread = fgWin ? U.GetWindowThreadProcessId(fgWin, null) : 0;
    const me = kernel32.symbols.GetCurrentThreadId();
    if (fgThread && fgThread !== me) U.AttachThreadInput(me, fgThread, 1);
    U.SetForegroundWindow(hwnd);
    if (fgThread && fgThread !== me) U.AttachThreadInput(me, fgThread, 0);
    U.PostMessageW(hwnd, WM_TRAY, 0, WM_RBUTTONUP);
  }
};

try {
  init();
  send({ type: "hwnd", hwnd: Number(hwnd) });
  // 快捷鍵登記在這個執行緒的視窗上（WM_HOTKEY 會送到這裡）；被其他程式占用時登記失敗
  const ok = HOTKEYS.map((hk) => U.RegisterHotKey(hwnd, hk.id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, hk.vk) !== 0);
  send({ type: "hotkeys", record: ok[0]!, pause: ok[1]! });
  // 在原生端等到有視窗訊息（點圖示、選單）或逾時才醒來：點擊立即反應，閒置時幾乎不耗 CPU。
  // 每輪之間讓出事件迴圈，處理主執行緒傳來的狀態（最慢延遲 WAIT_MS）。
  const WAIT_MS = 200, QS_ALLINPUT = 0x04ff;
  const loop = () => {
    U.MsgWaitForMultipleObjects(0, null, 0, WAIT_MS, QS_ALLINPUT);
    pump();
    setTimeout(loop, 0);
  };
  setTimeout(loop, 0);
} catch (e) {
  send({ type: "error", message: (e as Error).message });
}
