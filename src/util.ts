/** 小工具。 */
import { existsSync } from "node:fs";
import { join } from "node:path";

/** 檔名已存在時加上 _2、_3…，避免覆蓋既有檔案 */
export function uniquePath(dir: string, base: string, ext: string): string {
  let p = join(dir, base + ext);
  for (let i = 2; existsSync(p); i++) p = join(dir, `${base}_${i}${ext}`);
  return p;
}

/** 逐行讀取子行程輸出 */
export async function readLines(stream: ReadableStream<Uint8Array>, onLine: (line: string) => void) {
  const dec = new TextDecoder();
  let buf = "";
  try {
    for await (const chunk of stream) {
      buf += dec.decode(chunk, { stream: true });
      let i: number;
      while ((i = buf.indexOf("\n")) >= 0) {
        onLine(buf.slice(0, i).replace(/\r$/, ""));
        buf = buf.slice(i + 1);
      }
    }
  } catch {
    // 行程結束時串流中斷
  }
  if (buf.trim()) onLine(buf.trim());
}
