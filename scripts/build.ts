/**
 * 建置單一 exe（Rust：本機 API、錄影、系統匣與 Tauri / WebView2 操作視窗都在同一個執行檔）。
 * 版本號取自 package.json，寫進 exe 的檔案內容（右鍵 → 內容 → 詳細資料）；網頁介面由 Bun 打包後內嵌。
 * 用法：bun run build（需要安裝 Rust：https://rustup.rs/）
 */
import { copyFileSync, mkdirSync, statSync } from "node:fs";
import pkg from "../package.json";

const locked = process.argv.includes("--locked");

async function step(cmd: string[]) {
  const p = Bun.spawn(cmd, { stdout: "inherit", stderr: "inherit" });
  if ((await p.exited) !== 0) process.exit(1);
}

await step(["bun", "run", "scripts/make-icon.ts"]);
await step(["cargo", "build", "--release", ...(locked ? ["--locked"] : []), "-p", "screenrecorder-ui"]);
mkdirSync("./dist", { recursive: true });
const out = "./dist/ScreenRecorder.exe";
copyFileSync("./target/release/screenrecorder-ui.exe", out);
console.log(`已建置 ${out}（版本 ${pkg.version}，${(statSync(out).size / 1048576).toFixed(1)} MB）`);
