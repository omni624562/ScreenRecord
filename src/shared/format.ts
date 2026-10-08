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

export const even = (n: number) => Math.max(2, Math.floor(n / 2) * 2);

/** 輸出尺寸：依縮放比例計算並取偶數（yuv420p 要求寬高為偶數） */
export function outputSize(width: number, height: number, scale: number) {
  return { width: even(Math.round((width * scale) / 100)), height: even(Math.round((height * scale) / 100)) };
}

/** 倍率顯示：4 → "4"、1.5 → "1.5"、2.25 → "2.25" */
export function speedLabel(speed: number): string {
  return String(Math.round(speed * 100) / 100);
}

export type ExportFormat = "mp4" | "gif";

/** 匯出檔名：Rec_xxx.mp4 → Rec_xxx_4x.mp4 / Rec_xxx_4x.gif；原速 GIF 為 Rec_xxx.gif */
export function exportFileName(sourceName: string, speed: number, format: ExportFormat = "mp4"): string {
  const base = sourceName.replace(/\.mp4$/i, "");
  return format === "gif" && speed <= 1 ? `${base}.gif` : `${base}_${speedLabel(speed)}x.${format}`;
}

/** 從匯出檔名解析倍率與格式；不是匯出檔回傳 undefined（原速 GIF 也算，倍率為 1） */
export function parseExportName(name: string): { base: string; speed: number; format: ExportFormat } | undefined {
  const m = /^(.*)_(\d+(?:\.\d+)?)x(?:_\d+)?\.(mp4|gif)$/i.exec(name);
  if (m) return { base: `${m[1]}.mp4`, speed: Number(m[2]), format: m[3]!.toLowerCase() as ExportFormat };
  const g = /^(.*?)(?:_\d+)?\.gif$/i.exec(name);
  return g ? { base: `${g[1]}.mp4`, speed: 1, format: "gif" } : undefined;
}

/** 加速版可選的寬度（0 = 原尺寸）；比原片寬的選項不顯示 */
export const MP4_WIDTHS = [0, 1920, 1280] as const;

/** 縮小後的尺寸（高度等比、取偶數）；width 為 0 或不小於原寬時維持原尺寸 */
export function scaledSize(srcWidth: number, srcHeight: number, width: number) {
  if (!width || width >= srcWidth) return { width: srcWidth, height: srcHeight };
  return { width: even(width), height: even(Math.round((srcHeight * width) / srcWidth)) };
}

export interface EstimateInput {
  format: ExportFormat;
  /** 原片大小與長度 */
  srcBytes: number;
  srcSec: number;
  srcWidth: number;
  srcHeight: number;
  speed: number;
  /** 輸出尺寸 */
  width: number;
  height: number;
  /** GIF 每秒張數 */
  gifFps?: number;
}

/**
 * 粗估成品大小，回傳 [下限, 上限]（位元組）。畫面內容影響很大，只求數量級正確：
 * - MP4：以原片的位元率為準；加速後每張畫面差異變大，位元率約隨倍率的平方根增加（最多 4 倍），尺寸縮小時大致依面積遞減
 * - GIF：依每張畫面的像素數估算；只更新變動區域時，螢幕畫面大多每像素 0.03～0.2 位元組
 */
export function estimateBytes(o: EstimateInput): [number, number] {
  const outSec = o.srcSec / o.speed;
  if (o.format === "gif") {
    const frames = Math.max(1, outSec * (o.gifFps ?? 10));
    const px = o.width * o.height * frames;
    return [px * 0.03, px * 0.2];
  }
  const area = o.srcWidth && o.srcHeight ? (o.width * o.height) / (o.srcWidth * o.srcHeight) : 1;
  const mid = (o.srcBytes / o.srcSec) * outSec * Math.min(4, Math.sqrt(o.speed)) * area ** 0.75;
  return [mid * 0.5, mid * 1.8];
}

/** 錄影改名時的名稱檢查（不含 .mp4）；沒問題回傳 undefined */
export function checkRecordingName(base: string): string | undefined {
  const b = base.trim();
  if (!b) return "請輸入名稱";
  if (/[\\/:*?"<>|\x00-\x1f]/.test(b)) return '名稱不能包含 \\ / : * ? " < > |';
  if (/[. ]$/.test(b)) return "名稱結尾不能是句點或空白";
  if (/^(con|prn|aux|nul|com\d|lpt\d)$/i.test(b)) return "這是 Windows 保留的名稱，請換一個";
  if (b.length > 120) return "名稱太長（最多 120 個字）";
  if (parseExportName(`${b}.mp4`)) return "名稱結尾不能是「_數字x」（例如 _4x），會被當成加速版";
  return undefined;
}

/** 「1:30」「90」「1:02:03」→ 秒數；格式不對回傳 undefined */
export function parseClock(text: string): number | undefined {
  const t = text.trim();
  if (!/^\d+(?::\d{1,2}){0,2}(?:\.\d+)?$/.test(t)) return undefined;
  const sec = t.split(":").reduce((acc, part) => acc * 60 + Number(part), 0);
  return Number.isFinite(sec) && sec > 0 ? sec : undefined;
}

/** 依目標長度算倍率（限制在可用範圍，取兩位小數） */
export function speedForTarget(durationSec: number, targetSec: number, allowOriginal = false): number {
  const min = allowOriginal ? 1 : LIMITS.speedMin;
  const raw = durationSec / targetSec;
  return Math.min(LIMITS.speedMax, Math.max(min, Math.round(raw * 100) / 100));
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
