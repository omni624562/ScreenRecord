/**
 * 錄影狀態機（原速錄影）。
 *
 * - 每次「開始 / 繼續」啟動一個 FFmpeg 行程寫一個分段；「暫停」對它送 q 正常結束。
 * - 「停止」送 q 收尾後，以 concat demuxer（-c copy）合併所有分段成單一 MP4。
 * - FFmpeg 意外結束（例如鎖定畫面、UAC 安全桌面造成 Desktop Duplication 中斷）時自動開新分段續錄。
 */
import type { Subprocess } from "bun";
import { existsSync, mkdirSync, rmdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { chooseEncoder, startupFallback, concatArgs, concatList, ConfigError, parseMediaInfo, resolvePlan, segmentArgs, type CapturePlan, type EncoderSpec } from "./args.ts";
import { AudioPipe, type AudioSourceSpec } from "./audiopipe.ts";
import { qpcNow100ns } from "./com.ts";
import { lastLines, run } from "./ffmpeg.ts";
import { timestamp } from "./paths.ts";
import { readLines, uniquePath } from "./util.ts";
import type {
  CaptureMethod,
  LogEntry,
  MonitorInfo,
  RecordConfig,
  RecorderState,
  RecorderStatus,
  RecordingResult,
} from "./shared/types.ts";

interface Segment {
  index: number;
  file: string;
  method: CaptureMethod;
  proc: Subprocess<"pipe", "pipe", "pipe">;
  frames: number;
  bytes: number;
  /** 目前這一輪 -progress 區塊的 key=value */
  block: Record<string, string>;
  /** 近幾秒的取樣，用來算實際 fps */
  samples: { t: number; frame: number; dup: number }[];
  actualFps?: number;
  startedAt: number;
  lastFrameAt: number;
  running: boolean;
  stopRequested: boolean;
  stderr: string;
  audio?: AudioPipe;
}

export interface RecorderDeps {
  ffmpegPath(): string | undefined;
  /** CPU 與實測可用的 GPU 編碼器 */
  encoders(): Promise<{ cpu?: EncoderSpec; gpu: EncoderSpec[] }>;
  /** 「自動」編碼是否曾偵測到 CPU 跟不上 */
  preferGpu(): boolean;
  /** 記下「這台電腦 CPU 編碼跟不上」，之後的錄影自動改用 GPU */
  learnGpu(): void;
  monitors(): MonitorInfo[];
  /** ddagrab 是否可用（濾鏡存在且實測沒失敗；上次失敗時會重新測試） */
  ddagrabUsable(): Promise<boolean>;
}

const MAX_LOG = 60;
/** 超過這麼久沒有新畫面就視為卡住 */
const STALL_MS = 15_000;
/** 送 q 後最多等這麼久 */
const STOP_TIMEOUT_MS = 10_000;
/** 錄到一半中斷時最多連續重試幾次（約 1 分鐘）；仍失敗就停止並合併已錄的部分 */
const MAX_RETRIES = 8;
/** 計時器兩次觸發的間隔超過這個值，視為電腦睡眠 / 休眠後恢復 */
const SLEEP_GAP_MS = 5_000;

export class Recorder {
  private state: RecorderState = "idle";
  private busy?: string;
  private config?: RecordConfig;
  private plan?: CapturePlan;
  private method?: CaptureMethod;
  private segments: Segment[] = [];
  private current?: Segment;
  private accumulatedMs = 0;
  private spanStart?: number;
  private startedAt?: number;
  private everProducedFrames = false;
  private partsDir?: string;
  private finalPath?: string;
  private result?: RecordingResult;
  private retry?: { attempt: number; timer?: Timer; message: string };
  private audioSpecs: AudioSourceSpec[] = [];
  private enc?: EncoderSpec;
  private cpuEncoder?: EncoderSpec;
  private gpuAvailable = false;
  private audioDesc?: string;
  private slowSince?: number;
  private slowWarned = false;
  private log: LogEntry[] = [];
  private ticker?: Timer;
  private lastTickAt = 0;
  private queue: Promise<unknown> = Promise.resolve();
  private autoStopping = false;
  shuttingDown = false;

  constructor(private deps: RecorderDeps) {}

  // ───────────── 公開操作（序列化執行，避免連點造成競態） ─────────────

  start(config: RecordConfig) {
    return this.serial(() => this.doStart(config));
  }
  pause() {
    return this.serial(() => this.doPause());
  }
  resume() {
    return this.serial(() => this.doResume());
  }
  stop(reason?: string) {
    return this.serial(() => this.doStop(reason));
  }

  get active() {
    return this.state !== "idle";
  }

  status(): RecorderStatus {
    const frames = this.segments.reduce((s, x) => s + x.frames, 0);
    const bytes = this.segments.reduce((s, x) => s + x.bytes, 0);
    const fps = this.config?.fps ?? 30;
    const cur = this.current;
    let busy = this.busy;
    if (!busy && this.state === "recording" && cur?.running && cur.frames === 0 && !this.retry) busy = "正在啟動擷取…";
    return {
      state: this.state,
      busy,
      method: this.method,
      tiles: this.method === "ddagrab" ? this.plan?.dda?.tiles.length : undefined,
      encoder: this.enc?.name,
      audio: this.audioDesc,
      segments: this.segments.filter((s) => s.frames > 0).length,
      frames,
      videoSec: frames / fps,
      bytes,
      recordedMs: this.recordedMs(),
      maxMs: (this.config?.maxMinutes ?? 0) * 60_000,
      fps,
      outWidth: this.plan?.outWidth ?? 0,
      outHeight: this.plan?.outHeight ?? 0,
      actualFps: this.state === "recording" && cur?.running ? cur.actualFps : undefined,
      slow: this.state === "recording" && this.slowSince !== undefined && Date.now() - this.slowSince > 3000,
      retrying: this.retry?.message,
      startedAt: this.startedAt,
      result: this.result,
      log: this.log,
    };
  }

  /** 程式結束前呼叫：正常收尾並合併。 */
  async shutdown() {
    this.shuttingDown = true;
    if (this.state !== "idle") await this.stop("程式結束");
  }

  // ───────────── 內部 ─────────────

  private serial<T>(fn: () => Promise<T>): Promise<T> {
    const p = this.queue.then(fn, fn);
    this.queue = p.catch(() => {});
    return p;
  }

  private addLog(level: LogEntry["level"], text: string) {
    this.log.push({ t: Date.now(), level, text });
    if (this.log.length > MAX_LOG) this.log.splice(0, this.log.length - MAX_LOG);
    const tag = level === "error" ? "錯誤" : level === "warn" ? "警告" : "訊息";
    console.log(`[${new Date().toLocaleTimeString("zh-TW", { hour12: false })}] ${tag}：${text}`);
  }

  private recordedMs() {
    return this.accumulatedMs + (this.spanStart ? Date.now() - this.spanStart : 0);
  }

  private async doStart(config: RecordConfig) {
    if (this.state !== "idle") throw new ConfigError("目前已在錄影中");
    if (!this.deps.ffmpegPath()) throw new ConfigError("找不到 ffmpeg.exe，請先依畫面指示下載");
    const plan = resolvePlan(config, this.deps.monitors());
    const encoders = await this.deps.encoders();
    const encPref = config.encoder ?? "auto";
    const chosen = chooseEncoder(encPref, plan.outWidth, plan.outHeight, config.fps, encoders.cpu, encoders.gpu, this.deps.preferGpu());
    const enc = chosen.spec;
    let method: CaptureMethod;
    if (config.method === "gdigrab") method = "gdigrab";
    else if (config.method === "ddagrab") {
      if (!plan.dda) throw new ConfigError("此範圍涵蓋不同顯示卡上的螢幕，ddagrab 無法擷取，請改用 gdigrab 或自動");
      if (!(await this.deps.ddagrabUsable())) throw new ConfigError("這台電腦無法使用 ddagrab，請改用 gdigrab 或自動");
      method = "ddagrab";
    } else method = plan.dda && (await this.deps.ddagrabUsable()) ? "ddagrab" : "gdigrab";

    const stamp = timestamp();
    const outputDir = config.outputDir.trim();
    try {
      mkdirSync(outputDir, { recursive: true });
    } catch (e) {
      throw new ConfigError(`無法建立儲存資料夾：${(e as Error).message}`);
    }
    this.partsDir = join(outputDir, ".parts", stamp);
    mkdirSync(this.partsDir, { recursive: true });
    this.finalPath = uniquePath(outputDir, `Rec_${stamp}`, ".mp4");

    this.config = { ...config, outputDir };
    this.plan = plan;
    this.method = method;
    this.enc = enc;
    this.cpuEncoder = encoders.cpu;
    this.gpuAvailable = encoders.gpu.length > 0;
    this.segments = [];
    this.current = undefined;
    this.accumulatedMs = 0;
    this.startedAt = Date.now();
    this.everProducedFrames = false;
    this.result = undefined;
    this.retry = undefined;
    this.slowSince = undefined;
    this.slowWarned = false;
    this.autoStopping = false;
    this.log = [];
    const audio = config.audio ?? { system: false, mic: false, micId: "" };
    this.audioSpecs = [
      ...(audio.system ? [{ loopback: true }] : []),
      ...(audio.mic ? [{ loopback: false, micId: String(audio.micId ?? "") }] : []),
    ];
    this.audioDesc = undefined;

    const { width, height } = plan.rect;
    const where = config.source.type === "monitor"
      ? `螢幕 ${plan.monitors[0]?.displayNumber ?? "?"}`
      : config.source.type === "all"
        ? `所有螢幕（${plan.monitors.length} 個）`
        : `範圍 (${plan.rect.x}, ${plan.rect.y})`;
    const tiles = method === "ddagrab" && plan.dda!.tiles.length > 1 ? ` ×${plan.dda!.tiles.length} 合成` : "";
    this.addLog(
      "info",
      `開始錄影：${where} ${width}×${height} → ${plan.outWidth}×${plan.outHeight}，${config.fps} fps（${method}${tiles} / ${enc.name}）`,
    );
    if (enc !== encoders.cpu) this.addLog("info", chosen.reason);
    if (method === "gdigrab" && plan.monitors.length > 1 && !plan.dda && config.method === "auto")
      this.addLog("info", "範圍涵蓋不同顯示卡上的螢幕，ddagrab 無法合成，改用 gdigrab");

    this.state = "recording";
    this.spanStart = Date.now();
    try {
      this.startSegment();
    } catch (e) {
      // 例如 ffmpeg.exe 被移除 / 防毒隔離：回到待命，不留下錄影中的假狀態
      this.state = "idle";
      this.spanStart = undefined;
      this.cleanupParts();
      throw new ConfigError(`無法啟動 FFmpeg：${(e as Error).message}`);
    }
    this.lastTickAt = Date.now();
    this.ticker = setInterval(() => this.tick(), 250);
  }

  private async doPause() {
    if (this.state !== "recording") throw new ConfigError("目前不在錄影中");
    this.closeSpan();
    this.state = "paused";
    this.busy = "正在結束目前分段…";
    try {
      await this.stopCurrent();
    } finally {
      this.busy = undefined;
    }
    this.addLog("info", "已暫停");
  }

  private async doResume() {
    if (this.state !== "paused") throw new ConfigError("目前不是暫停狀態");
    this.state = "recording";
    this.spanStart = Date.now();
    this.addLog("info", "繼續錄影");
    if (!this.tryStartSegment()) throw new ConfigError("無法繼續錄影，已停止並儲存先前錄到的部分");
  }

  /** 在計時器 / 結束事件裡啟動分段：失敗時記錄並停止（保住已錄的部分），不讓例外弄垮整個程式 */
  private tryStartSegment(): boolean {
    try {
      this.startSegment();
      return true;
    } catch (e) {
      this.addLog("error", `無法啟動 FFmpeg：${(e as Error).message}`);
      void this.stop("無法繼續擷取，停止錄影").catch(() => {});
      return false;
    }
  }

  private async doStop(reason?: string) {
    if (this.state === "idle") throw new ConfigError("目前沒有在錄影");
    this.closeSpan();
    this.state = "stopping";
    this.busy = "正在結束錄影…";
    if (reason) this.addLog("info", reason);
    try {
      await this.stopCurrent();
      this.busy = "正在合併分段…";
      this.result = await this.finalize();
      this.addLog(this.result.ok ? "info" : "error", this.result.message);
    } catch (e) {
      // 例如磁碟已滿：要讓介面與系統匣看得到失敗，分段保留下來
      this.result = {
        ok: false, frames: 0, videoSec: 0, partsDir: this.partsDir,
        message: `儲存失敗：${(e as Error).message}（已錄的分段保留於 ${this.partsDir}）`,
      };
      this.addLog("error", this.result.message);
    } finally {
      clearInterval(this.ticker);
      this.ticker = undefined;
      this.busy = undefined;
      this.state = "idle";
    }
    return this.result;
  }

  private closeSpan() {
    if (this.spanStart) this.accumulatedMs += Date.now() - this.spanStart;
    this.spanStart = undefined;
    if (this.retry?.timer) clearTimeout(this.retry.timer);
    this.retry = undefined;
    this.slowSince = undefined;
  }

  private startSegment() {
    const ffmpeg = this.deps.ffmpegPath()!;
    const enc = this.enc!;
    const index = this.segments.length;
    const file = join(this.partsDir!, `seg_${String(index).padStart(3, "0")}.mp4`);
    const method = this.method!;
    // 錄聲音：每個分段各自開一組 WASAPI 擷取與 TCP 連線，時間零點對齊該分段的第一張畫面
    const audio = this.audioSpecs.length ? new AudioPipe(this.audioSpecs, (level, text) => this.addLog(level, text)) : undefined;
    if (audio && !this.audioDesc) {
      this.audioDesc = audio.describe();
      this.addLog("info", `錄製聲音：${this.audioDesc}`);
    }
    const args = segmentArgs(this.plan!, this.config!, method, enc, file, audio?.inputArgs());

    const proc = Bun.spawn([ffmpeg, ...args], { stdin: "pipe", stdout: "pipe", stderr: "pipe", windowsHide: true });
    const now = Date.now();
    const seg: Segment = {
      index, file, method, proc,
      frames: 0, bytes: 0, block: {}, samples: [], startedAt: now, lastFrameAt: now,
      running: true, stopRequested: false, stderr: "", audio,
    };
    this.segments.push(seg);
    this.current = seg;

    void readLines(proc.stdout, (line) => this.onProgress(seg, line));
    void readLines(proc.stderr, (line) => {
      if (audio) {
        // level+info 模式：showinfo 每張畫面的時間給聲音對齊用，其餘只保留警告以上的訊息
        const pts = /Parsed_showinfo.*\bpts_time:\s*(-?[\d.]+)/.exec(line);
        if (pts) return audio.onVideoFrame(Number(pts[1]), qpcNow100ns());
        if (!/\[(warning|error|fatal|panic)\]/.test(line)) return;
        // 畫面這端失敗時，音訊輸入仍會讓 FFmpeg 卡著不結束；直接結束它，才能立即退回 gdigrab 或重試
        if (/\[(error|fatal)\]/.test(line) && /Error configuring filter graph|Could not open encoder|Error while filtering/.test(line) && !seg.stopRequested) {
          seg.stderr = (seg.stderr + line + "\n").slice(-4000);
          proc.kill();
          return;
        }
      }
      seg.stderr = (seg.stderr + line + "\n").slice(-4000);
    });
    void proc.exited.then((code) => this.onExit(seg, code));
  }

  /** -progress 每 0.5 秒輸出一組 key=value，以 progress=continue|end 結尾 */
  private onProgress(seg: Segment, line: string) {
    const eq = line.indexOf("=");
    if (eq < 0) return;
    const key = line.slice(0, eq);
    seg.block[key] = line.slice(eq + 1).trim();
    if (key !== "progress") return;
    const b = seg.block;
    seg.block = {};

    const frame = Number(b.frame);
    const size = Number(b.total_size);
    if (Number.isFinite(size) && size > 0) seg.bytes = size;
    if (!Number.isFinite(frame)) return;
    const now = Date.now();
    if (frame > seg.frames) {
      seg.frames = frame;
      seg.lastFrameAt = now;
      this.everProducedFrames = true;
      if (this.retry && this.current === seg) {
        this.addLog("info", "擷取已恢復");
        this.retry = undefined;
      }
    }
    if (frame > 0) this.checkPerformance(seg, now, frame, Number(b.dup_frames) || 0);
  }

  /**
   * 以「近 5 秒實際寫入的張數」判斷是否跟得上即時錄影。
   * 不用 FFmpeg 的 speed=：它把啟動的 1～2 秒也算進去，開頭幾十秒都會偏低。
   * dup_frames 增加代表擷取來源跟不上，CFR 只好重複前一張補位。
   */
  private checkPerformance(seg: Segment, now: number, frame: number, dup: number) {
    seg.samples.push({ t: now, frame, dup });
    while (seg.samples.length > 2 && now - seg.samples[0]!.t > 5000) seg.samples.shift();
    const first = seg.samples[0]!;
    const span = (now - first.t) / 1000;
    if (span < 3 || this.current !== seg) return;
    const written = (frame - first.frame) / span;
    const unique = written - (dup - first.dup) / span;
    seg.actualFps = Math.max(0, unique);
    const fps = this.config!.fps;
    if (written < fps * 0.9 || unique < fps * 0.9) {
      this.slowSince ??= now;
      if (!this.slowWarned && now - this.slowSince > 3000) {
        this.slowWarned = true;
        this.addLog("warn", `電腦跟不上即時錄影：實際約 ${unique.toFixed(1)} fps（設定 ${fps} fps），建議降低解析度或 FPS`);
        // 寫入速度不足代表處理鏈（下載畫面、縮放、編碼）跟不上；擷取跟不上則是 dup 增加。
        // 改用 GPU 編碼可減輕 CPU 負擔，「自動」模式記下來，之後的錄影改用 GPU 編碼
        // （同一段錄影不中途切換，不同編碼器的分段無法無損合併；可在介面上重設）
        const auto = (this.config!.encoder ?? "auto") === "auto";
        if (auto && written < fps * 0.9 && this.enc === this.cpuEncoder && this.gpuAvailable && !this.deps.preferGpu()) {
          this.deps.learnGpu();
          this.addLog("warn", "電腦處理不及，之後的錄影會自動改用 GPU 編碼以減輕 CPU 負擔（可在「更多 → 編碼器」重設）");
        }
      }
    } else this.slowSince = undefined;
  }
  private onExit(seg: Segment, code: number) {
    seg.running = false;
    if (!seg.stopRequested) seg.audio?.close();
    if (seg.stopRequested || this.shuttingDown) return;
    if (this.current !== seg || this.state !== "recording") return;

    const detail = lastLines(seg.stderr, 2) || `結束代碼 ${code}`;
    if (!this.everProducedFrames) {
      const next = startupFallback({
        stderr: seg.stderr,
        gpuEncoderInUse: !!this.cpuEncoder && this.enc !== this.cpuEncoder,
        encoderAuto: (this.config!.encoder ?? "auto") === "auto",
        hasCpuEncoder: !!this.cpuEncoder,
        ddagrabInUse: seg.method === "ddagrab",
        methodAuto: this.config!.method === "auto",
      });
      // GPU 編碼器一開始就失敗（驅動問題等）：「自動」模式退回 CPU 編碼再試
      if (next === "cpu-encoder") {
        this.addLog("warn", `GPU 編碼器 ${this.enc?.name} 無法使用（${detail}），改用 CPU 編碼`);
        this.enc = this.cpuEncoder;
        this.tryStartSegment();
        return;
      }
      if (next === "gdigrab") {
        this.addLog("warn", `ddagrab 無法擷取（${detail}），改用 gdigrab`);
        this.method = "gdigrab";
        this.tryStartSegment();
        return;
      }
      this.addLog("error", `無法開始擷取：${detail}`);
      void this.stop().catch(() => {});
      return;
    }

    // 錄到一半中斷：保留已錄分段，延遲後開新分段續錄
    const attempt = (this.retry?.attempt ?? 0) + 1;
    if (attempt > MAX_RETRIES) {
      // 長時間無法恢復（螢幕被拔掉、磁碟已滿…）：ddagrab 先換 gdigrab 再試一輪，否則停止並合併已錄的部分
      if (seg.method === "ddagrab" && this.config!.method === "auto") {
        this.addLog("warn", `ddagrab 連續 ${MAX_RETRIES} 次無法擷取（${detail}），改用 gdigrab`);
        this.method = "gdigrab";
        this.retry = { attempt: 0, message: "改用 gdigrab 重試" };
        this.tryStartSegment();
        return;
      }
      this.addLog("error", `連續 ${MAX_RETRIES} 次無法恢復擷取（${detail}），停止錄影並儲存已錄的部分`);
      this.retry = undefined;
      void this.stop().catch(() => {});
      return;
    }
    const delay = Math.min(1000 * 2 ** (attempt - 1), 10_000);
    const message = `擷取中斷，${Math.round(delay / 1000)} 秒後重試（第 ${attempt} 次）`;
    this.addLog("warn", `FFmpeg 意外結束：${detail}；${message}`);
    this.retry = {
      attempt,
      message,
      timer: setTimeout(() => {
        if (this.state === "recording" && !this.shuttingDown && !this.current?.running) {
          if (this.retry) this.retry.message = `擷取中斷，正在重試（第 ${attempt} 次）`;
          this.tryStartSegment();
        }
      }, delay),
    };
  }

  /** 對目前分段送 q 並等待結束；逾時才強制終止（fragmented MP4 確保已寫入的畫面不遺失）。 */
  private async stopCurrent() {
    const seg = this.current;
    if (!seg || !seg.running) return;
    seg.stopRequested = true;
    try {
      seg.proc.stdin.write("q");
      seg.proc.stdin.flush();
    } catch {
      // FFmpeg 可能剛好自己結束（例如收到 Ctrl+C）
    }
    // 聲音持續送到 FFmpeg 真正結束為止：收到 q 後畫面還會多處理幾百毫秒，
    // 提早送 EOF 會讓每個分段結尾少一截聲音
    const exited = await Promise.race([seg.proc.exited.then(() => true), Bun.sleep(STOP_TIMEOUT_MS).then(() => false)]);
    if (!exited) {
      this.addLog("warn", `FFmpeg 未在 ${STOP_TIMEOUT_MS / 1000} 秒內結束，已強制終止`);
      seg.proc.kill();
      await seg.proc.exited;
    }
    seg.audio?.close();
  }

  private tick() {
    const now = Date.now();
    const gap = now - this.lastTickAt;
    this.lastTickAt = now;
    if (this.state !== "recording") return;
    if (gap > SLEEP_GAP_MS && this.spanStart) {
      // 電腦睡眠後恢復：睡著的時間不算錄影長度（否則會誤觸最長錄影時間）；
      // 重開分段，避免 FFmpeg 用重複畫面補滿這段空白
      this.spanStart += gap;
      this.addLog("warn", `電腦約 ${Math.round(gap / 1000)} 秒沒有運作（睡眠 / 休眠），重新開始擷取`);
      const seg = this.current;
      if (seg?.running && !seg.stopRequested) {
        seg.lastFrameAt = now;
        seg.proc.kill();
      }
      return;
    }
    const maxMs = (this.config?.maxMinutes ?? 0) * 60_000;
    if (maxMs > 0 && this.recordedMs() >= maxMs && !this.autoStopping) {
      this.autoStopping = true;
      void this.stop("已達最長錄影時間，自動停止").catch(() => {});
      return;
    }
    const seg = this.current;
    if (seg?.running && !seg.stopRequested && Date.now() - seg.lastFrameAt > STALL_MS) {
      this.addLog("warn", `超過 ${STALL_MS / 1000} 秒沒有擷取到新畫面，重新啟動 FFmpeg`);
      seg.lastFrameAt = Date.now();
      seg.proc.kill();
    }
  }

  private async finalize(): Promise<RecordingResult> {
    const fps = this.config!.fps;
    const parts = this.segments.filter((s) => s.frames > 0 && existsSync(s.file) && statSync(s.file).size > 0);
    const frames = parts.reduce((s, x) => s + x.frames, 0);
    if (parts.length === 0) {
      this.cleanupParts();
      return { ok: false, frames: 0, videoSec: 0, message: "沒有擷取到任何畫面，未產生影片" };
    }

    const ffmpeg = this.deps.ffmpegPath()!;
    const listFile = join(this.partsDir!, "concat.txt");
    writeFileSync(listFile, concatList(parts.map((p) => p.file)), "utf8");
    const out = this.finalPath!;
    const r = await run([ffmpeg, ...concatArgs(listFile, out)], 30 * 60_000);
    if (r.code !== 0 || !existsSync(out)) {
      return {
        ok: false,
        frames,
        videoSec: frames / fps,
        partsDir: this.partsDir,
        message: `合併失敗：${lastLines(r.stderr) || `結束代碼 ${r.code}`}（分段保留於 ${this.partsDir}）`,
      };
    }

    // 只讀成品的檔頭取得實際長度（毫秒級），不再整檔重讀一遍：長時間錄影停止時省下一半的等待。
    // 分段回報的張數可能多算被強制終止前還沒寫入的幾張，以檔頭長度為準
    const head = await run([ffmpeg, "-hide_banner", "-i", out], 15_000);
    const durationSec = parseMediaInfo(head.stderr).durationSec;
    const realFrames = durationSec ? Math.round(durationSec * fps) : frames;
    this.cleanupParts();
    return {
      ok: true,
      path: out,
      frames: realFrames,
      videoSec: durationSec ?? frames / fps,
      bytes: statSync(out).size,
      message: `已儲存 ${out}（${realFrames} 張，${parts.length} 個分段）`,
    };
  }

  private cleanupParts() {
    if (!this.partsDir) return;
    try {
      rmSync(this.partsDir, { recursive: true, force: true });
      rmdirSync(dirname(this.partsDir)); // 只有空資料夾才刪得掉
    } catch {
      // .parts 內還有其他（合併失敗而保留的）分段時刪不掉，屬正常
    }
  }
}
