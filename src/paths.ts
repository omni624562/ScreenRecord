import { basename, dirname, join, resolve } from "node:path";
import { homedir } from "node:os";

/** 是否為 bun build --compile 產生的單一執行檔 */
export const isCompiled = basename(process.execPath).toLowerCase() !== "bun.exe" &&
  basename(process.execPath).toLowerCase() !== "bun";

/** 程式所在資料夾：編譯版為 exe 所在處；開發時為專案根目錄 */
export const appDir = isCompiled ? dirname(process.execPath) : resolve(import.meta.dir, "..");

export function defaultOutputDir(): string {
  const profile = process.env.USERPROFILE || homedir();
  return join(profile, "Videos", "Timelapse");
}

const pad = (n: number) => String(n).padStart(2, "0");

/** 2026-10-05_14-30-05 */
export function timestamp(d = new Date()): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}_${pad(d.getHours())}-${pad(d.getMinutes())}-${pad(d.getSeconds())}`;
}
