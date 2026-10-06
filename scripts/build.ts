/**
 * 編譯單一 exe：版本號取自 package.json，寫進 exe 的檔案內容（右鍵 → 內容 → 詳細資料）。
 * 用法：bun run scripts/build.ts [--console]
 *   --console  另外編一個保留主控台視窗的版本（ScreenRecorder-console.exe），方便看即時訊息
 */
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
await step([
  "bun", "build", "--compile", "--target=bun-windows-x64",
  "--windows-icon=./assets/icon.ico",
  "--windows-title=ScreenRecorder",
  `--windows-description=螢幕錄影 ${pkg.version}`,
  `--windows-version=${winVersion}`,
  "./src/main.ts", "./src/tray-worker.ts",
  "--outfile", out,
]);
// 一般版改成 GUI 程式：啟動時不顯示主控台視窗
if (!consoleBuild) await step(["bun", "run", "scripts/set-gui-subsystem.ts", out]);
console.log(`已建置 ${out}（版本 ${pkg.version}）`);
