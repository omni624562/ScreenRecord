/**
 * 自動下載 FFmpeg（使用者按下按鈕才執行）。
 *
 * - 鎖定版本：固定下載 FFmpeg 9.0.2 essentials（gyan.dev），本程式已用這個版本測試過。
 * - SHA-256 內建在程式裡：不是從下載網站取得，即使下載來源被入侵或檔案被調包也會被擋下。
 * - 主來源失敗時改用 GitHub 上的同一個檔案（同樣比對內建的 SHA-256）。
 * - 用 Windows 內建的 tar（System32，不受 PATH 上 Git / MSYS2 的 GNU tar 影響）解出 bin\ffmpeg.exe，
 *   確認能執行後才放到定位：exe 所在資料夾可寫入就放那裡，否則放 %LOCALAPPDATA%\ScreenRecorder。
 */
import { existsSync, mkdirSync, renameSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { run as runCommand, type RunResult } from "./ffmpeg.ts";
import { logDir } from "./log.ts";
import { appDir } from "./paths.ts";
import type { DownloadStatus } from "./shared/types.ts";

export const FFMPEG_VERSION = "9.0.2";
export const FFMPEG_SHA256 = "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba";
export const FFMPEG_URLS = [
  `https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-${FFMPEG_VERSION}-essentials_build.zip`,
  `https://github.com/GyanD/codexffmpeg/releases/download/${FFMPEG_VERSION}/ffmpeg-${FFMPEG_VERSION}-essentials_build.zip`,
];

/** Windows 內建 tar（bsdtar，支援 zip） */
export const SYSTEM_TAR = join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe");

/** 可以寫入的放置位置：優先 exe 旁邊 */
export function downloadTarget(): string {
  for (const dir of [appDir, logDir]) {
    try {
      mkdirSync(dir, { recursive: true });
      const probe = join(dir, `.write-test-${process.pid}`);
      writeFileSync(probe, "");
      unlinkSync(probe);
      return join(dir, "ffmpeg.exe");
    } catch {
      // 沒有寫入權限（例如 Program Files），換下一個
    }
  }
  throw new Error("找不到可寫入的資料夾");
}

export interface DownloaderDeps {
  fetch: (url: string, init: { signal: AbortSignal }) => Promise<Response>;
  run: (cmd: string[], timeoutMs?: number) => Promise<RunResult>;
  target: () => string;
  /** 安裝完成後（例如重新偵測 FFmpeg） */
  onDone: () => Promise<void>;
  urls?: string[];
  sha256?: string;
  tar?: string;
}

class CanceledError extends Error {}

export class FfmpegDownloader {
  private st: DownloadStatus = { phase: "idle", received: 0 };
  private abort?: AbortController;
  private startedAt = 0;
  /** 測試用：目前工作的 Promise */
  job?: Promise<void>;
  private deps: Required<Omit<DownloaderDeps, "onDone">> & Pick<DownloaderDeps, "onDone">;

  constructor(deps: Partial<DownloaderDeps> & Pick<DownloaderDeps, "onDone">) {
    this.deps = {
      fetch: (url, init) => fetch(url, init),
      run: runCommand,
      target: downloadTarget,
      urls: FFMPEG_URLS,
      sha256: FFMPEG_SHA256,
      tar: SYSTEM_TAR,
      ...deps,
    };
  }

  status(): DownloadStatus {
    return { ...this.st };
  }

  get busy() {
    return ["downloading", "verifying", "extracting"].includes(this.st.phase);
  }

  start() {
    if (this.busy) return;
    this.abort = new AbortController();
    this.startedAt = Date.now();
    this.st = { phase: "downloading", received: 0 };
    this.job = this.install(this.abort.signal).then(
      async () => {
        // 安裝已完成；重新偵測失敗也不影響「已安裝」的結果
        try {
          await this.deps.onDone();
        } catch (e) {
          console.error(`[下載 FFmpeg] 安裝完成，但重新偵測失敗：${(e as Error).message}`);
        }
      },
      (e: Error) => {
        if (e instanceof CanceledError || this.st.phase === "canceled") return;
        this.st = { ...this.st, phase: "error", message: e.message };
        console.error(`[下載 FFmpeg] 失敗：${e.message}`);
      },
    );
  }

  cancel() {
    if (!this.busy) return;
    this.st = { ...this.st, phase: "canceled", message: "已取消下載" };
    this.abort?.abort();
  }

  private async install(signal: AbortSignal) {
    const target = this.deps.target();
    const work = join(tmpdir(), `ScreenRecorder-ffmpeg-${Date.now()}-${process.pid}`);
    mkdirSync(work, { recursive: true });
    const zip = join(work, "ffmpeg.zip");
    this.st.target = target;
    try {
      // 1. 下載並比對內建的 SHA-256；主來源失敗就換下一個
      let lastError: Error | undefined;
      let ok = false;
      for (const url of this.deps.urls) {
        try {
          console.log(`[下載 FFmpeg] ${url} → ${target}`);
          await this.downloadTo(url, zip, signal);
          ok = true;
          break;
        } catch (e) {
          if (signal.aborted) throw new CanceledError();
          lastError = e as Error;
          console.error(`[下載 FFmpeg] ${url} 失敗：${lastError.message}`);
        }
      }
      if (!ok) throw lastError ?? new Error("下載失敗");

      // 2. 解壓縮：只取 bin\ffmpeg.exe
      this.st.phase = "extracting";
      const { run, tar } = this.deps;
      const list = await run([tar, "-tf", zip], 60_000);
      if (list.code !== 0) throw new Error(`無法讀取壓縮檔：${list.stderr.trim()}`);
      const entry = list.stdout.split(/\r?\n/).map((l) => l.trim()).find((l) => /(^|\/)bin\/ffmpeg\.exe$/i.test(l));
      if (!entry) throw new Error("壓縮檔裡找不到 bin/ffmpeg.exe");
      const ex = await run([tar, "-xf", zip, "-C", work, entry], 120_000);
      const extracted = join(work, ...entry.split("/"));
      if (ex.code !== 0 || !existsSync(extracted)) throw new Error(`解壓縮失敗：${ex.stderr.trim()}`);
      if (signal.aborted) throw new CanceledError();

      // 3. 確認能執行再放到定位（先放暫存名稱再改名，避免留下半個檔案）
      const ver = await run([extracted, "-hide_banner", "-version"], 15_000);
      if (ver.code !== 0) throw new Error("下載的 ffmpeg.exe 無法執行");
      mkdirSync(dirname(target), { recursive: true });
      const staging = `${target}.download`;
      await Bun.write(staging, Bun.file(extracted));
      rmSync(target, { force: true });
      renameSync(staging, target);
      this.st.version = /ffmpeg version (\S+)/.exec(ver.stdout)?.[1];
      this.st.phase = "done";
      this.st.message = `已安裝 FFmpeg ${this.st.version ?? ""}：${target}`;
      console.log(`[下載 FFmpeg] ${this.st.message}`);
    } finally {
      rmSync(work, { recursive: true, force: true });
    }
  }

  /** 下載單一來源到 zip（邊下載邊計算雜湊與進度），並比對內建的 SHA-256 */
  private async downloadTo(url: string, zip: string, signal: AbortSignal) {
    this.st.phase = "downloading";
    this.st.received = 0;
    this.st.total = undefined;
    this.startedAt = Date.now();
    const res = await this.deps.fetch(url, { signal });
    if (!res.ok || !res.body) throw new Error(`下載失敗（HTTP ${res.status}）`);
    this.st.total = Number(res.headers.get("content-length")) || undefined;
    const hasher = new Bun.CryptoHasher("sha256");
    const sink = Bun.file(zip).writer();
    try {
      for await (const chunk of res.body) {
        hasher.update(chunk);
        sink.write(chunk);
        this.st.received += chunk.length;
        const sec = (Date.now() - this.startedAt) / 1000;
        this.st.speed = sec > 0.5 ? this.st.received / sec : undefined;
      }
    } finally {
      await sink.end();
    }
    if (signal.aborted) throw new CanceledError();
    this.st.phase = "verifying";
    if (hasher.digest("hex") !== this.deps.sha256.toLowerCase())
      throw new Error("檔案的 SHA-256 與預期不符（下載不完整或檔案遭替換），已刪除");
  }
}
