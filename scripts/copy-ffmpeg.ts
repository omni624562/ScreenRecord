/**
 * 把 PATH 上的 ffmpeg.exe 複製到 dist\，組成「exe + ffmpeg.exe 同資料夾」的發佈版。
 * 用法：bun run dist
 */
import { copyFileSync, existsSync, statSync } from "node:fs";
import { join, resolve } from "node:path";

const dist = resolve(import.meta.dir, "..", "dist");
const target = join(dist, "ffmpeg.exe");
if (existsSync(target)) {
  console.log(`dist\\ffmpeg.exe 已存在，略過（${(statSync(target).size / 1048576).toFixed(0)} MB）`);
  process.exit(0);
}
const src = Bun.which("ffmpeg");
if (!src) {
  console.error("PATH 上找不到 ffmpeg.exe；請自行下載並放到 dist\\ 資料夾（https://www.gyan.dev/ffmpeg/builds/）");
  process.exit(1);
}
copyFileSync(src, target);
console.log(`已複製 ${src} → ${target}（${(statSync(target).size / 1048576).toFixed(0)} MB）`);
