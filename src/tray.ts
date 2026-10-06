/** 主執行緒端的系統匣控制：推送狀態給 Worker、執行選單指令、錄影完成時顯示通知。 */
import { existsSync, mkdirSync } from "node:fs";
import { basename } from "node:path";
import type { App } from "./app.ts";
import { getAutostart, openUi, setAutostart } from "./desktop.ts";
import { isCompiled } from "./paths.ts";
import { APP_VERSION } from "./version.ts";
import { loadSettings, saveSettings } from "./settings.ts";
import { openWithExplorer } from "./server.ts";
import { clock, videoClock } from "./shared/format.ts";
import type { RecordConfig } from "./shared/types.ts";
import type { MainToTray, TrayCommand, TrayState, TrayToMain } from "./tray-protocol.ts";

const STATE_TEXT = { idle: "待命", recording: "錄影中", paused: "已暫停", stopping: "儲存中" } as const;

export class Tray {
  private worker?: Worker;
  private last = "";
  private autostart: boolean | null = null;
  private lastResultPath?: string;
  private lastResultMsg?: string;
  private timer?: Timer;
  hwnd?: number;
  /** 系統匣是否正常運作（失敗時程式改用「沒有頁面就自動結束」的舊行為） */
  ok = false;

  constructor(private app: App) {}

  /** 啟動 Worker，等到圖示加入系統匣（或失敗）為止 */
  async start(): Promise<boolean> {
    this.autostart = await getAutostart().catch(() => null);
    const result = Promise.withResolvers<boolean>();
    try {
      // 編譯版的 Worker 要用與建置時相同的相對路徑字串；開發模式則相對於本檔案
      this.worker = new Worker(isCompiled ? "./tray-worker.ts" : new URL("./tray-worker.ts", import.meta.url).href);
    } catch (e) {
      console.error(`系統匣無法啟動：${(e as Error).message}`);
      return false;
    }
    this.worker.onmessage = (e: MessageEvent<TrayToMain>) => {
      const m = e.data;
      if (m.type === "ready") result.resolve(true);
      else if (m.type === "hwnd") this.hwnd = m.hwnd;
      else if (m.type === "error") {
        console.error(`系統匣無法啟動：${m.message}`);
        result.resolve(false);
      } else if (m.type === "cmd") void this.run(m.cmd);
      else if (m.type === "log") console.log(`[系統匣] ${m.text}`);
    };
    this.worker.onerror = (e) => {
      console.error(`系統匣錯誤：${e.message}`);
      result.resolve(false);
    };
    this.push(true);
    this.ok = await Promise.race([result.promise, Bun.sleep(5000).then(() => false)]);
    if (this.ok) this.timer = setInterval(() => this.tick(), 1000);
    return this.ok;
  }

  notify(title: string, text: string, warn = false) {
    this.post({ type: "balloon", title, text, warn });
  }

  /** 結束前移除圖示（否則會殘留到滑鼠移過去才消失） */
  async dispose() {
    clearInterval(this.timer);
    if (!this.worker) return;
    const done = new Promise<void>((r) => {
      const prev = this.worker!.onmessage;
      this.worker!.onmessage = (e) => (e.data?.type === "disposed" ? r() : prev?.call(this.worker!, e));
    });
    this.post({ type: "dispose" });
    await Promise.race([done, Bun.sleep(1000)]);
    this.worker.terminate();
  }

  debugMenu() {
    this.post({ type: "debug-menu" });
  }

  // ───────────── 內部 ─────────────

  private post(m: MainToTray) {
    this.worker?.postMessage(m);
  }

  private tick() {
    this.push();
    // 錄影結束時通知（點通知會開啟操作視窗）
    const res = this.app.recorder.status().result;
    if (res && (res.path ?? res.message) !== (this.lastResultPath ?? this.lastResultMsg)) {
      this.lastResultPath = res.path;
      this.lastResultMsg = res.message;
      if (res.ok && res.path) this.notify("錄影已儲存", `${basename(res.path)}（${videoClock(res.videoSec)}）`);
      else this.notify("錄影未完成", res.message, true);
    }
  }

  private state(): TrayState {
    const st = this.app.recorder.status();
    const env = this.app.env();
    const cfg = this.config();
    const time = st.state === "idle" ? "" : ` ${clock(st.recordedMs)}`;
    const src = cfg.source;
    const lastSource = src.type === "all" ? "所有螢幕"
      : src.type === "region" ? "自訂範圍"
      : `螢幕 ${env.monitors.find((m) => m.id === src.monitorId)?.displayNumber ?? 1}`;
    return {
      rec: st.state,
      tip: `螢幕錄影 ${APP_VERSION} — ${STATE_TEXT[st.state]}${time}`,
      lastSource,
      monitors: env.monitors.map((m) => ({
        id: m.id,
        label: `螢幕 ${m.displayNumber}${m.primary ? "（主螢幕）" : ""}　${m.width}×${m.height}`,
      })),
      audio: { system: cfg.audio.system, mic: cfg.audio.mic },
      canRecord: !!this.app.ffmpegPath() && !this.app.exporter.running,
      lastResult: this.lastResultPath && existsSync(this.lastResultPath) ? this.lastResultPath : undefined,
      autostart: this.autostart,
      version: APP_VERSION,
    };
  }

  private push(force = false) {
    const s = this.state();
    const key = JSON.stringify(s);
    if (!force && key === this.last) return;
    this.last = key;
    this.post({ type: "state", state: s });
  }

  /** 最後一次的錄影設定；沒有時用預設值（主螢幕、30 fps、100%、不錄聲音） */
  private config(): RecordConfig {
    const saved = loadSettings().config;
    if (saved) return { ...saved, audio: saved.audio ?? { system: false, mic: false, micId: "" } };
    const env = this.app.env();
    const primary = env.monitors.find((m) => m.primary) ?? env.monitors[0];
    return {
      source: primary ? { type: "monitor", monitorId: primary.id } : { type: "all" },
      fps: 30,
      scale: 100,
      drawMouse: true,
      maxMinutes: 0,
      method: "auto",
      outputDir: this.app.defaultOutputDir,
      audio: { system: false, mic: false, micId: "" },
    };
  }

  /** 改錄音設定時同步更新網頁的設定（網頁看到 rev 變了會重新讀取） */
  private updateAudio(key: "system" | "mic") {
    const cfg = this.config();
    cfg.audio = { ...cfg.audio, [key]: !cfg.audio[key] };
    const ui = { ...(loadSettings().ui ?? {}), [key === "system" ? "audioSystem" : "audioMic"]: cfg.audio[key] };
    saveSettings({ config: cfg, ui });
  }

  private async run(cmd: TrayCommand) {
    const app = this.app;
    try {
      if (cmd === "open") return openUi(app.url);
      if (cmd === "changelog") return openUi(`${app.url}#changelog`);
      if (cmd === "quit") return void app.quit();
      if (cmd === "pause") return void (await app.recorder.pause());
      if (cmd === "resume") return void (await app.recorder.resume());
      if (cmd === "stop") return void (await app.recorder.stop());
      if (cmd === "toggle-system") return this.updateAudio("system");
      if (cmd === "toggle-mic") return this.updateAudio("mic");
      if (cmd === "open-folder") {
        const dir = this.config().outputDir;
        mkdirSync(dir, { recursive: true });
        return openWithExplorer(dir);
      }
      if (cmd === "play-last") return this.lastResultPath && openWithExplorer(this.lastResultPath);
      if (cmd === "autostart") {
        await setAutostart(!this.autostart);
        this.autostart = await getAutostart();
        return this.notify("開機自動啟動", this.autostart ? "已開啟：登入 Windows 後會自動常駐在系統匣" : "已關閉");
      }
      // 開始錄影
      if (app.exporter.running) throw new Error("轉檔進行中，請等它完成再錄影");
      const cfg = this.config();
      if (cmd === "start-all") cfg.source = { type: "all" };
      else if (cmd.startsWith("start-monitor:")) cfg.source = { type: "monitor", monitorId: cmd.slice("start-monitor:".length) };
      await app.recorder.start(cfg);
    } catch (e) {
      this.notify("無法執行", (e as Error).message, true);
    } finally {
      this.push();
    }
  }
}
