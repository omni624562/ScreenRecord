/** 以 bun:ffi 呼叫 COM 介面的小工具（DXGI、WASAPI 共用）。 */
import { CFunction, dlopen, FFIType, read, type Pointer } from "bun:ffi";

export const S_OK = 0;
export const S_FALSE = 1;

type Fn = (...args: unknown[]) => unknown;
const fnCache = new Map<string, Fn>();

/**
 * 取得 COM 物件 vtable 上第 index 個方法（第一個參數一律是 this）。
 * 同一類別的方法指標固定，快取後不必每次重新產生呼叫橋接（音訊每秒要呼叫上百次）。
 */
function method(obj: number, index: number, args: FFIType[], returns: FFIType): Fn {
  const vtbl = read.ptr(obj as Pointer, 0);
  const fn = read.ptr(vtbl as Pointer, index * 8);
  const key = `${fn}|${args.join(",")}|${returns}`;
  let f = fnCache.get(key);
  if (!f) {
    f = CFunction({ ptr: fn as Pointer, args: [FFIType.ptr, ...args], returns }) as unknown as Fn;
    fnCache.set(key, f);
  }
  return f;
}

/** 呼叫回傳 HRESULT 的 COM 方法 */
export function vcall(obj: number, index: number, args: FFIType[], ...values: unknown[]): number {
  return method(obj, index, args, FFIType.i32)(obj, ...values) as number;
}

/**
 * 先把某個物件的某個方法綁好，之後直接呼叫：省掉每次讀 vtable、組快取鍵、查表的成本。
 * 用在每秒要呼叫上百次的地方（音訊擷取），物件存活期間有效。
 */
export function bindMethod(obj: number, index: number, args: FFIType[], returns: FFIType = FFIType.i32): (...values: unknown[]) => number {
  const f = method(obj, index, args, returns);
  return (...values) => f(obj, ...values) as number;
}

export function release(obj: number | undefined) {
  if (obj) method(obj, 2, [], FFIType.u32)(obj);
}

/** "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" → GUID 位元組（前三段為 little-endian） */
export function guid(s: string): Uint8Array {
  const h = s.replace(/[{}-]/g, "");
  const b = new Uint8Array(16);
  const v = new DataView(b.buffer);
  v.setUint32(0, Number.parseInt(h.slice(0, 8), 16), true);
  v.setUint16(4, Number.parseInt(h.slice(8, 12), 16), true);
  v.setUint16(6, Number.parseInt(h.slice(12, 16), 16), true);
  for (let i = 0; i < 8; i++) b[8 + i] = Number.parseInt(h.slice(16 + i * 2, 18 + i * 2), 16);
  return b;
}

/** 讀取 buffer 內的 UTF-16 字串（遇到 0 結束） */
export function wstr(buf: Uint8Array, offset: number, maxChars: number): string {
  const u16 = new Uint16Array(buf.buffer, buf.byteOffset + offset, maxChars);
  const end = u16.indexOf(0);
  return String.fromCharCode(...u16.subarray(0, end < 0 ? maxChars : end));
}

/** 讀取原生記憶體中的 UTF-16 字串（例如 CoTaskMemAlloc 配置的 LPWSTR） */
export function wstrAt(p: number, maxChars = 1024): string {
  let s = "";
  for (let i = 0; i < maxChars; i++) {
    const c = read.u16(p as Pointer, i * 2);
    if (c === 0) break;
    s += String.fromCharCode(c);
  }
  return s;
}

export const hex = (hr: number) => `0x${(hr >>> 0).toString(16).padStart(8, "0")}`;

/** UTF-16 + 結尾 0，用來傳 LPCWSTR 參數 */
export function toWide(s: string): Uint16Array {
  const a = new Uint16Array(s.length + 1);
  for (let i = 0; i < s.length; i++) a[i] = s.charCodeAt(i);
  return a;
}

let kernel: ReturnType<typeof openKernel> | undefined;
function openKernel() {
  return dlopen("kernel32.dll", {
    QueryPerformanceCounter: { args: [FFIType.ptr], returns: FFIType.i32 },
    QueryPerformanceFrequency: { args: [FFIType.ptr], returns: FFIType.i32 },
  });
}
let qpcFreq = 0n;
const qpcBuf = new BigInt64Array(1);

/**
 * 目前的 QPC 時間（100ns 單位）。WASAPI 封包時間戳與 FFmpeg 的 av_gettime_relative()
 * 在 Windows 上都以 QPC 為基準，用同一個時鐘才能讓聲音與畫面長時間對齊。
 */
export function qpcNow100ns(): number {
  kernel ??= openKernel();
  if (!qpcFreq) {
    kernel.symbols.QueryPerformanceFrequency(qpcBuf);
    qpcFreq = qpcBuf[0]!;
  }
  kernel.symbols.QueryPerformanceCounter(qpcBuf);
  return Number((qpcBuf[0]! * 10_000_000n) / qpcFreq);
}
