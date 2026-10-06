/** 剪輯計算（前後端共用）：保留哪些時間區段、裁切範圍、輸出檔名。 */
import type { Rect } from "./types.ts";

/** [開始秒, 結束秒) */
export type Range = [number, number];

export interface EditSpec {
  /** 保留的開頭（剪掉之前的部分） */
  start: number;
  /** 保留的結尾（剪掉之後的部分） */
  end: number;
  /** 中間要刪除的片段 */
  removed: Range[];
  /** 畫面裁切（影片像素座標）；undefined = 不裁切 */
  crop?: Rect;
}

/** 片段短於這個長度就忽略（約一張畫面以下） */
const MIN_RANGE = 0.02;
const ms = (t: number) => Math.round(t * 1000) / 1000;

/** 合併重疊或相鄰的區段，並限制在 [0, duration] 內 */
export function normalizeRanges(ranges: Range[], duration: number): Range[] {
  const sorted = ranges
    .map(([a, b]): Range => [Math.max(0, Math.min(a, b)), Math.min(duration, Math.max(a, b))])
    .filter(([a, b]) => b - a >= MIN_RANGE)
    .sort((p, q) => p[0] - q[0]);
  const out: Range[] = [];
  for (const r of sorted) {
    const last = out[out.length - 1];
    if (last && r[0] <= last[1]) last[1] = Math.max(last[1], r[1]);
    else out.push([r[0], r[1]]);
  }
  return out.map(([a, b]) => [ms(a), ms(b)]);
}

/** 實際保留的區段：[start, end] 扣掉 removed */
export function keepRanges(duration: number, spec: EditSpec): Range[] {
  const start = Math.max(0, Math.min(spec.start, duration));
  const end = Math.max(start, Math.min(spec.end, duration));
  let keep: Range[] = [[start, end]];
  for (const [a, b] of normalizeRanges(spec.removed, duration)) {
    keep = keep.flatMap(([s, e]): Range[] => {
      if (b <= s || a >= e) return [[s, e]];
      const parts: Range[] = [];
      if (a > s) parts.push([s, a]);
      if (b < e) parts.push([b, e]);
      return parts;
    });
  }
  return keep.filter(([a, b]) => b - a >= MIN_RANGE).map(([a, b]) => [ms(a), ms(b)]);
}

export const totalLength = (ranges: Range[]) => ranges.reduce((s, [a, b]) => s + (b - a), 0);

/** 裁切範圍：限制在畫面內、寬高取偶數（yuv420p 需要），至少 16px */
export function normalizeCrop(crop: Rect | undefined, width: number, height: number): Rect | undefined {
  if (!crop) return undefined;
  const x = Math.max(0, Math.min(Math.round(crop.x), width - 16));
  const y = Math.max(0, Math.min(Math.round(crop.y), height - 16));
  const w = Math.floor(Math.max(16, Math.min(Math.round(crop.width), width - x)) / 2) * 2;
  const h = Math.floor(Math.max(16, Math.min(Math.round(crop.height), height - y)) / 2) * 2;
  if (x === 0 && y === 0 && w >= width - 1 && h >= height - 1) return undefined; // 等於整個畫面
  return { x, y, width: w, height: h };
}

/** Rec_xxx.mp4 → Rec_xxx_cut.mp4 */
export function cutFileName(sourceName: string): string {
  return sourceName.replace(/\.mp4$/i, "") + "_cut.mp4";
}
