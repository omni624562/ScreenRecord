/**
 * 掃描儲存資料夾內的錄影：加速版歸到原始錄影底下，支援搜尋、篩選、排序、分頁。
 * 讀取影片資訊（長度、解析度、有無聲音）要執行 FFmpeg，只對需要的檔案做，並快取結果。
 */
import { readdir, stat } from "node:fs/promises";
import { basename, join } from "node:path";
import { parseMediaInfo } from "./args.ts";
import { run } from "./ffmpeg.ts";
import { parseExportName } from "./shared/format.ts";
import { LIBRARY_ROW_PX, type LibraryEntry, type LibraryPage, type LibraryQuery, type MediaInfo } from "./shared/types.ts";

/** 快取的是 Promise：同一個檔案同時被要求（搜尋、換頁、匯出完成重新整理）只會執行一次 FFmpeg */
const cache = new Map<string, { key: string; info: Promise<MediaInfo> }>();

export async function probeMedia(ffmpeg: string, path: string): Promise<MediaInfo> {
  const st = await stat(path);
  const key = `${st.size}:${st.mtimeMs}`;
  const hit = cache.get(path);
  if (hit?.key === key) return hit.info;
  const info = (async (): Promise<MediaInfo> => {
    // 沒指定輸出時 ffmpeg 會以代碼 1 結束，但 stderr 已含完整的串流資訊
    const r = await run([ffmpeg, "-hide_banner", "-i", path], 15_000);
    return { path, name: basename(path), bytes: st.size, mtime: st.mtimeMs, ...parseMediaInfo(r.stderr) };
  })();
  cache.set(path, { key, info });
  info.catch(() => cache.get(path)?.info === info && cache.delete(path)); // 失敗不快取，下次再試
  return info;
}

/** 移除已不存在的檔案的快取（每次掃描資料夾後呼叫） */
function pruneCache(dir: string, present: Set<string>) {
  const prefix = join(dir, "_").slice(0, -1).toLowerCase(); // 與 join() 產生的路徑同樣格式，結尾為分隔符
  for (const path of cache.keys()) {
    const lower = path.toLowerCase();
    if (lower.startsWith(prefix) && !lower.slice(prefix.length).includes("\\") && !present.has(lower)) cache.delete(path);
  }
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

/**
 * 掃描資料夾：原始錄影（含剪輯版）各自一筆，加速版放在對應的原檔底下。
 * 用非同步檔案 API：儲存位置在網路磁碟且回應很慢時，不會卡住錄影與狀態更新。
 */
async function scan(dir: string): Promise<LibraryEntry[]> {
  let names: string[];
  try {
    names = (await readdir(dir)).filter((n) => /\.(mp4|gif)$/i.test(n));
  } catch {
    return [];
  }
  const stats = await pool(names, 16, (name) => stat(join(dir, name)).catch(() => undefined)); // 檔案剛好被刪除或鎖住
  const files: MediaInfo[] = [];
  names.forEach((name, i) => {
    const st = stats[i];
    if (st?.isFile()) files.push({ name, path: join(dir, name), bytes: st.size, mtime: st.mtimeMs });
  });
  pruneCache(dir, new Set(files.map((f) => f.path.toLowerCase())));
  const byName = new Map(files.map((f) => [f.name.toLowerCase(), f]));
  const entries = new Map<string, LibraryEntry>();
  const derived: (MediaInfo & { speed: number; format: "mp4" | "gif"; base: string })[] = [];
  for (const f of files) {
    let e = parseExportName(f.name);
    // 原速 GIF：先找同名的 MP4（Rec_X_cut_2.gif → Rec_X_cut_2.mp4），找不到才用去掉 _N 的名稱
    if (e?.format === "gif" && e.speed === 1) {
      const exact = f.name.replace(/\.gif$/i, ".mp4");
      if (byName.has(exact.toLowerCase())) e = { ...e, base: exact };
    }
    if (e && byName.has(e.base.toLowerCase())) derived.push({ ...f, speed: e.speed, format: e.format, base: e.base.toLowerCase() });
    else if (!/\.gif$/i.test(f.name)) entries.set(f.name.toLowerCase(), { ...f, exports: [] }); // 找不到原片的 GIF 不列出
  }
  for (const d of derived) {
    const { base, ...x } = d;
    entries.get(base)?.exports.push(x);
  }
  for (const e of entries.values()) e.exports.sort((p, q) => p.speed - q.speed || (p.format === "gif" ? 1 : 0) - (q.format === "gif" ? 1 : 0) || p.mtime - q.mtime);
  return [...entries.values()];
}

/** 已讀過的資訊直接帶入，沒讀過的才執行 FFmpeg */
async function withInfo(ffmpeg: string | undefined, e: LibraryEntry): Promise<LibraryEntry> {
  if (!ffmpeg) return e;
  const probe = (m: MediaInfo) => probeMedia(ffmpeg, m.path).catch(() => m);
  const [info, exports] = await Promise.all([
    probe(e),
    Promise.all(e.exports.map(async (x) => ({ ...(await probe(x)), speed: x.speed, format: x.format }))),
  ]);
  return { ...info, exports };
}

/** 依高度分頁：回傳每頁第一筆的索引。一筆（含子列）放不下一整頁時自己一頁 */
export function pageStarts(list: LibraryEntry[], fitPx: number): number[] {
  const starts = [0];
  let used = 0;
  list.forEach((e, i) => {
    const h = LIBRARY_ROW_PX.main + e.exports.length * LIBRARY_ROW_PX.sub;
    if (used > 0 && used + h > fitPx) {
      starts.push(i);
      used = 0;
    }
    used += h;
  });
  return starts;
}

export async function listLibrary(ffmpeg: string | undefined, dir: string, q: LibraryQuery = {}): Promise<LibraryPage> {
  let list = await scan(dir);

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
  let pages: number, page: number, slice: LibraryEntry[], pageSize: number;
  const clampPage = () => Math.min(Math.max(1, Math.floor(q.page ?? 1) || 1), pages);
  if (q.fitPx !== undefined && Number.isFinite(q.fitPx)) {
    const starts = pageStarts(list, Math.max(LIBRARY_ROW_PX.main, q.fitPx));
    pages = starts.length;
    page = clampPage();
    slice = list.slice(starts[page - 1], starts[page] ?? list.length);
    pageSize = slice.length;
  } else {
    pageSize = Math.max(1, Math.min(200, Math.floor(q.pageSize ?? 30)));
    pages = Math.max(1, Math.ceil(total / pageSize));
    page = clampPage();
    slice = list.slice((page - 1) * pageSize, page * pageSize);
  }
  const items = needAll ? slice : await pool(slice, 6, (e) => withInfo(ffmpeg, e));
  return { dir, total, page, pages, pageSize, items };
}
