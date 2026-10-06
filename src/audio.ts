/**
 * WASAPI 音訊擷取（bun:ffi 直接呼叫 Core Audio COM 介面）。
 *
 * FFmpeg 在 Windows 只能透過 dshow 錄麥克風，錄不到「電腦正在播放的聲音」，
 * 因此系統聲音（loopback）與麥克風都在這裡自行擷取，統一轉成 48 kHz / 立體聲 / float32，
 * 每個封包附上 QPC 時間戳，再由 AudioPipe 對齊畫面時間後送進 FFmpeg。
 */
import { dlopen, FFIType, ptr, toArrayBuffer, type Pointer } from "bun:ffi";
import { bindMethod, guid, hex, release, S_FALSE, S_OK, toWide, vcall, wstrAt } from "./com.ts";
import type { AudioDevice } from "./shared/types.ts";

export const SAMPLE_RATE = 48_000;
export const CHANNELS = 2;
export const BYTES_PER_FRAME = CHANNELS * 4;

const CLSID_MMDeviceEnumerator = guid("BCDE0395-E52F-467C-8E3D-C4579291692E");
const IID_IMMDeviceEnumerator = guid("A95664D2-9614-4F35-A746-DE8DB63617E6");
const IID_IAudioClient = guid("1CB9AD4C-DBFA-4C32-B178-C2F568A703B2");
const IID_IAudioCaptureClient = guid("C8ADBD64-E71E-48A0-A4DE-185C395CD317");
/** PKEY_Device_FriendlyName = {a45c254e-df1c-4efd-8020-67d146a850e0}, 14 */
const PKEY_FriendlyName = (() => {
  const k = new Uint8Array(20);
  k.set(guid("a45c254e-df1c-4efd-8020-67d146a850e0"));
  new DataView(k.buffer).setUint32(16, 14, true);
  return k;
})();

const CLSCTX_ALL = 0x17;
const eRender = 0;
const eCapture = 1;
const eConsole = 0;
const DEVICE_STATE_ACTIVE = 1;
const AUDCLNT_STREAMFLAGS_LOOPBACK = 0x0002_0000;
const AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM = 0x8000_0000;
const AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY = 0x0800_0000;
const AUDCLNT_BUFFERFLAGS_SILENT = 0x2;
const AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR = 0x4;
const RPC_E_CHANGED_MODE = 0x80010106 | 0;
const BUFFER_100NS = 20_000_000; // 2 秒緩衝：JS 事件迴圈偶爾停頓也不會掉資料

// vtable 索引
const VT_ENUM_ENUM_ENDPOINTS = 3;
const VT_ENUM_GET_DEFAULT = 4;
const VT_ENUM_GET_DEVICE = 5;
const VT_COLL_COUNT = 3;
const VT_COLL_ITEM = 4;
const VT_DEV_ACTIVATE = 3;
const VT_DEV_OPEN_PROPS = 4;
const VT_DEV_GET_ID = 5;
const VT_PROPS_GET_VALUE = 5;
const VT_AC_INITIALIZE = 3;
const VT_AC_START = 10;
const VT_AC_STOP = 11;
const VT_AC_GET_SERVICE = 14;
const VT_CC_GET_BUFFER = 3;
const VT_CC_RELEASE_BUFFER = 4;
const VT_CC_NEXT_PACKET = 5;

let libs: ReturnType<typeof openLibs> | undefined;
function openLibs() {
  return dlopen("ole32.dll", {
    CoInitializeEx: { args: [FFIType.ptr, FFIType.u32], returns: FFIType.i32 },
    CoCreateInstance: { args: [FFIType.ptr, FFIType.ptr, FFIType.u32, FFIType.ptr, FFIType.ptr], returns: FFIType.i32 },
    CoTaskMemFree: { args: [FFIType.ptr], returns: FFIType.void },
    PropVariantClear: { args: [FFIType.ptr], returns: FFIType.i32 },
  });
}

let enumerator = 0;
function getEnumerator(): number {
  libs ??= openLibs();
  if (enumerator) return enumerator;
  const hr0 = libs.symbols.CoInitializeEx(null, 0 /* COINIT_MULTITHREADED */);
  if (hr0 !== S_OK && hr0 !== S_FALSE && hr0 !== RPC_E_CHANGED_MODE) throw new Error(`CoInitializeEx 失敗 (${hex(hr0)})`);
  const out = new BigUint64Array(1);
  const hr = libs.symbols.CoCreateInstance(ptr(CLSID_MMDeviceEnumerator), null, CLSCTX_ALL, ptr(IID_IMMDeviceEnumerator), ptr(out));
  if (hr !== S_OK) throw new Error(`無法建立音訊裝置列舉器 (${hex(hr)})`);
  enumerator = Number(out[0]);
  return enumerator;
}

function deviceId(dev: number): string {
  const out = new BigUint64Array(1);
  if (vcall(dev, VT_DEV_GET_ID, [FFIType.ptr], ptr(out)) !== S_OK) return "";
  const p = Number(out[0]);
  const id = wstrAt(p);
  libs!.symbols.CoTaskMemFree(p as unknown as Pointer);
  return id;
}

function friendlyName(dev: number): string {
  const out = new BigUint64Array(1);
  if (vcall(dev, VT_DEV_OPEN_PROPS, [FFIType.u32, FFIType.ptr], 0 /* STGM_READ */, ptr(out)) !== S_OK) return "";
  const store = Number(out[0]);
  const pv = new Uint8Array(24); // PROPVARIANT
  try {
    if (vcall(store, VT_PROPS_GET_VALUE, [FFIType.ptr, FFIType.ptr], ptr(PKEY_FriendlyName), ptr(pv)) !== S_OK) return "";
    const v = new DataView(pv.buffer);
    const name = v.getUint16(0, true) === 31 /* VT_LPWSTR */ ? wstrAt(Number(v.getBigUint64(8, true))) : "";
    libs!.symbols.PropVariantClear(ptr(pv));
    return name;
  } finally {
    release(store);
  }
}

function defaultDevice(flow: number): number {
  const out = new BigUint64Array(1);
  const hr = vcall(getEnumerator(), VT_ENUM_GET_DEFAULT, [FFIType.u32, FFIType.u32, FFIType.ptr], flow, eConsole, ptr(out));
  return hr === S_OK ? Number(out[0]) : 0;
}

/** 列出可錄音的裝置（麥克風等）與預設播放裝置名稱。 */
export function listAudioDevices(): { render?: string; captures: AudioDevice[] } {
  const en = getEnumerator();
  const defCap = defaultDevice(eCapture);
  const defaultId = defCap ? deviceId(defCap) : "";
  release(defCap);
  const defRender = defaultDevice(eRender);
  const render = defRender ? friendlyName(defRender) : undefined;
  release(defRender);

  const captures: AudioDevice[] = [];
  const out = new BigUint64Array(1);
  if (vcall(en, VT_ENUM_ENUM_ENDPOINTS, [FFIType.u32, FFIType.u32, FFIType.ptr], eCapture, DEVICE_STATE_ACTIVE, ptr(out)) === S_OK) {
    const coll = Number(out[0]);
    const cnt = new Uint32Array(1);
    vcall(coll, VT_COLL_COUNT, [FFIType.ptr], ptr(cnt));
    for (let i = 0; i < cnt[0]!; i++) {
      if (vcall(coll, VT_COLL_ITEM, [FFIType.u32, FFIType.ptr], i, ptr(out)) !== S_OK) continue;
      const dev = Number(out[0]);
      const id = deviceId(dev);
      captures.push({ id, name: friendlyName(dev) || id, isDefault: id === defaultId });
      release(dev);
    }
    release(coll);
  }
  captures.sort((a, b) => Number(b.isDefault) - Number(a.isDefault));
  return { render, captures };
}

/**
 * 一個封包：data 是交錯排列的 float32 立體聲，指向共用的中轉區，只在回呼期間有效（要保留請自行複製）；
 * null 代表這段是靜音。qpc 為第一個取樣的 QPC 時間（100ns），時間戳無效時為 undefined。
 */
export type PacketVisitor = (data: Float32Array | null, frames: number, qpc: number | undefined) => void;

/**
 * 效能：Windows 上的 Bun 在計時器回呼中配置新的 TypedArray 每次約 300µs（實測），
 * 對新配置的陣列呼叫 ptr() 或用 toArrayBuffer 包外部記憶體也要 100～600µs；
 * 每秒上百個封包會吃掉 10% 以上的單核。所以擷取時完全不配置：
 * 以 RtlMoveMemory 複製到預先配置的中轉區（指標只取一次），由呼叫端直接處理。
 */
const kernelMem = dlopen("kernel32.dll", { RtlMoveMemory: { args: [FFIType.ptr, FFIType.ptr, FFIType.u64], returns: FFIType.void } }).symbols;
const STAGING = new Float32Array(SAMPLE_RATE * CHANNELS);
const STAGING_PTR = ptr(STAGING);
const STAGING_FRAMES = STAGING.length / CHANNELS;

/** 48 kHz / 2ch / float32 的 WAVEFORMATEX（搭配 AUTOCONVERTPCM，任何裝置都轉成同一格式） */
const WAVE_FORMAT = (() => {
  const b = new Uint8Array(18);
  const v = new DataView(b.buffer);
  v.setUint16(0, 3, true); // WAVE_FORMAT_IEEE_FLOAT
  v.setUint16(2, CHANNELS, true);
  v.setUint32(4, SAMPLE_RATE, true);
  v.setUint32(8, SAMPLE_RATE * BYTES_PER_FRAME, true);
  v.setUint16(12, BYTES_PER_FRAME, true);
  v.setUint16(14, 32, true);
  v.setUint16(16, 0, true);
  return b;
})();

/** 一個 WASAPI 擷取串流：loopback = 錄預設播放裝置的聲音；否則錄指定（或預設）麥克風。 */
export class WasapiCapture {
  private device = 0;
  private client = 0;
  private capture = 0;
  readonly name: string;
  // 重複使用的輸出參數緩衝：放在同一塊記憶體，指標只取一次
  private outBuf = new ArrayBuffer(48);
  private pData = new BigUint64Array(this.outBuf, 0, 1);
  private pDevPos = new BigUint64Array(this.outBuf, 8, 1);
  private pQpc = new BigUint64Array(this.outBuf, 16, 1);
  private pFrames = new Uint32Array(this.outBuf, 24, 1);
  private pFlags = new Uint32Array(this.outBuf, 28, 1);
  private pNext = new Uint32Array(this.outBuf, 32, 1);
  private outPtr = ptr(this.outBuf);
  // 擷取時每秒呼叫上百次的方法：建立時綁好
  private nextPacket?: (...v: unknown[]) => number;
  private getBuffer?: (...v: unknown[]) => number;
  private releaseBuffer?: (...v: unknown[]) => number;

  constructor(readonly loopback: boolean, micId?: string) {
    const en = getEnumerator();
    if (loopback) {
      this.device = defaultDevice(eRender);
      if (!this.device) throw new Error("找不到播放裝置");
    } else if (micId) {
      const out = new BigUint64Array(1);
      const wide = toWide(micId);
      if (vcall(en, VT_ENUM_GET_DEVICE, [FFIType.ptr, FFIType.ptr], ptr(wide), ptr(out)) === S_OK) this.device = Number(out[0]);
    }
    if (!this.device && !loopback) this.device = defaultDevice(eCapture);
    if (!this.device) throw new Error("找不到麥克風");
    this.name = friendlyName(this.device) || (loopback ? "播放裝置" : "麥克風");

    try {
      const out = new BigUint64Array(1);
      let hr = vcall(this.device, VT_DEV_ACTIVATE, [FFIType.ptr, FFIType.u32, FFIType.ptr, FFIType.ptr], ptr(IID_IAudioClient), CLSCTX_ALL, null, ptr(out));
      if (hr !== S_OK) throw new Error(`Activate 失敗 (${hex(hr)})`);
      this.client = Number(out[0]);
      // JS 位元運算結果是有號 int32，>>> 0 轉回無號，否則最高位元的旗標會傳錯
      const flags = ((loopback ? AUDCLNT_STREAMFLAGS_LOOPBACK : 0) | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY) >>> 0;
      hr = vcall(this.client, VT_AC_INITIALIZE, [FFIType.u32, FFIType.u32, FFIType.i64, FFIType.i64, FFIType.ptr, FFIType.ptr],
        0 /* SHARED */, flags, BUFFER_100NS, 0, ptr(WAVE_FORMAT), null);
      if (hr !== S_OK) throw new Error(`Initialize 失敗 (${hex(hr)})`);
      hr = vcall(this.client, VT_AC_GET_SERVICE, [FFIType.ptr, FFIType.ptr], ptr(IID_IAudioCaptureClient), ptr(out));
      if (hr !== S_OK) throw new Error(`GetService 失敗 (${hex(hr)})`);
      this.capture = Number(out[0]);
      this.nextPacket = bindMethod(this.capture, VT_CC_NEXT_PACKET, [FFIType.ptr]);
      this.getBuffer = bindMethod(this.capture, VT_CC_GET_BUFFER, [FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr, FFIType.ptr]);
      this.releaseBuffer = bindMethod(this.capture, VT_CC_RELEASE_BUFFER, [FFIType.u32]);
      hr = vcall(this.client, VT_AC_START, []);
      if (hr !== S_OK) throw new Error(`Start 失敗 (${hex(hr)})`);
    } catch (e) {
      this.close();
      throw e;
    }
  }

  /** 依序處理目前所有可讀的封包（visit 期間資料有效）；裝置失效（拔除、切換）時丟出例外。 */
  read(visit: PacketVisitor): void {
    const p = this.outPtr;
    for (;;) {
      let hr = this.nextPacket!(p + 32);
      if (hr !== S_OK) throw new Error(`音訊裝置中斷 (${hex(hr)})`);
      if (this.pNext[0] === 0) return;
      hr = this.getBuffer!(p, p + 24, p + 28, p + 8, p + 16);
      if (hr < 0) throw new Error(`音訊裝置中斷 (${hex(hr)})`);
      const frames = this.pFrames[0]!;
      const flags = this.pFlags[0]!;
      const qpc = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR ? undefined : Number(this.pQpc[0]);
      try {
        if (frames === 0) continue;
        if (flags & AUDCLNT_BUFFERFLAGS_SILENT) {
          visit(null, frames, qpc);
          continue;
        }
        // 封包通常約 10ms；超過中轉區（1 秒）時分段處理，時間戳依取樣數往後推
        const src = Number(this.pData[0]);
        for (let off = 0; off < frames; off += STAGING_FRAMES) {
          const n = Math.min(STAGING_FRAMES, frames - off);
          kernelMem.RtlMoveMemory(STAGING_PTR, src + off * CHANNELS * 4, n * CHANNELS * 4);
          visit(STAGING.subarray(0, n * CHANNELS), n, qpc === undefined ? undefined : qpc + Math.round((off * 1e7) / SAMPLE_RATE));
        }
      } finally {
        this.releaseBuffer!(frames);
      }
    }
  }

  close() {
    if (this.client) vcall(this.client, VT_AC_STOP, []);
    release(this.capture);
    release(this.client);
    release(this.device);
    this.capture = this.client = this.device = 0;
  }
}

