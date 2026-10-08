/**
 * 編譯單一 exe：版本號取自 package.json，寫進 exe 的檔案內容（右鍵 → 內容 → 詳細資料）。
 * 用法：bun run scripts/build.ts [--console]
 *   --console  另外編一個保留主控台視窗的版本（ScreenRecorder-console.exe），方便看即時訊息
 *
 * 操作視窗程式（target\release\screenrecorder-ui.exe，Tauri / WebView2）有編好時會一起內嵌，
 * 仍是單一 exe；沒有時只能用瀏覽器開操作視窗。先執行：cargo build --release -p screenrecorder-ui
 */
import { existsSync } from "node:fs";
import pkg from "../package.json";

const consoleBuild = process.argv.includes("--console");
const out = consoleBuild ? "./dist/ScreenRecorder-console.exe" : "./dist/ScreenRecorder.exe";
// Windows 檔案版本需要四段數字
const winVersion = [...pkg.version.split(".").map((n) => Number.parseInt(n, 10) || 0), 0, 0, 0].slice(0, 4).join(".");

async function step(cmd: string[]) {
  const p = Bun.spawn(cmd, { stdout: "inherit", stderr: "inherit" });
  if ((await p.exited) !== 0) process.exit(1);
}

await step(["bun", "run", "scripts/make-icon.ts"]);
const shellExe = "./target/release/screenrecorder-ui.exe";
const withShell = existsSync(shellExe);
console.log(withShell ? `內嵌操作視窗程式：${shellExe}` : "⚠ 找不到操作視窗程式（shell），操作視窗將使用瀏覽器開啟");
await step([
  "bun", "build", "--compile", "--target=bun-windows-x64",
  "--windows-icon=./assets/icon.ico",
  "--windows-title=ScreenRecorder",
  `--windows-description=螢幕錄影 ${pkg.version}`,
  `--windows-version=${winVersion}`,
  "./src/main.ts", "./src/tray-worker.ts",
  ...(withShell ? [shellExe] : []),
  "--outfile", out,
]);
// 一般版改成 GUI 程式：啟動時不顯示主控台視窗
if (!consoleBuild) await step(["bun", "run", "scripts/set-gui-subsystem.ts", out]);
console.log(`已建置 ${out}（版本 ${pkg.version}）`);
