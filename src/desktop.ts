/** 與 Windows 桌面整合：開啟操作視窗、開機自動啟動。 */
import { existsSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { run } from "./ffmpeg.ts";
import { logDir } from "./log.ts";
import { appDir, isCompiled } from "./paths.ts";
import { openWithExplorer } from "./server.ts";
import { APP_VERSION } from "./version.ts";

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

/** 操作視窗程式（Tauri / WebView2）的檔名開頭；編譯版內嵌時 Bun 會在後面加上雜湊 */
const SHELL_PREFIX = "screenrecorder-ui";

/**
 * 找操作視窗程式：
 * - 編譯版：內嵌在 exe 裡，第一次使用時取出到 %LOCALAPPDATA%\ScreenRecorder\ui（依版本分檔，執行中的舊版不會被覆寫）
 * - 開發時：shell\target\release（cargo build --release 的結果）
 */
async function findShell(): Promise<string | undefined> {
  if (!isCompiled) {
    const p = join(appDir, "shell", "target", "release", `${SHELL_PREFIX}.exe`);
    return existsSync(p) ? p : undefined;
  }
  const embedded = Bun.embeddedFiles.find((f) => (f as Blob & { name?: string }).name?.startsWith(SHELL_PREFIX));
  if (!embedded) return undefined;
  const dir = join(logDir, "ui");
  const target = join(dir, `${SHELL_PREFIX}-${APP_VERSION}.exe`);
  try {
    if (!existsSync(target) || statSync(target).size !== embedded.size) {
      mkdirSync(dir, { recursive: true });
      await Bun.write(target, embedded);
      // 清掉舊版本（執行中的刪不掉就留著，下次再清）
      for (const n of readdirSync(dir)) {
        if (n.startsWith(SHELL_PREFIX) && join(dir, n) !== target) rmSync(join(dir, n), { force: true });
      }
    }
    return target;
  } catch (e) {
    console.error(`無法取出操作視窗程式：${(e as Error).message}`);
    return existsSync(target) ? target : undefined;
  }
}

/**
 * 用 Tauri（WebView2）開啟操作視窗。WebView2 不能用（例如沒有安裝執行環境）時程式會很快以錯誤結束，
 * 這時回傳 false，改用瀏覽器。已經開著時，新啟動的程式把網址交給原視窗後正常結束。
 */
async function openShell(url: string): Promise<boolean> {
  if (process.env.SCREENRECORDER_UI === "browser") return false; // 疑難排解：強制用瀏覽器
  const exe = await findShell();
  if (!exe) return false;
  try {
    const proc = Bun.spawn(
      [exe, `--url=${url}`, `--title=螢幕錄影 v${APP_VERSION}`, `--data-dir=${join(logDir, "webview")}`],
      { stdin: "ignore", stdout: "ignore", stderr: "ignore", detached: true },
    );
    const code = await Promise.race([proc.exited, Bun.sleep(5000).then(() => undefined)]);
    if (code !== undefined && code !== 0) {
      console.error(`操作視窗程式無法啟動（結束代碼 ${code}），改用瀏覽器`);
      return false;
    }
    return true;
  } catch (e) {
    console.error(`操作視窗程式無法啟動：${(e as Error).message}，改用瀏覽器`);
    return false;
  }
}

/**
 * 開啟操作視窗：優先用內建的 Tauri 視窗（WebView2，Windows 11 內建）；不能用時改用 Chrome 的 app 模式
 * （沒有網址列與分頁的獨立視窗），沒有 Chrome 用 Edge，都沒有才交給預設瀏覽器。
 * detached：不放進本程式的 Job Object，關閉本程式時不會連帶關掉使用者的瀏覽器。
 * 使用獨立的設定檔（%LOCALAPPDATA%\ScreenRecorder\browser）：不帶使用者的擴充功能、翻譯提示，
 * 右鍵與視窗選單裡不會出現 Acrobat 之類的項目，也不影響使用者平常的瀏覽器。
 */
export async function openUi(url: string) {
  if (await openShell(url)) return;
  const browser = findAppBrowser();
  if (browser) {
    try {
      Bun.spawn([
        browser,
        `--app=${url}`,
        "--window-size=1280,900",
        `--user-data-dir=${join(logDir, "browser")}`,
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-sync",
        "--disable-features=Translate",
      ], { stdin: "ignore", stdout: "ignore", stderr: "ignore", detached: true });
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
