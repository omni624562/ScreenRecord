/**
 * 轉檔工作：加速匯出、剪輯（同一時間只跑一個，避免搶 CPU）。
 * 都是從原檔重新編碼成新檔，原檔不變。
 */
import type { Subprocess } from "bun";
import { existsSync, rmSync, statSync } from "node:fs";
import { basename, dirname } from "node:path";
import { ConfigError, cutArgs, exportArgs, gifArgs, type EncoderSpec } from "./args.ts";
import { lastLines } from "./ffmpeg.ts";
import { probeMedia } from "./library.ts";
import { readLines, uniquePath } from "./util.ts";
import { cutFileName, keepRanges, normalizeCrop, totalLength, type EditSpec } from "./shared/edit.ts";
import { exportFileName, LIMITS, speedLabel } from "./shared/format.ts";
import type { ExportStatus, MediaInfo } from "./shared/types.ts";

interface Job extends ExportStatus {
  proc?: Subprocess<"ignore", "pipe", "pipe">;
  startedAt: number;
  endedAt?: number;
  stderr: string;
  canceled: boolean;
}

export interface ExporterDeps {
  ffmpegPath(): string | undefined;
  encoder(): EncoderSpec | undefined;
}

const KIND_TEXT = { speed: "加速版", gif: "GIF", cut: "剪輯" } as const;

export class Exporter {
  private job?: Job;
  private nextId = 1;

  constructor(private deps: ExporterDeps) {}

  /** 準備中（讀取影片資訊）也算進行中：避免準備的空檔又開始錄影或第二個轉檔 */
  private starting = false;

  get running() {
    return this.starting || this.job?.state === "running";
  }

  /** 檢查與開始之間有 await，用旗標讓整段成為一個不可重入的動作 */
  private async exclusive(fn: () => Promise<ExportStatus>): Promise<ExportStatus> {
    if (this.running) throw new ConfigError("已有轉檔工作進行中，請等它完成");
    this.starting = true;
    try {
      return await fn();
    } finally {
      this.starting = false;
    }
  }

  status(): ExportStatus | undefined {
    const j = this.job;
    if (!j) return undefined;
    const elapsedMs = (j.endedAt ?? Date.now()) - j.startedAt;
    const etaSec = j.state === "running" && j.progress > 0.02
      ? (elapsedMs / 1000) * (1 - j.progress) / j.progress
      : undefined;
    const { proc: _p, stderr: _s, canceled: _c, startedAt: _t, endedAt: _e, ...pub } = j;
    return { ...pub, elapsedMs, etaSec };
  }

  /** 加速匯出 */
  start(source: string, speed: number, keepAudio = true): Promise<ExportStatus> {
    return this.exclusive(() => this.doStart(source, speed, keepAudio));
  }

  /** 匯出 GIF（可同時加速；無聲音） */
  startGif(source: string, speed: number, opts: { width: number; fps: number }): Promise<ExportStatus> {
    return this.exclusive(() => this.doGif(source, speed, opts));
  }

  /** 剪輯：剪頭尾、刪除中間片段、裁切畫面，另存為 *_cut.mp4 */
  startCut(source: string, spec: EditSpec): Promise<ExportStatus> {
    return this.exclusive(() => this.doCut(source, spec));
  }

  private async doStart(source: string, speed: number, keepAudio: boolean): Promise<ExportStatus> {
    if (!Number.isFinite(speed) || speed < LIMITS.speedMin || speed > LIMITS.speedMax)
      throw new ConfigError(`倍率需介於 ${LIMITS.speedMin}～${LIMITS.speedMax}`);
    const { ffmpeg, enc, info, fps } = await this.prepare(source);
    const output = uniquePath(dirname(source), exportFileName(basename(source), speed).replace(/\.mp4$/i, ""), ".mp4");
    const withAudio = keepAudio && !!info.hasAudio;
    const args = exportArgs(source, output, speed, fps, enc, withAudio);
    const note = `${speedLabel(speed)}×${withAudio ? "，含聲音" : ""}`;
    return this.run("speed", ffmpeg, args, source, output, speed, info.durationSec! / speed, note);
  }

  private async doGif(source: string, speed: number, opts: { width: number; fps: number }): Promise<ExportStatus> {
    if (!Number.isFinite(speed) || speed < 1 || speed > LIMITS.speedMax) throw new ConfigError(`倍率需介於 1～${LIMITS.speedMax}`);
    const fps = Math.min(30, Math.max(5, Math.round(opts.fps) || 10));
    const width = Math.min(1920, Math.max(160, Math.round(opts.width) || 640));
    const { ffmpeg, info } = await this.prepare(source);
    const outputSec = info.durationSec! / speed;
    const name = exportFileName(basename(source), speed, "gif").replace(/\.gif$/i, "");
    const output = uniquePath(dirname(source), name, ".gif");
    const args = gifArgs(source, output, speed, { fps, width, srcWidth: info.width ?? 0, srcHeight: info.height ?? 0, outputSec });
    const note = `${speed > 1 ? `${speedLabel(speed)}×，` : ""}寬 ${Math.min(width, info.width ?? width)}、${fps} fps`;
    return this.run("gif", ffmpeg, args, source, output, speed, outputSec, note);
  }

  private async doCut(source: string, spec: EditSpec): Promise<ExportStatus> {
    const { ffmpeg, enc, info, fps } = await this.prepare(source);
    const keep = keepRanges(info.durationSec!, spec);
    const length = totalLength(keep);
    if (length < 0.1) throw new ConfigError("剪輯後留下的長度太短");
    let crop = spec.crop;
    if (crop) {
      if (!info.width || !info.height) throw new ConfigError("無法讀取影片尺寸，不能裁切畫面");
      crop = normalizeCrop(crop, info.width, info.height);
    }
    const unchanged = keep.length === 1 && keep[0]![0] === 0 && keep[0]![1] >= info.durationSec! - 0.05 && !crop;
    if (unchanged) throw new ConfigError("沒有任何剪輯或裁切");

    const output = uniquePath(dirname(source), cutFileName(basename(source)).replace(/\.mp4$/i, ""), ".mp4");
    const args = cutArgs(source, output, keep, crop, fps, enc, !!info.hasAudio);
    const note = `保留 ${keep.length} 段${crop ? `，裁切 ${crop.width}×${crop.height}` : ""}`;
    return this.run("cut", ffmpeg, args, source, output, 1, length, note);
  }

  cancel() {
    const j = this.job;
    if (j?.state !== "running" || !j.proc) throw new ConfigError("目前沒有進行中的轉檔");
    j.canceled = true;
    j.proc.kill();
  }

  /** 程式結束時：中止轉檔並刪除未完成的檔案 */
  async shutdown() {
    const j = this.job;
    if (j?.state === "running" && j.proc) {
      j.canceled = true;
      j.proc.kill();
      await j.proc.exited;
      rmSync(j.output, { force: true });
    }
  }

  // ───────────── 內部 ─────────────

  private async prepare(source: string): Promise<{ ffmpeg: string; enc: EncoderSpec; info: MediaInfo; fps: number }> {
    const ffmpeg = this.deps.ffmpegPath();
    const enc = this.deps.encoder();
    if (!ffmpeg || !enc) throw new ConfigError("FFmpeg 無法使用");
    if (!source.toLowerCase().endsWith(".mp4") || !existsSync(source) || !statSync(source).isFile())
      throw new ConfigError("找不到影片檔");
    const info = await probeMedia(ffmpeg, source);
    if (!info.durationSec) throw new ConfigError("無法讀取影片長度，檔案可能已損壞");
    const fps = Math.min(LIMITS.fpsMax, Math.max(1, Math.round(info.fps ?? 30)));
    return { ffmpeg, enc, info, fps };
  }

  private run(
    kind: Job["kind"],
    ffmpeg: string,
    args: string[],
    source: string,
    output: string,
    speed: number,
    expectedSec: number,
    note: string,
  ): ExportStatus {
    const proc = Bun.spawn([ffmpeg, ...args], { stdin: "ignore", stdout: "pipe", stderr: "pipe", windowsHide: true });
    const job: Job = {
      id: this.nextId++,
      kind,
      state: "running",
      source,
      output,
      speed,
      progress: 0,
      expectedSec,
      elapsedMs: 0,
      proc,
      startedAt: Date.now(),
      stderr: "",
      canceled: false,
    };
    this.job = job;
    const label = KIND_TEXT[kind];
    console.log(`[${label}] ${basename(source)} → ${basename(output)}（${note}）`);

    void readLines(proc.stdout, (line) => {
      const eq = line.indexOf("=");
      if (eq < 0) return;
      const key = line.slice(0, eq);
      const value = Number(line.slice(eq + 1));
      if (key === "out_time_us" && Number.isFinite(value) && value > 0)
        job.progress = Math.min(0.999, value / 1e6 / expectedSec);
      else if (key === "total_size" && Number.isFinite(value) && value > 0) job.bytes = value;
    });
    void readLines(proc.stderr, (line) => {
      job.stderr = (job.stderr + line + "\n").slice(-4000);
    });
    void proc.exited.then((code) => {
      job.endedAt = Date.now();
      job.proc = undefined;
      if (job.canceled) {
        job.state = "canceled";
        job.message = `已取消${label}`;
        rmSync(output, { force: true });
      } else if (code === 0 && existsSync(output)) {
        job.state = "done";
        job.progress = 1;
        try {
          job.bytes = statSync(output).size;
        } catch {
          // 檔案剛好被移走：大小未知不影響結果
        }
        job.message = `已儲存 ${output}`;
      } else {
        job.state = "error";
        job.message = `${label}失敗：${lastLines(job.stderr) || `結束代碼 ${code}`}`;
        rmSync(output, { force: true });
      }
      console.log(`[${label}] ${job.message}`);
    });
    return this.status()!;
  }
}
