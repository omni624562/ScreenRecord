/** 前後端共用的計算與格式化（確保介面顯示與實際 FFmpeg 參數一致）。 */

export const LIMITS = {
  fpsMin: 1,
  fpsMax: 60,
  speedMin: 1.1,
  speedMax: 1000,
  maxMinutesMax: 7 * 24 * 60,
} as const;

export const FPS_PRESETS = [15, 24, 30, 60] as const;
export const SPEED_PRESETS = [1.5, 2, 4, 8, 16] as const;

const even = (n: number) => Math.max(2, Math.floor(n / 2) * 2);

/** 輸出尺寸：依縮放比例計算並取偶數（yuv420p 要求寬高為偶數） */
export function outputSize(width: number, height: number, scale: number) {
  return { width: even(Math.round((width * scale) / 100)), height: even(Math.round((height * scale) / 100)) };
}

/** 倍率顯示：4 → "4"、1.5 → "1.5"、2.25 → "2.25" */
export function speedLabel(speed: number): string {
  return String(Math.round(speed * 100) / 100);
}

/** 加速版檔名：Rec_xxx.mp4 → Rec_xxx_4x.mp4 */
export function exportFileName(sourceName: string, speed: number): string {
  return sourceName.replace(/\.mp4$/i, "") + `_${speedLabel(speed)}x.mp4`;
}

/** 從加速版檔名解析倍率；不是加速版回傳 undefined */
export function parseExportName(name: string): { base: string; speed: number } | undefined {
  const m = /^(.*)_(\d+(?:\.\d+)?)x(?:_\d+)?\.mp4$/i.exec(name);
  return m ? { base: `${m[1]}.mp4`, speed: Number(m[2]) } : undefined;
}

/** 秒數 → 「1 小時 2 分 3 秒」 */
export function humanDuration(sec: number): string {
  if (!Number.isFinite(sec)) return "—";
  if (sec < 10) return `${Math.round(sec * 10) / 10} 秒`;
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const parts: string[] = [];
  if (h) parts.push(`${h} 小時`);
  if (m) parts.push(`${m} 分`);
  if (r || parts.length === 0) parts.push(`${r} 秒`);
  return parts.join(" ");
}

/** 毫秒 → HH:MM:SS */
export function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return [h, m, r].map((v) => String(v).padStart(2, "0")).join(":");
}

/** 秒 → MM:SS.s（超過 1 小時加上小時） */
export function videoClock(sec: number): string {
  const tenth = Math.max(0, Math.floor(sec * 10));
  const h = Math.floor(tenth / 36000);
  const m = Math.floor((tenth % 36000) / 600);
  const s = Math.floor((tenth % 600) / 10);
  const t = tenth % 10;
  const mmss = `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${t}`;
  return h ? `${h}:${mmss}` : mmss;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 2 : 1)} ${units[i]}`;
}
