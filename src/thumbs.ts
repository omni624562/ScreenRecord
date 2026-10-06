/**
 * 錄影清單的縮圖：用 FFmpeg 擷取一張畫面（320px 寬 JPEG），存在 %LOCALAPPDATA%\ScreenRecorder\thumbs。
 * 以「路徑 + 大小 + 修改時間」為鍵：檔案被覆寫（剪輯另存不會）時自動重做；同時最多產生 2 張。
 */
import { existsSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { stat } from "node:fs/promises";
import { join } from "node:path";
import { run } from "./ffmpeg.ts";
import { logDir } from "./log.ts";

const dir = join(logDir, "thumbs");
const MAX_FILES = 2000;
const CONCURRENCY = 2;

const inflight = new Map<string, Promise<string | undefined>>();
let running = 0;
const waiting: (() => void)[] = [];

async function slot<T>(fn: () => Promise<T>): Promise<T> {
  if (running >= CONCURRENCY) await new Promise<void>((r) => waiting.push(r));
  running++;
  try {
    return await fn();
  } finally {
    running--;
    waiting.shift()?.();
  }
}

/** 取得影片縮圖的檔案路徑；產生失敗回傳 undefined */
export async function thumbnail(ffmpeg: string, video: string): Promise<string | undefined> {
  const st = await stat(video);
  const key = new Bun.CryptoHasher("sha1").update(`${video.toLowerCase()}|${st.size}|${st.mtimeMs}`).digest("hex");
  const out = join(dir, `${key}.jpg`);
  if (existsSync(out)) return out;
  let job = inflight.get(key);
  if (!job) {
    job = slot(async () => {
      mkdirSync(dir, { recursive: true });
      // 先取第 1 秒（避開開頭可能的黑畫面），太短的影片改取第一張
      for (const at of ["1", "0"]) {
        const r = await run(
          [ffmpeg, "-hide_banner", "-loglevel", "error", "-ss", at, "-i", video, "-frames:v", "1", "-vf", "scale=320:-2:flags=bilinear", "-q:v", "5", "-y", out],
          20_000,
        );
        if (r.code === 0 && existsSync(out) && statSync(out).size > 0) return out;
      }
      rmSync(out, { force: true });
      return undefined;
    }).finally(() => inflight.delete(key));
    inflight.set(key, job);
  }
  return job;
}

/** 啟動時整理：超過上限就刪掉最舊的縮圖 */
export function pruneThumbnails() {
  try {
    const files = readdirSync(dir)
      .filter((n) => n.endsWith(".jpg"))
      .map((n) => ({ p: join(dir, n), t: statSync(join(dir, n)).mtimeMs }));
    if (files.length <= MAX_FILES) return;
    files.sort((a, b) => a.t - b.t);
    for (const f of files.slice(0, files.length - MAX_FILES)) rmSync(f.p, { force: true });
  } catch {
    // 資料夾還不存在
  }
}
