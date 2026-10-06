/**
 * 螢幕錄影（原速錄影 + 加速匯出）
 *
 * 參數：
 *   --port <n>   指定連接埠（預設 47391，被占用時自動往後找）
 *   --no-open    啟動後不自動開啟操作視窗
 *   --tray       只常駐系統匣、不開視窗（開機自動啟動時使用）
 *
 * 編譯版不顯示主控台視窗，訊息寫入記錄檔；平常常駐在系統匣，從圖示選單操作或結束。
 */
import { App } from "./app.ts";
import { logFile, setupLogFile } from "./log.ts";
import { appDir, isCompiled } from "./paths.ts";
import { openUi } from "./desktop.ts";
import { APP_ID, startServer } from "./server.ts";
import { Tray } from "./tray.ts";

const DEFAULT_PORT = 47391;

function argValue(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

setupLogFile();

const preferredPort = Number(argValue("--port") ?? process.env.PORT ?? DEFAULT_PORT);
const autoOpen = !process.argv.includes("--no-open") && !process.argv.includes("--tray");

/** 已有一個執行中的實例時，直接開啟它的頁面 */
async function findRunningInstance(port: number): Promise<boolean> {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/api/ping`, { signal: AbortSignal.timeout(800) });
    const data = (await r.json()) as { app?: string };
    return data.app === APP_ID;
  } catch {
    return false;
  }
}

if (await findRunningInstance(preferredPort)) {
  const url = `http://127.0.0.1:${preferredPort}/`;
  console.log(`程式已在執行中：${url}`);
  if (autoOpen) openUi(url);
  process.exit(0);
}

console.log("螢幕錄影 — 原速錄影、事後加速匯出");
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
for (let port = preferredPort; port < preferredPort + 20 && !server; port++) {
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
const trayOk = process.platform === "win32" && (await tray.start());
console.log(trayOk ? "已常駐於系統匣：右鍵點圖示可錄影或結束程式\n" : "系統匣無法使用，關閉操作視窗 5 分鐘後會自動結束\n");
if (autoOpen) openUi(url);

// 系統匣無法使用時的退路：沒有主控台視窗，關掉分頁後程式就看不到了，
// 超過一段時間沒有任何頁面在輪詢狀態、而且沒在錄影 / 轉檔，就自動結束。
// （瀏覽器對背景分頁的計時器最慢會降到每分鐘一次，所以門檻設得寬一點）
const IDLE_EXIT_MS = 5 * 60_000;
if (isCompiled && !trayOk) {
  setInterval(() => {
    if (app.recorder.active || app.exporter.running) return;
    if (Date.now() - app.lastSeen > IDLE_EXIT_MS) {
      console.log("超過 5 分鐘沒有開著的頁面，自動結束程式");
      void app.quit();
    }
  }, 15_000);
}

// Ctrl+C / 關閉主控台視窗：先讓錄影正常收尾再結束；連按兩次強制結束
let signals = 0;
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK"] as const) {
  process.on(sig, () => {
    if (++signals > 1) process.exit(130);
    void app.quit();
  });
}
