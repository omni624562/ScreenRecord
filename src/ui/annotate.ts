/**
 * 剪輯視窗的標註：文字（含表情符號）、箭頭、方框、圓框、螢光筆、步驟編號、馬賽克、模糊。
 * 座標與大小一律用原影片的像素，時間用原影片的秒數（剪輯前）。
 * 匯出時，馬賽克 / 模糊交給 FFmpeg；其他標註在這裡畫成透明 PNG，由 FFmpeg 疊上。
 */
import type { OverlaySpec } from "../shared/edit.ts";

export type AnnKind = "text" | "arrow" | "rect" | "ellipse" | "highlight" | "step" | "mosaic" | "blur";
/** 馬賽克 / 模糊的形狀 */
export type Shape = "rect" | "round" | "ellipse";
export const SHAPE_LABELS: Record<Shape, string> = { rect: "方形", round: "圓角", ellipse: "橢圓" };

export interface Ann {
  id: number;
  kind: AnnKind;
  /** 左上角；箭頭為起點 */
  x: number;
  y: number;
  /** 寬高；箭頭為終點減起點（可為負） */
  w: number;
  h: number;
  start: number;
  end: number;
  color: string;
  /** 文字：字級；箭頭 / 框線：線寬；編號：圓的直徑 */
  size: number;
  text?: string;
  /** 文字加半透明深色底 */
  bg?: boolean;
  n?: number;
  /** 馬賽克 / 模糊的形狀（預設方形） */
  shape?: Shape;
  /** 範圍外模糊（或馬賽克），範圍內清楚 */
  invert?: boolean;
}

/**
 * 馬賽克的格子大小 / 模糊的半徑（影片像素），與匯出時 FFmpeg 用的一致（core/src/args.rs 的 blur_effect）。
 * 框外時以整個畫面為準。
 */
export function effectSize(a: Ann, vw: number, vh: number): number {
  const short = a.invert ? Math.min(vw, vh) : Math.min(Math.abs(a.w), Math.abs(a.h));
  if (a.kind === "mosaic") return Math.max(12, Math.floor(short / (a.invert ? 45 : 6)));
  return Math.min(30, Math.max(a.invert ? 4 : 1, Math.floor(short / (a.invert ? 40 : 4)) - (a.invert ? 0 : 1)));
}

/** 圓角的半徑（影片像素） */
export const roundRadius = (w: number, h: number) => Math.min(Math.abs(w), Math.abs(h)) * 0.2;

/** 形狀的 SVG 路徑（影片像素座標） */
export function shapePath(shape: Shape | undefined, x: number, y: number, w: number, h: number): string {
  if (shape === "ellipse") {
    const rx = w / 2;
    const ry = h / 2;
    return `M${x} ${y + ry}a${rx} ${ry} 0 1 0 ${w} 0a${rx} ${ry} 0 1 0 ${-w} 0Z`;
  }
  if (shape === "round") {
    const r = roundRadius(w, h);
    return `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h - r}A${r} ${r} 0 0 1 ${x + w - r} ${y + h}H${x + r}A${r} ${r} 0 0 1 ${x} ${y + h - r}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`;
  }
  return `M${x} ${y}h${w}v${h}h${-w}Z`;
}

export const ANN_LABELS: Record<AnnKind, string> = {
  text: "文字",
  arrow: "箭頭",
  rect: "方框",
  ellipse: "圓框",
  highlight: "螢光筆",
  step: "編號",
  mosaic: "馬賽克",
  blur: "模糊",
};

export const COLORS = ["#e5484d", "#f5b301", "#30a46c", "#0090ff", "#8e4ec6", "#ffffff", "#111111"];
export const EMOJIS = ["👉", "👆", "✅", "❌", "⚠️", "💡", "⭐", "❓", "👍", "😀", "😮", "🔥"];

const FONT_FAMILY = `"Microsoft JhengHei UI", "Microsoft JhengHei", "Segoe UI Emoji", "Segoe UI", "Noto Color Emoji", sans-serif`;

/** 依影片高度換算的預設大小（以 1080p 為準） */
export function defaultSize(kind: AnnKind, vh: number): number {
  const k = vh / 1080;
  if (kind === "text") return Math.round(48 * k);
  if (kind === "step") return Math.round(64 * k);
  return Math.max(2, Math.round(8 * k));
}

export const isBox = (k: AnnKind) => k === "rect" || k === "ellipse" || k === "highlight" || k === "mosaic" || k === "blur";

function font(a: Ann) {
  return `700 ${a.size}px ${FONT_FAMILY}`;
}

let measureCtx: CanvasRenderingContext2D | null | undefined;
/** 文字與編號的寬高由內容決定 */
export function measure(a: Ann) {
  if (a.kind === "step") {
    a.w = a.h = a.size;
    return;
  }
  if (a.kind !== "text") return;
  measureCtx ??= document.createElement("canvas").getContext("2d");
  const ctx = measureCtx;
  if (!ctx) return;
  ctx.font = font(a);
  const lines = (a.text || " ").split("\n");
  const pad = a.bg ? a.size * 0.35 : a.size * 0.1;
  a.w = Math.ceil(Math.max(...lines.map((l) => ctx.measureText(l || " ").width)) + pad * 2);
  a.h = Math.ceil(lines.length * a.size * 1.25 + pad * 2 - a.size * 0.25);
}

/** 標註實際佔的範圍（含線寬、箭頭頭部），用於匯出圖片與點選 */
export function bbox(a: Ann): { x: number; y: number; w: number; h: number } {
  if (a.kind === "arrow") {
    const pad = a.size * 2.5;
    const x0 = Math.min(a.x, a.x + a.w);
    const y0 = Math.min(a.y, a.y + a.h);
    return { x: x0 - pad, y: y0 - pad, w: Math.abs(a.w) + pad * 2, h: Math.abs(a.h) + pad * 2 };
  }
  const pad = a.kind === "rect" || a.kind === "ellipse" ? a.size : 0;
  return { x: a.x - pad, y: a.y - pad, w: a.w + pad * 2, h: a.h + pad * 2 };
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, r);
}

/** 畫一個標註（s = 影片像素換算成畫布像素的倍率）；馬賽克 / 模糊不在這裡畫 */
export function draw(ctx: CanvasRenderingContext2D, a: Ann, s: number) {
  ctx.save();
  ctx.scale(s, s);
  ctx.lineJoin = "round";
  ctx.lineCap = "round";
  if (a.kind === "text") {
    ctx.font = font(a);
    ctx.textBaseline = "top";
    const lines = (a.text || " ").split("\n");
    const pad = a.bg ? a.size * 0.35 : a.size * 0.1;
    if (a.bg) {
      ctx.fillStyle = "rgba(0,0,0,0.62)";
      roundRect(ctx, a.x, a.y, a.w, a.h, a.size * 0.3);
      ctx.fill();
    }
    lines.forEach((line, i) => {
      const ty = a.y + pad + i * a.size * 1.25;
      if (!a.bg) {
        // 沒有底色時加上外框，在任何背景上都看得清楚
        ctx.strokeStyle = a.color === "#111111" ? "rgba(255,255,255,0.9)" : "rgba(0,0,0,0.75)";
        ctx.lineWidth = Math.max(2, a.size / 8);
        ctx.strokeText(line, a.x + pad, ty);
      }
      ctx.fillStyle = a.color;
      ctx.fillText(line, a.x + pad, ty);
    });
  } else if (a.kind === "arrow") {
    const x2 = a.x + a.w;
    const y2 = a.y + a.h;
    const ang = Math.atan2(a.h, a.w);
    const head = a.size * 3.2;
    const shadow = (fn: () => void) => {
      ctx.strokeStyle = "rgba(0,0,0,0.35)";
      ctx.lineWidth = a.size + 3;
      fn();
    };
    const path = () => {
      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.lineTo(x2 - Math.cos(ang) * head * 0.6, y2 - Math.sin(ang) * head * 0.6);
      ctx.stroke();
    };
    shadow(path);
    ctx.strokeStyle = a.color;
    ctx.lineWidth = a.size;
    path();
    ctx.fillStyle = a.color;
    ctx.beginPath();
    ctx.moveTo(x2, y2);
    ctx.lineTo(x2 - Math.cos(ang - 0.45) * head, y2 - Math.sin(ang - 0.45) * head);
    ctx.lineTo(x2 - Math.cos(ang + 0.45) * head, y2 - Math.sin(ang + 0.45) * head);
    ctx.closePath();
    ctx.fill();
  } else if (a.kind === "rect" || a.kind === "ellipse") {
    ctx.strokeStyle = a.color;
    ctx.lineWidth = a.size;
    ctx.beginPath();
    if (a.kind === "rect") ctx.roundRect(a.x, a.y, a.w, a.h, a.size);
    else ctx.ellipse(a.x + a.w / 2, a.y + a.h / 2, Math.abs(a.w / 2), Math.abs(a.h / 2), 0, 0, Math.PI * 2);
    ctx.stroke();
  } else if (a.kind === "highlight") {
    ctx.fillStyle = a.color;
    ctx.globalAlpha = 0.35;
    ctx.fillRect(a.x, a.y, a.w, a.h);
  } else if (a.kind === "step") {
    const r = a.size / 2;
    ctx.fillStyle = a.color;
    ctx.beginPath();
    ctx.arc(a.x + r, a.y + r, r, 0, Math.PI * 2);
    ctx.fill();
    ctx.lineWidth = Math.max(2, a.size / 16);
    ctx.strokeStyle = "#fff";
    ctx.stroke();
    ctx.fillStyle = a.color === "#ffffff" || a.color === "#f5b301" ? "#111" : "#fff";
    ctx.font = `700 ${Math.round(a.size * 0.56)}px ${FONT_FAMILY}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(String(a.n ?? 1), a.x + r, a.y + r + a.size * 0.03);
  }
  ctx.restore();
}

/** 點 (x, y)（影片像素）是否點到這個標註 */
export function hit(a: Ann, x: number, y: number, tolerance: number): boolean {
  if (a.kind === "arrow") {
    // 到線段的距離
    const x2 = a.x + a.w;
    const y2 = a.y + a.h;
    const len2 = a.w * a.w + a.h * a.h || 1;
    const t = Math.max(0, Math.min(1, ((x - a.x) * a.w + (y - a.y) * a.h) / len2));
    const dx = x - (a.x + t * a.w);
    const dy = y - (a.y + t * a.h);
    return Math.hypot(dx, dy) <= a.size * 2 + tolerance || Math.hypot(x - x2, y - y2) <= a.size * 3 + tolerance;
  }
  const b = bbox(a);
  return x >= b.x - tolerance && x <= b.x + b.w + tolerance && y >= b.y - tolerance && y <= b.y + b.h + tolerance;
}

/** 匯出：馬賽克 / 模糊給 FFmpeg 處理；其他畫成剛好包住標註的透明 PNG */
export function toOverlay(a: Ann, vw: number, vh: number): OverlaySpec | undefined {
  if (a.kind === "mosaic" || a.kind === "blur") {
    const o: OverlaySpec = { kind: a.kind, x: Math.round(a.x), y: Math.round(a.y), w: Math.round(a.w), h: Math.round(a.h), start: a.start, end: a.end };
    if (a.invert) o.invert = true;
    // 圓角、橢圓：畫一張黑底白色形狀的遮罩（FFmpeg 會縮放成範圍大小）
    if (a.shape && a.shape !== "rect" && o.w >= 2 && o.h >= 2) {
      const canvas = document.createElement("canvas");
      canvas.width = o.w;
      canvas.height = o.h;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        ctx.fillStyle = "#000";
        ctx.fillRect(0, 0, o.w, o.h);
        ctx.fillStyle = "#fff";
        ctx.fill(new Path2D(shapePath(a.shape, 0, 0, o.w, o.h)));
        o.mask = canvas.toDataURL("image/png");
      }
    }
    return o;
  }
  const b = bbox(a);
  // 只保留畫面內的部分
  const x0 = Math.max(0, Math.floor(b.x));
  const y0 = Math.max(0, Math.floor(b.y));
  const x1 = Math.min(vw, Math.ceil(b.x + b.w));
  const y1 = Math.min(vh, Math.ceil(b.y + b.h));
  if (x1 - x0 < 1 || y1 - y0 < 1) return undefined;
  const canvas = document.createElement("canvas");
  canvas.width = x1 - x0;
  canvas.height = y1 - y0;
  const ctx = canvas.getContext("2d");
  if (!ctx) return undefined;
  ctx.translate(-x0, -y0);
  draw(ctx, a, 1);
  return { kind: "image", x: x0, y: y0, w: x1 - x0, h: y1 - y0, start: a.start, end: a.end, png: canvas.toDataURL("image/png") };
}

/** 清單與時間軸上顯示的名稱 */
export function label(a: Ann): string {
  if (a.kind === "text") {
    const t = (a.text ?? "").replace(/\s+/g, " ").trim();
    return t ? `「${t.length > 12 ? `${t.slice(0, 12)}…` : t}」` : "文字";
  }
  if (a.kind === "step") return `編號 ${a.n ?? 1}`;
  if (a.kind === "mosaic" || a.kind === "blur") {
    const parts = [a.shape && a.shape !== "rect" ? SHAPE_LABELS[a.shape] : "", a.invert ? "範圍外" : ""].filter(Boolean);
    return parts.length ? `${ANN_LABELS[a.kind]}（${parts.join("・")}）` : ANN_LABELS[a.kind];
  }
  return ANN_LABELS[a.kind];
}
