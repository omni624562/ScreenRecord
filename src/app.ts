/** 全域狀態：FFmpeg 偵測結果、螢幕與音訊裝置清單、錄影器與匯出器。 */
import { desktopRect, encoderSpec, previewArgs, type EncoderSpec } from "./args.ts";
import { listAudioDevices } from "./audio.ts";
import { FfmpegDownloader } from "./downloader.ts";
import { Exporter } from "./exporter.ts";
import { probeFfmpeg, testDdagrab, testHwEncoders } from "./ffmpeg.ts";
import { enumerateMonitors } from "./monitors.ts";
import { appDir, defaultOutputDir } from "./paths.ts";
import { Recorder } from "./recorder.ts";
import { loadSettings, saveSettings } from "./settings.ts";
import type { EnvInfo, FfmpegInfo, MonitorInfo } from "./shared/types.ts";
import { APP_VERSION } from "./version.ts";

export class App {
  readonly defaultOutputDir = defaultOutputDir();
  private ffmpeg: FfmpegInfo & { encoderSpec?: EncoderSpec; hwListed?: string[] } = { found: false, hasDdagrab: false, hasGdigrab: false, searched: [] };
  private monitors: MonitorInfo[] = [];
  private monitorError?: string;
  private audio: EnvInfo["audio"] = { captures: [] };
  private quitting = false;
  /** 最後一次有頁面來查狀態的時間（判斷分頁是否都關了） */
  lastSeen = Date.now();
  /** 操作介面網址（伺服器啟動後設定） */
  url = "";
  tray?: { dispose(): Promise<void> };

  readonly recorder = new Recorder({
    ffmpegPath: () => this.ffmpegPath(),
    encoders: () => this.encoders(),
    monitors: () => this.monitors,
    ddagrabUsable: () => this.ddagrabReady(),
    preferGpu: () => !!loadSettings().preferGpu,
    learnGpu: () => {
      saveSettings({ preferGpu: true });
      this.ffmpeg.preferGpu = true;
    },
  });

  /** 下載完成後重新偵測，介面輪詢時就會看到 FFmpeg 已就緒 */
  readonly downloader = new FfmpegDownloader({
    onDone: async () => {
      await this.refresh();
    },
  });

  readonly exporter = new Exporter({
    ffmpegPath: () => this.ffmpegPath(),
    encoder: () => this.ffmpeg.encoderSpec,
  });

  ffmpegPath() {
    return this.ffmpeg.found ? this.ffmpeg.path : undefined;
  }

  private hwTest?: Promise<void>;

  /** 軟體編碼器與實測可用的硬體編碼器（硬體測試還沒做完就先等它） */
  private async encoders(): Promise<{ cpu?: EncoderSpec; gpu: EncoderSpec[] }> {
    await this.hwTest;
    return {
      cpu: this.ffmpeg.encoderSpec,
      gpu: (this.ffmpeg.hwEncoders ?? []).map((n) => encoderSpec(n)!).filter(Boolean),
    };
  }

  /** 清除「自動改用 GPU」的紀錄（例如換了更快的電腦，或當時只是暫時忙碌） */
  resetLearnedGpu() {
    saveSettings({ preferGpu: false });
    this.ffmpeg.preferGpu = false;
  }

  private ddagrabUsable() {
    return this.ffmpeg.hasDdagrab && this.ffmpeg.ddagrabWorks !== false;
  }

  /**
   * 錄影前確認 ddagrab：啟動時的實測可能剛好遇到鎖定畫面或 UAC 安全桌面而失敗
   * （例如開機自動啟動時），上次失敗就重測一次，不要整段期間都只能用 gdigrab。
   */
  private async ddagrabReady(): Promise<boolean> {
    if (!this.ffmpeg.hasDdagrab) return false;
    if (this.ffmpeg.ddagrabWorks === false) await this.testDdagrab();
    return this.ffmpeg.ddagrabWorks !== false;
  }

  env(): EnvInfo {
    const { encoderSpec: _e, hwListed: _h, ...ffmpeg } = this.ffmpeg;
    return {
      appDir,
      appVersion: APP_VERSION,
      defaultOutputDir: this.defaultOutputDir,
      ffmpeg,
      monitors: this.monitors,
      monitorError: this.monitorError,
      desktop: desktopRect(this.monitors),
      audio: this.audio,
    };
  }

  refreshDevices() {
    try {
      this.monitors = enumerateMonitors();
      this.monitorError = this.monitors.length ? undefined : "找不到任何螢幕";
    } catch (e) {
      this.monitors = [];
      this.monitorError = `無法列舉螢幕：${(e as Error).message}`;
    }
    try {
      this.audio = listAudioDevices();
    } catch (e) {
      this.audio = { captures: [], error: `無法列舉音訊裝置：${(e as Error).message}` };
    }
  }

  /** 重新偵測 FFmpeg、螢幕與音訊裝置；ddagrab 實測在背景進行。 */
  async refresh(waitDdagrab = false): Promise<EnvInfo> {
    this.refreshDevices();
    const info = await probeFfmpeg();
    // 保留上一次成功的 ddagrab 實測結果（同一個執行檔），避免每次重新整理都要等；失敗則重測
    if (info.path === this.ffmpeg.path && this.ffmpeg.ddagrabWorks) {
      info.ddagrabWorks = this.ffmpeg.ddagrabWorks;
      info.ddagrabError = this.ffmpeg.ddagrabError;
    }
    // 硬體編碼器實測結果同樣保留（換了 ffmpeg.exe 才重測）
    if (info.path === this.ffmpeg.path && this.ffmpeg.hwEncoders) info.hwEncoders = this.ffmpeg.hwEncoders;
    info.preferGpu = !!loadSettings().preferGpu;
    this.ffmpeg = info;
    if (info.found && !info.hwEncoders) {
      const path = info.path!;
      // 測試出錯（例如 ffmpeg.exe 中途被刪除）時視為「沒有可用的 GPU」，不能讓之後每次錄影都因此失敗
      this.hwTest = testHwEncoders(path, info.hwListed ?? [])
        .catch((e: Error) => {
          console.error(`GPU 編碼器測試失敗：${e.message}`);
          return [] as string[];
        })
        .then((list) => {
          if (this.ffmpeg.path !== path) return;
          this.ffmpeg.hwEncoders = list;
          console.log(list.length ? `可用的 GPU 編碼器：${list.join("、")}` : "沒有可用的 GPU 編碼器，將使用 CPU 編碼");
        });
    }
    if (info.found && info.hasDdagrab && info.ddagrabWorks === undefined) {
      const test = this.testDdagrab();
      if (waitDdagrab) await test;
    }
    return this.env();
  }

  private async testDdagrab() {
    const path = this.ffmpeg.path!;
    const primary = this.monitors.find((m) => m.primary) ?? this.monitors[0];
    const r = await testDdagrab(path, primary);
    if (this.ffmpeg.path !== path) return;
    this.ffmpeg.ddagrabWorks = r.ok;
    this.ffmpeg.ddagrabError = r.error;
    console.log(r.ok ? "ddagrab 測試成功，將優先使用 GPU 擷取" : `ddagrab 測試失敗，將改用 gdigrab：${r.error}`);
  }

  /** 整個桌面的預覽 JPEG；ddagrab 失敗時退回 gdigrab */
  async preview(): Promise<ArrayBuffer | undefined> {
    const ffmpeg = this.ffmpegPath();
    if (!ffmpeg) return undefined;
    const grab = async (dda: boolean) => {
      const p = Bun.spawn([ffmpeg, ...previewArgs(this.monitors, dda)], { stdin: "ignore", stdout: "pipe", stderr: "ignore", windowsHide: true });
      const timer = setTimeout(() => p.kill(), 10_000);
      const [img, code] = await Promise.all([new Response(p.stdout).arrayBuffer(), p.exited]);
      clearTimeout(timer);
      return code === 0 && img.byteLength > 0 ? img : undefined;
    };
    const dda = this.ddagrabUsable() && this.monitors.length > 0;
    return (dda ? await grab(true) : undefined) ?? (await grab(false));
  }

  private live?: { kill(): void };

  /**
   * 即時預覽串流（multipart JPEG）。同時只保留一條：新的連線會結束舊的，
   * 瀏覽器斷線（關閉視窗、切換分頁、改張數）時 FFmpeg 也跟著結束。
   */
  livePreview(fps: number, signal: AbortSignal): ReadableStream<Uint8Array> | undefined {
    const ffmpeg = this.ffmpegPath();
    if (!ffmpeg) return undefined;
    this.live?.kill();
    const p = Bun.spawn([ffmpeg, ...previewArgs(this.monitors, this.ddagrabUsable() && this.monitors.length > 0, 1280, fps)], {
      stdin: "ignore", stdout: "pipe", stderr: "ignore", windowsHide: true,
    });
    const kill = () => {
      try {
        p.kill();
      } catch {
        // 已結束
      }
    };
    this.live = { kill };
    signal.addEventListener("abort", kill);
    const reader = p.stdout.getReader();
    return new ReadableStream<Uint8Array>({
      async pull(ctrl) {
        const { value, done } = await reader.read().catch(() => ({ value: undefined, done: true }));
        if (done || !value) return ctrl.close();
        ctrl.enqueue(value);
      },
      cancel: kill,
    });
  }

  /** 正常收尾後結束程式（錄影中會先停止並合併）。 */
  async quit(code = 0) {
    if (this.quitting) return;
    this.quitting = true;
    console.log("正在結束程式…");
    this.live?.kill();
    this.downloader.cancel();
    try {
      await Promise.all([this.recorder.shutdown(), this.exporter.shutdown()]);
      await this.tray?.dispose();
    } catch (e) {
      console.error(e);
    }
    process.exit(code);
  }
}
