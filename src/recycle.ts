/** 把檔案移到資源回收筒（SHFileOperationW + FOF_ALLOWUNDO，可從資源回收筒還原）。 */
import { dlopen, FFIType, ptr } from "bun:ffi";

const FO_DELETE = 3;
const FOF_SILENT = 0x4;
const FOF_NOCONFIRMATION = 0x10;
const FOF_ALLOWUNDO = 0x40;
const FOF_NOERRORUI = 0x400;

let shell: ReturnType<typeof open> | undefined;
function open() {
  return dlopen("shell32.dll", { SHFileOperationW: { args: [FFIType.ptr], returns: FFIType.i32 } });
}

export function moveToRecycleBin(paths: string[]): void {
  if (paths.length === 0) return;
  shell ??= open();
  // pFrom：每個路徑以 \0 分隔，最後再多一個 \0
  const joined = paths.join("\0") + "\0\0";
  const from = new Uint16Array(joined.length);
  for (let i = 0; i < joined.length; i++) from[i] = joined.charCodeAt(i);

  // SHFILEOPSTRUCTW（x64，56 bytes）
  const op = new Uint8Array(56);
  const v = new DataView(op.buffer);
  v.setUint32(8, FO_DELETE, true);
  v.setBigUint64(16, BigInt(ptr(from) as unknown as number), true);
  v.setUint16(32, FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI, true);
  const r = shell.symbols.SHFileOperationW(ptr(op));
  if (r !== 0) throw new Error(`無法移到資源回收筒（代碼 0x${(r >>> 0).toString(16)}），檔案可能正在使用中`);
  if (v.getInt32(36, true)) throw new Error("刪除動作被中止");
}
