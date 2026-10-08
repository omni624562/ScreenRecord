/**
 * 螢幕錄影（原速錄影 + 加速匯出）
 *
 * 參數：
 *   --port <n>   指定連接埠（預設 47391，被占用時自動往後找）
 *   --no-open    啟動後不自動開啟操作視窗
 *   --tray       只常駐系統匣、不開視窗（開機自動啟動時使用）
 *   --no-tray    不建立系統匣圖示、不檢查是否已在執行（開發測試用，可與正式程式並存）
 *
 * 編譯版不顯示主控台視窗，訊息寫入記錄檔；平常常駐在系統匣，從圖示選單操作或結束。
 */
import { App } from "./app.ts";
import { logFile, setupLogFile } from "./log.ts";
import { APP_VERSION } from "./version.ts";
import { appDir, isCompiled } from "./paths.ts";
import { openUi } from "./desktop.ts";
import { APP_ID, startServer } from "./server.ts";
import { Tray } from "./tray.ts";
import { pruneThumbnails } from "./thumbs.ts";

const DEFAULT_PORT = 47391;

function argValue(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

setupLogFile();

const preferredPort = Number(argValue("--port") ?? process.env.PORT ?? DEFAULT_PORT);
const autoOpen = !process.argv.includes("--no-open") && !process.argv.includes("--tray");
const noTray = process.argv.includes("--no-tray");
const PORT_RANGE = 20;

async function isInstance(port: number): Promise<boolean> {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/api/ping`, { signal: AbortSignal.timeout(800) });
    const data = (await r.json()) as { app?: string };
    return data.app === APP_ID;
  } catch {
    return false;
  }
}

/**
 * 找執行中的實例：預設埠被占用時實例可能落在後面的埠，所以整段都要找，
 * 否則會多開一個程式（系統匣出現兩個圖示）。
 */
async function findRunningInstance(): Promise<number | undefined> {
  const ports = new Set<number>();
  for (const base of [preferredPort, DEFAULT_PORT]) for (let i = 0; i < PORT_RANGE; i++) ports.add(base + i);
  const list = [...ports];
  const hits = await Promise.all(list.map(isInstance));
  return list.find((_, i) => hits[i]);
}

const runningPort = noTray ? undefined : await findRunningInstance();
if (runningPort !== undefined) {
  const url = `http://127.0.0.1:${runningPort}/`;
  console.log(`程式已在執行中：${url}`);
  if (autoOpen) await openUi(url);
  process.exit(0);
}

console.log(`螢幕錄影 ${APP_VERSION} — 原速錄影、事後加速匯出`);
console.log(`程式資料夾：${appDir}`);

const app = new App();
const env = await app.refresh();
const ff = env.ffmpeg;
if (!ff.found) {
  console.log("⚠ 找不到 ffmpeg.exe！請下載 FFmpeg（https://www.gyan.dev/ffmpeg/builds/），");
  console.log(`  把 bin\\ffmpeg.exe 放到 ${appDir}，再於網頁上按「重新偵測」。`);
} else {
  console.log(`FFmpeg ${ff.version}：${ff.path}`);
  console.log(`  ddagrab：${ff.hasDdagrab ? "有（測試中…）" : "無，將使用 gdigrab"}　編碼器：${ff.encoder ?? "無"}`);
  if (ff.error) console.log(`  ⚠ ${ff.error}`);
}
for (const m of env.monitors) {
  console.log(`  螢幕 ${m.displayNumber}${m.primary ? "（主螢幕）" : ""}：${m.width}×${m.height} @ (${m.x}, ${m.y})，${m.adapterName}`);
}
if (env.monitorError) console.log(`  ⚠ ${env.monitorError}`);

let server: ReturnType<typeof startServer> | undefined;
for (let port = preferredPort; port < preferredPort + PORT_RANGE && !server; port++) {
  try {
    server = startServer(app, port, !isCompiled);
  } catch (e) {
    if (!/EADDRINUSE|address already in use|in use/i.test(String((e as Error).message ?? e))) throw e;
  }
}
if (!server) {
  console.error(`無法啟動網頁伺服器（${preferredPort} 起的連接埠都被占用）`);
  process.exit(1);
}

const url = `http://127.0.0.1:${server.port}/`;
console.log(`\n介面網址：${url}`);
app.url = url;
console.log(`記錄檔：${logFile}`);

// 系統匣常駐：關掉操作視窗後程式仍在背景，從圖示選單操作或結束
const tray = new Tray(app);
app.tray = tray;
const trayOk = !noTray && process.platform === "win32" && (await tray.start());
console.log(trayOk ? "已常駐於系統匣：右鍵點圖示可錄影或結束程式\n" : "系統匣無法使用，關閉操作視窗 5 分鐘後會自動結束\n");
if (autoOpen) void openUi(url);
if (!noTray) app.scheduleUpdateChecks();
setTimeout(pruneThumbnails, 30_000);

// 系統匣無法使用時的退路：沒有主控台視窗，關掉分頁後程式就看不到了，
// 超過一段時間沒有任何頁面在輪詢狀態、而且沒在錄影 / 轉檔，就自動結束。
// （瀏覽器對背景分頁的計時器最慢會降到每分鐘一次，所以門檻設得寬一點）
const IDLE_EXIT_MS = 5 * 60_000;
if (isCompiled && !noTray) {
  setInterval(() => {
    // 系統匣啟動後才失效（圖示消失）也一樣適用，否則關掉視窗後就再也結束不了
    if (tray.ok || app.recorder.active || app.exporter.running) return;
    if (Date.now() - app.lastSeen > IDLE_EXIT_MS) {
      console.log("超過 5 分鐘沒有開著的頁面，自動結束程式");
      void app.quit();
    }
  }, 15_000);
}

// 沒接住的例外只記錄、不讓程式整個結束：錄影中的 FFmpeg 會跟著被終止，分段就合併不了
process.on("uncaughtException", (e) => console.error(`未預期的錯誤：${e?.stack ?? e}`));
process.on("unhandledRejection", (e) => console.error(`未處理的 Promise 錯誤：${(e as Error)?.stack ?? e}`));

// Ctrl+C / 關閉主控台視窗：先讓錄影正常收尾再結束；連按兩次強制結束
let signals = 0;
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK"] as const) {
  process.on(sig, () => {
    if (++signals > 1) process.exit(130);
    void app.quit();
  });
}
