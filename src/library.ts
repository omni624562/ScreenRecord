/**
 * 掃描儲存資料夾內的錄影：加速版歸到原始錄影底下，支援搜尋、篩選、排序、分頁。
 * 讀取影片資訊（長度、解析度、有無聲音）要執行 FFmpeg，只對需要的檔案做，並快取結果。
 */
import { readdirSync, statSync } from "node:fs";
import { basename, join } from "node:path";
import { parseMediaInfo } from "./args.ts";
import { run } from "./ffmpeg.ts";
import { parseExportName } from "./shared/format.ts";
import type { LibraryEntry, LibraryPage, LibraryQuery, MediaInfo } from "./shared/types.ts";

const cache = new Map<string, { key: string; info: MediaInfo }>();

export async function probeMedia(ffmpeg: string, path: string): Promise<MediaInfo> {
  const st = statSync(path);
  const key = `${st.size}:${st.mtimeMs}`;
  const hit = cache.get(path);
  if (hit?.key === key) return hit.info;
  // 沒指定輸出時 ffmpeg 會以代碼 1 結束，但 stderr 已含完整的串流資訊
  const r = await run([ffmpeg, "-hide_banner", "-i", path], 15_000);
  const info: MediaInfo = { path, name: basename(path), bytes: st.size, mtime: st.mtimeMs, ...parseMediaInfo(r.stderr) };
  cache.set(path, { key, info });
  return info;
}

/** 同時最多 limit 個工作 */
async function pool<T, R>(items: T[], limit: number, fn: (x: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(limit, items.length) }, async () => {
      while (next < items.length) {
        const i = next++;
        out[i] = await fn(items[i]!);
      }
    }),
  );
  return out;
}

export const isCutName = (name: string) => /_cut(_\d+)?\.mp4$/i.test(name);

/** 掃描資料夾：原始錄影（含剪輯版）各自一筆，加速版放在對應的原檔底下 */
function scan(dir: string): LibraryEntry[] {
  let names: string[];
  try {
    names = readdirSync(dir).filter((n) => n.toLowerCase().endsWith(".mp4"));
  } catch {
    return [];
  }
  const files: MediaInfo[] = [];
  for (const name of names) {
    try {
      const st = statSync(join(dir, name));
      if (st.isFile()) files.push({ name, path: join(dir, name), bytes: st.size, mtime: st.mtimeMs });
    } catch {
      // 檔案剛好被刪除或鎖住
    }
  }
  const byName = new Map(files.map((f) => [f.name.toLowerCase(), f]));
  const entries = new Map<string, LibraryEntry>();
  const derived: (MediaInfo & { speed: number; base: string })[] = [];
  for (const f of files) {
    const e = parseExportName(f.name);
    if (e && byName.has(e.base.toLowerCase())) derived.push({ ...f, speed: e.speed, base: e.base.toLowerCase() });
    else entries.set(f.name.toLowerCase(), { ...f, exports: [] });
  }
  for (const d of derived) {
    const { base, ...x } = d;
    entries.get(base)?.exports.push(x);
  }
  for (const e of entries.values()) e.exports.sort((p, q) => p.speed - q.speed || p.mtime - q.mtime);
  return [...entries.values()];
}

/** 已讀過的資訊直接帶入，沒讀過的才執行 FFmpeg */
async function withInfo(ffmpeg: string | undefined, e: LibraryEntry): Promise<LibraryEntry> {
  if (!ffmpeg) return e;
  const probe = (m: MediaInfo) => probeMedia(ffmpeg, m.path).catch(() => m);
  const [info, exports] = await Promise.all([
    probe(e),
    Promise.all(e.exports.map(async (x) => ({ ...(await probe(x)), speed: x.speed }))),
  ]);
  return { ...info, exports };
}

export async function listLibrary(ffmpeg: string | undefined, dir: string, q: LibraryQuery = {}): Promise<LibraryPage> {
  let list = scan(dir);

  const text = q.q?.trim().toLowerCase();
  if (text) {
    list = list.filter((e) => {
      const d = new Date(e.mtime);
      const date = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
      return e.name.toLowerCase().includes(text) || date.includes(text) || e.exports.some((x) => x.name.toLowerCase().includes(text));
    });
  }
  if (q.filter === "original") list = list.filter((e) => !isCutName(e.name));
  else if (q.filter === "cut") list = list.filter((e) => isCutName(e.name));
  else if (q.filter === "speed") list = list.filter((e) => e.exports.length > 0);

  // 依聲音篩選、依長度排序需要每個檔案的資訊（有快取，第一次較慢）
  const needAll = q.filter === "audio" || q.sort === "duration";
  if (needAll && ffmpeg) list = await pool(list, 6, (e) => withInfo(ffmpeg, e));
  if (q.filter === "audio") list = list.filter((e) => e.hasAudio);

  const sorters: Record<string, (a: LibraryEntry, b: LibraryEntry) => number> = {
    new: (a, b) => b.mtime - a.mtime,
    old: (a, b) => a.mtime - b.mtime,
    size: (a, b) => b.bytes - a.bytes,
    duration: (a, b) => (b.durationSec ?? 0) - (a.durationSec ?? 0),
  };
  list.sort(sorters[q.sort ?? "new"] ?? sorters.new!);

  const total = list.length;
  const pageSize = Math.max(1, Math.min(200, Math.floor(q.pageSize ?? 30)));
  const pages = Math.max(1, Math.ceil(total / pageSize));
  const page = Math.min(Math.max(1, Math.floor(q.page ?? 1)), pages);
  const slice = list.slice((page - 1) * pageSize, page * pageSize);
  const items = needAll ? slice : await pool(slice, 6, (e) => withInfo(ffmpeg, e));
  return { dir, total, page, pages, pageSize, items };
}
