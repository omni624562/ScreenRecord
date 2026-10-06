/** 與 Windows 桌面整合：開啟操作視窗、開機自動啟動。 */
import { existsSync } from "node:fs";
import { join } from "node:path";
import { run } from "./ffmpeg.ts";
import { isCompiled } from "./paths.ts";
import { openWithExplorer } from "./server.ts";

const RUN_KEY = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_NAME = "ScreenRecorder";

/** 依序尋找可用 app 模式開啟的瀏覽器：Chrome（預設）→ Edge */
const BROWSERS = [
  ["Google", "Chrome", "Application", "chrome.exe"],
  ["Microsoft", "Edge", "Application", "msedge.exe"],
];

export function findAppBrowser(): string | undefined {
  const bases = [process.env.ProgramFiles, process.env["ProgramFiles(x86)"], process.env.LOCALAPPDATA].filter(Boolean) as string[];
  for (const parts of BROWSERS) {
    for (const b of bases) {
      const p = join(b, ...parts);
      if (existsSync(p)) return p;
    }
  }
  return undefined;
}

/**
 * 開啟操作視窗：用 Chrome 的 app 模式（沒有網址列與分頁的獨立視窗）；沒有 Chrome 用 Edge，
 * 兩者都沒有才交給預設瀏覽器。
 * detached：不放進本程式的 Job Object，關閉本程式時不會連帶關掉使用者的瀏覽器。
 */
export function openUi(url: string) {
  const browser = findAppBrowser();
  if (browser) {
    try {
      Bun.spawn([browser, `--app=${url}`, "--window-size=1280,900"], { stdin: "ignore", stdout: "ignore", stderr: "ignore", detached: true });
      return;
    } catch {
      // 改用預設瀏覽器
    }
  }
  openWithExplorer(url);
}
/** 開機自動啟動：寫入目前使用者的 Run 機碼（只有編譯版可用；啟動時只常駐系統匣，不開視窗） */
export async function getAutostart(): Promise<boolean | null> {
  if (!isCompiled) return null;
  const r = await run(["reg.exe", "query", RUN_KEY, "/v", RUN_NAME], 5000);
  return r.code === 0;
}

export async function setAutostart(on: boolean): Promise<void> {
  if (!isCompiled) throw new Error("開發模式無法設定開機自動啟動");
  const r = on
    ? await run(["reg.exe", "add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", `"${process.execPath}" --tray`, "/f"], 5000)
    : await run(["reg.exe", "delete", RUN_KEY, "/v", RUN_NAME, "/f"], 5000);
  if (r.code !== 0) throw new Error(`無法${on ? "設定" : "取消"}開機自動啟動：${r.stderr.trim()}`);
}
