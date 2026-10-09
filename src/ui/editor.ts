/** 剪輯對話框：剪頭尾、刪除中間片段、裁切畫面、加上標註，另存為新檔。 */
import { keepRanges, normalizeCrop, normalizeRanges, totalLength, cutFileName, type EditSpec, type OverlaySpec, type Range } from "../shared/edit.ts";
import { videoClock } from "../shared/format.ts";
import type { LibraryEntry } from "../shared/types.ts";
import { ANN_LABELS, COLORS, EMOJIS, bbox, defaultSize, draw, hit, isBox, label, measure, toOverlay, type Ann, type AnnKind } from "./annotate.ts";

/** 存起來的剪輯設定與標註（之後可以再修改） */
export interface ProjectData {
  v: 1;
  duration: number;
  spec: { start: number; end: number; removed: Range[]; crop?: EditSpec["crop"] };
  cropOn: boolean;
  anns: Ann[];
}

/** /api/edit/project 的回應 */
export interface ProjectInfo {
  project: { output: string; source: string; savedAt: number; data: ProjectData } | null;
  /** output：開啟的就是剪輯版；source：開啟的是原始影片，這是最近一次用它做的剪輯 */
  matched?: "output" | "source";
  /** 原始影片的資訊（找不到原片時沒有） */
  source?: LibraryEntry | null;
}

export interface EditorDeps {
  /** 送出剪輯工作；replace = 取代這個剪輯版，project = 存起來供之後修改 */
  save(source: string, spec: EditSpec, extra: { replace?: string; project?: ProjectData }): Promise<void>;
  /** 查詢之前的剪輯設定 */
  project(path: string): Promise<ProjectInfo>;
  toast(msg: string, error?: boolean): void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

let deps: EditorDeps;
let entry: LibraryEntry | undefined;
let duration = 0;
let fps = 30;
let vw = 0;
let vh = 0;
let spec: EditSpec = { start: 0, end: 0, removed: [] };
/** 在時間軸上拖曳選取、準備刪除的範圍 */
let sel: Range | undefined;
/** 預覽結果：只播放保留的部分 */
let previewing = false;
let cropOn = false;
/** 時間軸顯示的範圍（放大時只顯示一部分） */
let view = { a: 0, b: 0 };
/** 縮圖產生的序號：重新開啟、關閉或縮放時讓舊的停止 */
let stripSeq = 0;
let stripTimer = 0;
/** 產生縮圖用的（不顯示的）影片 */
let stripVideo: HTMLVideoElement | undefined;
let bound = false;
/** 下方顯示的分頁（下次開啟時沿用） */
let tab: "time" | "crop" | "ann" = "time";

// 標註
let anns: Ann[] = [];
let annSel: number | undefined;
let nextAnnId = 1;
/** 目前選的工具（放置一個後回到選取模式）；emoji 為表情工具選好的表情 */
let tool: AnnKind | "emoji" | undefined;
let emoji = EMOJIS[0]!;
/** 拖曳標註軌時固定每個標註所在的列，避免拖曳中跳列 */
let laneFreeze: Map<number, number> | undefined;
/** 儲存時取代這個剪輯版（修改之前的剪輯） */
let replaceTarget: string | undefined;
/** 開啟的那個剪輯版（從原片重新載入時記住，可以改回直接剪輯它） */
let openedCut: LibraryEntry | undefined;
/** 開啟的序號：查詢專案回來時對話框已經換了影片就不套用 */
let openSeq = 0;

const video = () => $<HTMLVideoElement>("edVideo");
const now = () => video().currentTime;
const selected = () => anns.find((a) => a.id === annSel);

/**
 * 開啟剪輯視窗。之前用這支影片做過剪輯（或開啟的就是剪輯版）時，
 * 從原始影片重新載入當時的剪輯與標註，修改後取代那個剪輯版。
 */
export async function openEditor(e: LibraryEntry, d: EditorDeps) {
  deps = d;
  if (!bound) bind();
  const seq = ++openSeq;
  const info = await d.project(e.path).catch(() => undefined);
  if (seq !== openSeq) return;
  const p = info?.project;
  replaceTarget = undefined;
  openedCut = undefined;
  if (p && info.matched === "output" && info.source) {
    // 開啟的是剪輯版：改用原始影片，套用上次的設定
    load(info.source);
    openedCut = e;
    applyProject(p.data);
    replaceTarget = e.path;
  } else {
    load(e);
  }
  showBanner(info);
  render();
  const dlg = $<HTMLDialogElement>("editor");
  if (!dlg.open) dlg.showModal();
}

/** 說明列：正在修改哪個剪輯版、找不到原片、或原片有上次的剪輯可以載入 */
function showBanner(info: ProjectInfo | undefined) {
  const banner = $("edBanner");
  const text = $("edBannerText");
  const acts = $("edBannerActs");
  const p = info?.project;
  banner.classList.remove("warn");
  acts.innerHTML = "";
  const btn = (label: string, fn: () => void) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn small";
    b.textContent = label;
    b.addEventListener("click", fn);
    acts.append(b);
  };
  banner.hidden = false;
  if (replaceTarget) {
    text.textContent = `正在修改剪輯版「${baseName(replaceTarget)}」：已從原始影片「${entry?.name ?? ""}」載入上次的剪輯與標註，可以直接修改。儲存時會取代這個剪輯版。`;
    btn("改為另存新的剪輯版", () => {
      replaceTarget = undefined;
      showBanner(undefined);
      render();
    });
    const cut = openedCut;
    if (cut) btn("改成直接剪輯這個檔案", () => {
      openedCut = undefined;
      replaceTarget = undefined;
      load(cut);
      showBanner(undefined);
      render();
    });
  } else if (p && info.matched === "output" && !info.source) {
    banner.classList.add("warn");
    text.textContent = `找不到這個剪輯版的原始影片「${baseName(p.source)}」，之前的標註已燒進影片、無法修改；只能在這個檔案上繼續剪輯。`;
  } else if (p && info.matched === "source") {
    text.textContent = `這支影片之前剪輯成「${baseName(p.output)}」。`;
    btn("載入上次的剪輯來修改", () => {
      applyProject(p.data);
      replaceTarget = p.output;
      showBanner(undefined);
      render();
    });
  } else banner.hidden = true;
}

const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** 載入影片並清除所有剪輯設定 */
function load(e: LibraryEntry) {
  entry = e;
  duration = e.durationSec ?? 0;
  fps = e.fps ?? 30;
  vw = e.width ?? 0;
  vh = e.height ?? 0;
  spec = { start: 0, end: duration, removed: [] };
  sel = undefined;
  previewing = false;
  cropOn = false;
  view = { a: 0, b: duration };
  anns = [];
  annSel = undefined;
  tool = undefined;
  $("edName").textContent = e.name;
  const v = video();
  v.src = mediaUrl(e.path);
  v.currentTime = 0;
  setAspect();
  stripVideo?.removeAttribute("src");
  stripVideo = document.createElement("video");
  stripVideo.muted = true;
  stripVideo.preload = "auto";
  stripVideo.src = mediaUrl(e.path);
  scheduleStrip(0);
}

/** 套用存起來的剪輯設定與標註 */
function applyProject(d: ProjectData) {
  if (!d || d.v !== 1 || !d.spec) return;
  const full = d.spec.end >= (d.duration || 0) - 0.05;
  spec = {
    start: clamp(Number(d.spec.start) || 0, 0, duration),
    end: full ? duration : clamp(Number(d.spec.end) || duration, 0, duration),
    removed: Array.isArray(d.spec.removed) ? d.spec.removed : [],
    crop: d.spec.crop,
  };
  if (spec.end - spec.start < 0.1) spec = { start: 0, end: duration, removed: [] };
  cropOn = !!d.cropOn && !!d.spec.crop;
  const kinds = new Set(Object.keys(ANN_LABELS));
  anns = (Array.isArray(d.anns) ? d.anns : [])
    .filter((a) => a && kinds.has(a.kind) && [a.x, a.y, a.w, a.h, a.start, a.end, a.size].every(Number.isFinite))
    .map((a) => ({ ...a }));
  for (const a of anns) {
    a.id = nextAnnId++;
    measure(a);
  }
}

/** 目前的剪輯設定與標註（存起來供之後修改） */
function projectData(): ProjectData {
  return { v: 1, duration, spec: { start: spec.start, end: spec.end, removed: spec.removed, crop: spec.crop }, cropOn, anns: anns.map((a) => ({ ...a })) };
}

const mediaUrl = (path: string) => `/api/media?path=${encodeURIComponent(path)}`;

function scheduleStrip(delay = 150) {
  clearTimeout(stripTimer);
  stripTimer = window.setTimeout(() => void drawStrip(), delay);
}

/**
 * 時間軸縮圖：用不顯示的 video 依序跳到時間軸上各個時間點，畫到時間軸的 canvas 上（放大後只畫顯示的範圍）。
 * 一張一張畫，先看到的部分先出現；關閉、換影片或再次縮放時停止。
 */
async function drawStrip() {
  const seq = ++stripSeq;
  const tv = stripVideo;
  const canvas = $<HTMLCanvasElement>("edStrip");
  const ctx = canvas.getContext("2d");
  if (!ctx || !tv) return;
  const tl = $("edTimeline");
  const dpr = window.devicePixelRatio || 1;
  // 對話框剛開啟時版面可能還沒算好：等一個畫格
  await new Promise((r) => requestAnimationFrame(r));
  const w = Math.max(1, Math.round(tl.clientWidth * dpr));
  const h = Math.max(1, Math.round(tl.clientHeight * dpr));
  const { a, b } = view;
  try {
    if (tv.readyState < 2) {
      await new Promise<void>((resolve, reject) => {
        tv.addEventListener("loadeddata", () => resolve(), { once: true });
        tv.addEventListener("error", () => reject(new Error("無法讀取影片")), { once: true });
      });
    }
    if (seq !== stripSeq) return;
    canvas.width = w;
    canvas.height = h;
    ctx.clearRect(0, 0, w, h);
    const ar = tv.videoWidth && tv.videoHeight ? tv.videoWidth / tv.videoHeight : 16 / 9;
    const tileW = Math.max(1, h * ar);
    const n = Math.max(1, Math.ceil(w / tileW));
    const len = Number.isFinite(tv.duration) && tv.duration > 0 ? tv.duration : duration;
    for (let i = 0; i < n; i++) {
      if (seq !== stripSeq) return;
      const t = Math.min(len - 0.05, a + ((i + 0.5) / n) * (b - a));
      await new Promise<void>((resolve) => {
        tv.addEventListener("seeked", () => resolve(), { once: true });
        tv.currentTime = Math.max(0, t);
      });
      if (seq !== stripSeq) return;
      ctx.drawImage(tv, Math.round(i * tileW), 0, Math.ceil(tileW), h);
    }
  } catch {
    // 縮圖只是輔助，失敗就不顯示
  }
}

/** 外框比例 = 影片比例（CSS 用 --ar / --ar-num 同時限制寬高） */
function setAspect() {
  const st = $("edStage").style;
  if (!vw || !vh) return;
  st.setProperty("--ar", `${vw} / ${vh}`);
  st.setProperty("--ar-num", String(vw / vh));
}

/** 釋放影片檔（之後才能刪除或覆寫） */
function releaseMedia() {
  stripSeq++;
  clearTimeout(stripTimer);
  for (const v of [video(), stripVideo]) {
    if (!v) continue;
    v.pause();
    if (v.getAttribute("src")) {
      v.removeAttribute("src");
      v.load();
    }
  }
  stripVideo = undefined;
}

function close() {
  previewing = false;
  releaseMedia();
  $<HTMLDialogElement>("editor").close();
}

// ───────────── 繪製 ─────────────

function render() {
  renumberSteps();
  for (const b of document.querySelectorAll<HTMLButtonElement>("#edTabs [data-tab]")) b.setAttribute("aria-selected", String(b.dataset.tab === tab));
  for (const sec of document.querySelectorAll<HTMLElement>(".ed-grid [data-panel]")) sec.hidden = sec.dataset.panel !== tab;
  $("edAnnCount").textContent = anns.length ? String(anns.length) : "";
  renderTimeline();
  renderTime();
  renderAnnPanel();
  const keep = keepRanges(duration, spec);
  $("edRangeText").textContent = `開頭 ${videoClock(spec.start)}　結尾 ${videoClock(spec.end)}`;

  $("edSelRow").hidden = !sel;
  if (sel) $("edSelText").textContent = `已選取 ${videoClock(sel[0])} – ${videoClock(sel[1])}（${videoClock(sel[1] - sel[0])}）`;
  $("edPreview").textContent = previewing ? "停止預覽" : "預覽結果";
  const removed = normalizeRanges(spec.removed, duration);
  $("edRemoved").innerHTML = removed
    .map(([a, b], i) => `<li>刪除 ${videoClock(a)} – ${videoClock(b)}<button type="button" data-remove="${i}" aria-label="還原這段">×</button></li>`)
    .join("");

  // 裁切
  $<HTMLInputElement>("edCropOn").checked = cropOn;
  $("edCropFields").classList.toggle("off", !cropOn);
  $("edStage").classList.toggle("crop-mode", cropOn && !tool);
  $("edStage").classList.toggle("tool-mode", !!tool);
  const c = spec.crop ?? { x: 0, y: 0, width: vw, height: vh };
  for (const [id, v] of [["ecx", c.x], ["ecy", c.y], ["ecw", c.width], ["ech", c.height]] as const) {
    const input = $<HTMLInputElement>(id);
    if (document.activeElement !== input) input.value = String(v);
  }
  const box = $("edCrop");
  box.hidden = !cropOn || !spec.crop || !vw;
  if (!box.hidden && spec.crop) {
    Object.assign(box.style, {
      left: `${(spec.crop.x / vw) * 100}%`,
      top: `${(spec.crop.y / vh) * 100}%`,
      width: `${(spec.crop.width / vw) * 100}%`,
      height: `${(spec.crop.height / vh) * 100}%`,
    });
  }
  $("edCropHint").textContent = !vw ? "無法讀取影片尺寸，不能裁切。" : "開啟後可直接在影片上拖曳框選要保留的區域。";

  // 摘要
  const length = totalLength(keep);
  const crop = cropOn ? normalizeCrop(spec.crop, vw, vh) : undefined;
  const size = crop ? `${crop.width}×${crop.height}` : vw ? `${vw}×${vh}` : "";
  $("edSummary").innerHTML =
    `輸出長度 <strong>${videoClock(length)}</strong>（原 ${videoClock(duration)}）・保留 ${keep.length} 段` +
    (size ? `・畫面 <strong>${size}</strong>` : "") +
    (anns.length ? `・標註 ${anns.length} 個` : "") +
    (goneCount(keep) ? `<span class="warn-text">（${goneCount(keep)} 個在刪除的片段中，不會出現）</span>` : "") +
    (replaceTarget ? `<br><span class="muted small">儲存後取代 ${esc(baseName(replaceTarget))}</span>` : entry ? `<br><span class="muted small">另存為 ${cutFileName(entry.name)}</span>` : "");
  const unchanged = keep.length === 1 && keep[0]![0] === 0 && keep[0]![1] >= duration - 0.05 && !crop && !anns.length;
  const save = $<HTMLButtonElement>("edSave");
  save.disabled = unchanged || length < 0.1;
  save.textContent = replaceTarget ? "儲存修改" : "另存剪輯版";
  save.title = unchanged ? "還沒有任何剪輯、裁切或標註" : "";
}

/** 時間在時間軸上的位置（0～100%，放大時可能超出） */
const pct = (t: number) => `${((t - view.a) / (view.b - view.a || 1)) * 100}%`;
const pctLen = (d: number) => `${(d / (view.b - view.a || 1)) * 100}%`;

function renderTimeline() {
  if (!duration) return;
  const parts: string[] = [];
  // 剪掉的頭尾
  if (spec.start > 0) parts.push(`<div class="ed-seg cut" style="left:${pct(0)};width:${pctLen(spec.start)}"></div>`);
  if (spec.end < duration) parts.push(`<div class="ed-seg cut" style="left:${pct(spec.end)};width:${pctLen(duration - spec.end)}"></div>`);
  // 刪除的中間片段（點 × 還原）
  normalizeRanges(spec.removed, duration).forEach(([a, b], i) => {
    parts.push(`<div class="ed-seg cut removed" style="left:${pct(a)};width:${pctLen(b - a)}"><button type="button" class="ed-restore" data-remove="${i}" title="還原這段（${videoClock(a)} – ${videoClock(b)}）">×</button></div>`);
  });
  $("edTrack").innerHTML = parts.join("");
  $("edHStart").style.left = pct(spec.start);
  $("edHEnd").style.left = pct(spec.end);
  Object.assign($("edRange").style, { left: pct(spec.start), width: pctLen(spec.end - spec.start) });
  const selEl = $("edSel");
  selEl.hidden = !sel;
  if (sel) Object.assign(selEl.style, { left: pct(sel[0]), width: pctLen(Math.max(sel[1] - sel[0], (view.b - view.a) / 1000)) });
  renderAnnTrack(parts);
  // 放大時的捲軸
  const zoomed = view.b - view.a < duration - 0.001;
  $("edScroll").hidden = !zoomed;
  $("edZoomAll").hidden = !zoomed;
  if (zoomed) Object.assign($("edScrollThumb").style, { left: `${(view.a / duration) * 100}%`, width: `${((view.b - view.a) / duration) * 100}%` });
}

const LANE_H = 22;

/** 標註軌：依出現時間排成幾列（不重疊），刪除的片段一樣壓暗，讓人看得出標註會不會被剪掉 */
function renderAnnTrack(cutParts: string[]) {
  const track = $("edAnnTrack");
  track.hidden = !anns.length;
  if (!anns.length) return;
  const keep = keepRanges(duration, spec);
  const lanes = laneFreeze ?? assignLanes();
  const n = Math.max(1, ...[...lanes.values()].map((l) => l + 1));
  track.style.height = `${n * LANE_H + 4}px`;
  const bars = anns.map((a) => {
    const lane = lanes.get(a.id) ?? 0;
    const blur = a.kind === "mosaic" || a.kind === "blur";
    const c = blur ? "#7a7f87" : a.color;
    const fg = c === "#ffffff" || c === "#f5b301" ? "#111" : "#fff";
    const gone = !inOutput(a, keep);
    const tip = `${label(a)}：${videoClock(a.start)} – ${videoClock(a.end)}${gone ? "（在刪除的片段中，不會出現在輸出影片）" : ""}。拖曳移動，拖曳兩端調整長短`;
    return `<div class="ed-abar${a.id === annSel ? " on" : ""}${gone ? " gone" : ""}" data-ann="${a.id}" title="${esc(tip)}" style="left:${pct(a.start)};width:${pctLen(a.end - a.start)};top:${lane * LANE_H + 3}px;--c:${c};--fg:${fg}"><i class="edge l" data-edge="start"></i>${esc(label(a))}<i class="edge r" data-edge="end"></i></div>`;
  });
  track.innerHTML = cutParts.join("").replace(/<button[^>]*>×<\/button>/g, "") + bars.join("") + `<div class="ed-at-ph" id="edAnnPh" style="left:${pct(now())}"></div>`;
}

/** 依開始時間排列，每個標註放進第一個不重疊的列 */
function assignLanes(): Map<number, number> {
  const ends: number[] = [];
  const map = new Map<number, number>();
  for (const a of [...anns].sort((p, q) => p.start - q.start || p.id - q.id)) {
    let lane = ends.findIndex((e) => e <= a.start + 1e-6);
    if (lane < 0) lane = ends.length;
    ends[lane] = a.end;
    map.set(a.id, lane);
  }
  return map;
}

/** 標註在輸出影片中看得到嗎（與保留的部分重疊） */
function inOutput(a: Ann, keep: Range[] = keepRanges(duration, spec)) {
  return keep.reduce((acc, [x, y]) => acc + Math.max(0, Math.min(y, a.end) - Math.max(x, a.start)), 0) >= 0.05;
}
const goneCount = (keep: Range[]) => anns.filter((a) => !inOutput(a, keep)).length;

/** 步驟編號依出現時間自動排成 1、2、3… */
function renumberSteps() {
  anns
    .filter((a) => a.kind === "step")
    .sort((p, q) => p.start - q.start || p.id - q.id)
    .forEach((a, i) => {
      if (a.n !== i + 1) {
        a.n = i + 1;
        measure(a);
      }
    });
}

function renderTime() {
  const t = now();
  $("edTime").textContent = `${videoClock(t)} / ${videoClock(duration)}`;
  $("edPlayhead").style.left = duration ? pct(t) : "0";
  const ph = document.getElementById("edAnnPh");
  if (ph) ph.style.left = duration ? pct(t) : "0";
  $("edPlay").textContent = video().paused ? "播放" : "暫停";
  // 播放時播放頭跑出放大的範圍：跟著捲動
  if (!video().paused && (t > view.b || t < view.a) && view.b - view.a < duration) {
    const span = view.b - view.a;
    setView(t - span * 0.1, t + span * 0.9);
  }
  drawAnns();
}

// ───────────── 時間軸縮放 ─────────────

function setView(a: number, b: number) {
  const span = clamp(b - a, Math.min(duration, Math.max(0.5, 20 / fps)), duration);
  const start = clamp(a, 0, duration - span);
  view = { a: start, b: start + span };
  renderTimeline();
  renderTime();
  scheduleStrip();
}

/** 以時間 t 為中心縮放（factor < 1 放大） */
function zoom(factor: number, t: number) {
  const span = view.b - view.a;
  const ratio = (t - view.a) / (span || 1);
  const next = span * factor;
  setView(t - ratio * next, t - ratio * next + next);
}

// ───────────── 操作 ─────────────

function seek(t: number) {
  video().currentTime = clamp(t, 0, Math.max(0, duration - 0.001));
}

function step(kind: string) {
  video().pause();
  const d = kind === "frame" ? 1 / fps : kind === "-frame" ? -1 / fps : Number(kind);
  seek(now() + d);
}

function setStart(t = now()) {
  spec.start = clamp(t, 0, spec.end - 0.1);
  render();
}
function setEnd(t = now()) {
  spec.end = clamp(t, spec.start + 0.1, duration);
  render();
}
/** 刪除選取的範圍 */
function deleteSelection() {
  if (!sel) return;
  if (sel[1] - sel[0] < 0.05) deps.toast("選取的片段太短", true);
  else spec.removed = normalizeRanges([...spec.removed, sel], duration);
  sel = undefined;
  render();
}

/** 預覽結果：從保留的第一段開始播放，跳過刪除的片段，播到結尾就停 */
function startPreview() {
  const keep = keepRanges(duration, spec);
  if (!keep.length) return;
  sel = undefined;
  previewing = true;
  const v = video();
  const t = now();
  // 播放頭已在保留範圍內（且不在最後）就從那裡開始，否則從頭
  const inside = keep.find(([a, b]) => t >= a && t < b - 0.1);
  seek(inside ? t : keep[0]![0]);
  v.play().catch(() => {});
  render();
}
function stopPreview() {
  if (!previewing) return;
  previewing = false;
  video().pause();
  render();
}
/** 預覽中：播到刪除的片段就跳到下一段保留的開頭，播完最後一段就停 */
function previewTick() {
  if (!previewing) return;
  const t = now();
  const keep = keepRanges(duration, spec);
  const cur = keep.find(([a, b]) => t >= a - 0.02 && t < b);
  if (cur) return;
  const next = keep.find(([a]) => a > t);
  if (next) seek(next[0]);
  else {
    stopPreview();
    const last = keep[keep.length - 1];
    if (last) seek(last[1] - 0.001);
  }
}

function setCrop(c: { x: number; y: number; width: number; height: number }) {
  spec.crop = normalizeCrop(c, vw, vh) ?? { x: 0, y: 0, width: vw, height: vh };
  render();
}

// ───────────── 標註 ─────────────

/** 標註畫在與影片同尺寸的 canvas 上；馬賽克 / 模糊用 CSS 模糊的區塊預覽 */
function drawAnns() {
  const canvas = $<HTMLCanvasElement>("edAnn");
  const stage = $("edStage");
  const ctx = canvas.getContext("2d");
  if (!ctx || !vw) return;
  const dpr = window.devicePixelRatio || 1;
  const cw = Math.max(1, Math.round(stage.clientWidth * dpr));
  const ch = Math.max(1, Math.round(stage.clientHeight * dpr));
  if (canvas.width !== cw || canvas.height !== ch) {
    canvas.width = cw;
    canvas.height = ch;
  }
  ctx.clearRect(0, 0, cw, ch);
  const s = cw / vw;
  const t = now();
  const blurs: string[] = [];
  for (const a of anns) {
    const visible = t >= a.start && t <= a.end;
    const isSel = a.id === annSel;
    if (!visible && !isSel) continue;
    ctx.globalAlpha = visible ? 1 : 0.35;
    if (a.kind === "mosaic" || a.kind === "blur") {
      blurs.push(
        `<div class="ed-blur ${a.kind}${visible ? "" : " off"}" style="left:${(a.x / vw) * 100}%;top:${(a.y / vh) * 100}%;width:${(a.w / vw) * 100}%;height:${(a.h / vh) * 100}%"><span>${ANN_LABELS[a.kind]}</span></div>`,
      );
    } else draw(ctx, a, s);
    if (isSel) drawSelection(ctx, a, s);
  }
  ctx.globalAlpha = 1;
  $("edBlurs").innerHTML = blurs.join("");
}

/** 選取框與調整大小的把手 */
function drawSelection(ctx: CanvasRenderingContext2D, a: Ann, s: number) {
  ctx.save();
  ctx.globalAlpha = 1;
  ctx.strokeStyle = "#0090ff";
  ctx.lineWidth = 1.5;
  ctx.setLineDash([5, 4]);
  const b = bbox(a);
  ctx.strokeRect(b.x * s, b.y * s, b.w * s, b.h * s);
  ctx.setLineDash([]);
  for (const [hx, hy] of handles(a)) {
    ctx.fillStyle = "#fff";
    ctx.strokeStyle = "#0090ff";
    ctx.beginPath();
    ctx.rect(hx * s - 5, hy * s - 5, 10, 10);
    ctx.fill();
    ctx.stroke();
  }
  ctx.restore();
}

/** 調整大小的把手位置（影片像素）：箭頭為兩端，其他為右下角 */
function handles(a: Ann): [number, number][] {
  if (a.kind === "arrow") return [[a.x, a.y], [a.x + a.w, a.y + a.h]];
  return [[a.x + a.w, a.y + a.h]];
}

/**
 * 新標註的出現時間：從目前位置起 3 秒，但不超出目前所在的保留片段；
 * 太靠近片段結尾時往前延伸（一定包含目前這一格，放好就看得到）。
 */
function defaultWindow(t: number): { start: number; end: number } {
  const len = Math.min(3, duration);
  const seg = keepRanges(duration, spec).find(([a, b]) => t >= a - 1e-3 && t < b) ?? [0, duration];
  const end = Math.min(seg[1], Math.max(t, 0) + len);
  const start = Math.max(seg[0], Math.min(t, end - len));
  return { start, end: Math.max(end, start + 0.1) };
}

function newAnn(kind: AnnKind, x: number, y: number): Ann {
  const a: Ann = {
    id: nextAnnId++,
    kind,
    x,
    y,
    w: 0,
    h: 0,
    ...defaultWindow(now()),
    color: kind === "highlight" ? "#f5b301" : kind === "text" ? "#ffffff" : COLORS[0]!,
    size: defaultSize(kind, vh || 1080),
  };
  if (kind === "text") {
    a.text = tool === "emoji" ? emoji : "說明文字";
    a.bg = tool !== "emoji";
    if (tool === "emoji") a.size = Math.round(defaultSize("text", vh || 1080) * 1.6);
  }
  if (kind === "step") a.n = anns.filter((o) => o.kind === "step").length + 1;
  measure(a);
  if (kind === "text" || kind === "step") {
    // 以點的位置為中心
    a.x = clamp(x - a.w / 2, 0, Math.max(0, vw - a.w));
    a.y = clamp(y - a.h / 2, 0, Math.max(0, vh - a.h));
  }
  return a;
}

function selectAnn(id: number | undefined) {
  annSel = id;
  if (id !== undefined) tab = "ann";
  render();
}

function deleteAnn() {
  const a = selected();
  if (!a) return;
  anns = anns.filter((o) => o !== a);
  annSel = undefined;
  render();
}

/** 「1:05.3」「65.3」「1:02:03」→ 秒（可為 0） */
function parseTime(text: string): number | undefined {
  const t = text.trim();
  if (!/^\d+(?::\d{1,2}){0,2}(?:\.\d+)?$/.test(t)) return undefined;
  return t.split(":").reduce((acc, p) => acc * 60 + Number(p), 0);
}

function renderAnnPanel() {
  for (const b of document.querySelectorAll<HTMLButtonElement>("#edTools [data-tool]")) {
    b.classList.toggle("on", b.dataset.tool === tool);
    b.disabled = !vw;
  }
  $("edEmojis").hidden = tool !== "emoji";
  const a = selected();
  $("edAnnProps").hidden = !a;
  const hint = $("edAnnHint");
  hint.textContent = !vw
    ? "無法讀取影片尺寸，不能加上標註。"
    : tool
      ? tool === "text" || tool === "emoji" || tool === "step"
        ? `在影片上點一下放置${tool === "emoji" ? "（先在上面選表情）" : ""}。按 Esc 取消。`
        : "在影片上按住拖曳放置。按 Esc 取消。"
      : a
        ? ""
        : "① 選工具 ② 在影片上點一下或拖曳放置。標註從目前位置起出現 3 秒，可在時間軸下方的標註軌拖曳調整。";
  hint.hidden = !hint.textContent;
  if (a) {
    const isText = a.kind === "text";
    $("edAnnName").textContent = label(a);
    const warn = $("edAnnWarn");
    warn.hidden = inOutput(a);
    warn.textContent = "這個標註在刪除的片段中，輸出的影片看不到。請在標註軌把它拖到保留的部分。";
    $("edAnnTextField").hidden = !isText;
    const ta = $<HTMLTextAreaElement>("edAnnText");
    if (isText && document.activeElement !== ta) ta.value = a.text ?? "";
    const blurLike = a.kind === "mosaic" || a.kind === "blur";
    $("edColors").hidden = blurLike;
    $("edAnnSizeRow").hidden = blurLike;
    $("edAnnBgWrap").hidden = !isText;
    $<HTMLInputElement>("edAnnBg").checked = !!a.bg;
    $("edAnnSizeLabel").textContent = isText ? "字級" : a.kind === "step" ? "大小" : "線寬";
    const size = $<HTMLInputElement>("edAnnSize");
    const k = (vh || 1080) / 1080;
    const [lo, hi] = isText ? [16, 240] : a.kind === "step" ? [24, 240] : [2, 40];
    size.min = String(Math.round(lo * k));
    size.max = String(Math.round(hi * k));
    size.value = String(a.size);
    for (const b of document.querySelectorAll<HTMLButtonElement>("#edColors [data-color]")) b.classList.toggle("on", b.dataset.color === a.color);
    for (const [id, v] of [["edAnnFrom", a.start], ["edAnnTo", a.end]] as const) {
      const input = $<HTMLInputElement>(id);
      if (document.activeElement !== input) input.value = videoClock(v);
    }
  }
  const keep = keepRanges(duration, spec);
  $("edAnnList").innerHTML = [...anns]
    .sort((p, q) => p.start - q.start || p.id - q.id)
    .map((o) => {
      const gone = !inOutput(o, keep);
      return `<li class="${o.id === annSel ? "on" : ""}${gone ? " gone" : ""}" data-ann="${o.id}" title="${gone ? "在刪除的片段中，不會出現在輸出影片" : ""}"><i style="background:${o.kind === "mosaic" || o.kind === "blur" ? "#888" : o.color}"></i><span class="name">${esc(label(o))}</span><span class="muted">${videoClock(o.start)}–${videoClock(o.end)}</span><button type="button" data-ann-del="${o.id}" aria-label="刪除這個標註">×</button></li>`;
    })
    .join("");
}

/** 更新選取的標註（文字、字級改變時重新計算寬高） */
function updateAnn(change: Partial<Ann>) {
  const a = selected();
  if (!a) return;
  Object.assign(a, change);
  measure(a);
  if (a.end - a.start < 0.1) a.end = Math.min(duration, a.start + 0.1);
  render();
}

function setupAnnotations() {
  setHtmlColors();
  $("edEmojis").innerHTML = EMOJIS.map((e) => `<button type="button" class="btn ghost small" data-emoji="${e}">${e}</button>`).join("");
  $("edTools").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-tool]");
    if (!b) return;
    const t = b.dataset.tool as AnnKind | "emoji";
    tool = tool === t ? undefined : t;
    if (tool) annSel = undefined;
    render();
  });
  $("edEmojis").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-emoji]");
    if (!b) return;
    emoji = b.dataset.emoji!;
    for (const x of document.querySelectorAll<HTMLButtonElement>("#edEmojis [data-emoji]")) x.classList.toggle("on", x === b);
  });
  $("edColors").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-color]");
    if (b) updateAnn({ color: b.dataset.color! });
  });
  $<HTMLTextAreaElement>("edAnnText").addEventListener("input", (e) => updateAnn({ text: (e.target as HTMLTextAreaElement).value }));
  $<HTMLInputElement>("edAnnSize").addEventListener("input", (e) => {
    const a = selected();
    if (!a) return;
    const v = Number((e.target as HTMLInputElement).value);
    // 文字放大時以中心為準
    const cx = a.x + a.w / 2;
    const cy = a.y + a.h / 2;
    updateAnn({ size: v });
    if (a.kind === "text" || a.kind === "step") {
      a.x = cx - a.w / 2;
      a.y = cy - a.h / 2;
      render();
    }
  });
  $<HTMLInputElement>("edAnnBg").addEventListener("change", (e) => updateAnn({ bg: (e.target as HTMLInputElement).checked }));
  for (const [id, key] of [["edAnnFrom", "start"], ["edAnnTo", "end"]] as const) {
    $<HTMLInputElement>(id).addEventListener("change", (e) => {
      const a = selected();
      const v = parseTime((e.target as HTMLInputElement).value);
      if (!a || v === undefined) return void render();
      const t = clamp(v, 0, duration);
      if (key === "start") updateAnn({ start: Math.min(t, a.end - 0.1) });
      else updateAnn({ end: Math.max(t, a.start + 0.1) });
    });
  }
  $("edAnnFromNow").addEventListener("click", () => {
    const a = selected();
    if (a) updateAnn({ start: Math.min(now(), a.end - 0.1) });
  });
  $("edAnnToNow").addEventListener("click", () => {
    const a = selected();
    if (a) updateAnn({ end: Math.max(now(), a.start + 0.1) });
  });
  $("edAnnDel").addEventListener("click", deleteAnn);
  $("edAnnList").addEventListener("click", (e) => {
    const del = (e.target as HTMLElement).closest<HTMLElement>("[data-ann-del]");
    if (del) {
      anns = anns.filter((o) => o.id !== Number(del.dataset.annDel));
      if (annSel === Number(del.dataset.annDel)) annSel = undefined;
      return render();
    }
    const li = (e.target as HTMLElement).closest<HTMLElement>("[data-ann]");
    if (!li) return;
    const a = anns.find((o) => o.id === Number(li.dataset.ann));
    if (!a) return;
    // 選取並跳到它出現的時間
    if (now() < a.start || now() > a.end) seek(a.start);
    selectAnn(a.id);
  });
}

function setHtmlColors() {
  $("edColors").innerHTML = COLORS.map((c) => `<button type="button" class="ed-color" data-color="${c}" style="--c:${c}" aria-label="顏色 ${c}"></button>`).join("");
}

/** 匯出用的標註（畫成 PNG / 馬賽克範圍） */
function overlaysForExport(): OverlaySpec[] {
  return anns.map((a) => toOverlay(a, vw, vh)).filter((o): o is OverlaySpec => !!o);
}

// ───────────── 事件 ─────────────

function bind() {
  bound = true;
  const v = video();
  v.addEventListener("loadedmetadata", () => {
    // 以實際讀到的長度為準（清單中的長度來自 FFmpeg，可能有幾十毫秒差異）
    if (Number.isFinite(v.duration) && v.duration > 0 && Math.abs(v.duration - duration) > 0.05) {
      const wasFull = spec.end >= duration - 0.001;
      const wasAll = view.b >= duration - 0.001 && view.a === 0;
      duration = v.duration;
      if (wasFull) spec.end = duration;
      if (wasAll) view = { a: 0, b: duration };
    }
    if (!vw && v.videoWidth) {
      vw = v.videoWidth;
      vh = v.videoHeight;
      setAspect();
    }
    render();
  });
  v.addEventListener("error", () => deps.toast("瀏覽器無法播放這個檔案", true));
  for (const ev of ["timeupdate", "seeked", "play", "pause"]) v.addEventListener(ev, () => renderTime());
  v.addEventListener("pause", () => {
    // 使用者在預覽中按了暫停（或影片播完）
    if (previewing && !v.seeking) {
      previewing = false;
      render();
    }
  });
  // 播放中才用 rAF 讓播放頭平順移動（預覽時也在這裡跳過刪除的片段）；暫停 / 關閉後停止
  let raf = 0;
  const loop = () => {
    previewTick();
    renderTime();
    raf = v.paused ? 0 : requestAnimationFrame(loop);
  };
  v.addEventListener("play", () => {
    if (!raf) raf = requestAnimationFrame(loop);
  });
  new ResizeObserver(() => drawAnns()).observe($("edStage"));

  $("edClose").addEventListener("click", close);
  // 按 Esc 關閉也要釋放檔案（否則之後移到資源回收筒會顯示「正在使用中」）
  $<HTMLDialogElement>("editor").addEventListener("close", releaseMedia);
  $("edPlay").addEventListener("click", () => {
    if (previewing) return stopPreview();
    v.paused ? v.play().catch(() => {}) : v.pause();
  });
  $("edPreview").addEventListener("click", () => (previewing ? stopPreview() : startPreview()));
  for (const b of document.querySelectorAll<HTMLButtonElement>("[data-step]")) b.addEventListener("click", () => step(b.dataset.step!));
  $("edSetStart").addEventListener("click", () => setStart());
  $("edSetEnd").addEventListener("click", () => setEnd());
  $("edDelSel").addEventListener("click", deleteSelection);
  $("edClearSel").addEventListener("click", () => {
    sel = undefined;
    render();
  });
  const restore = (e: Event) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-remove]");
    if (!b) return false;
    const list = normalizeRanges(spec.removed, duration);
    list.splice(Number(b.dataset.remove), 1);
    spec.removed = list;
    render();
    return true;
  };
  $("edRemoved").addEventListener("click", restore);
  $("edReset").addEventListener("click", () => {
    spec = { start: 0, end: duration, removed: [] };
    sel = undefined;
    stopPreview();
    cropOn = false;
    anns = [];
    annSel = undefined;
    tool = undefined;
    render();
  });
  $("edSave").addEventListener("click", async () => {
    if (!entry) return;
    const btn = $<HTMLButtonElement>("edSave");
    btn.disabled = true;
    try {
      const overlays = overlaysForExport();
      await deps.save(
        entry.path,
        { ...spec, crop: cropOn ? normalizeCrop(spec.crop, vw, vh) : undefined, ...(overlays.length ? { overlays } : {}) },
        { replace: replaceTarget, project: projectData() },
      );
      close();
    } catch (err) {
      deps.toast((err as Error).message, true);
      btn.disabled = false;
    }
  });
  setupAnnotations();
  $("edTabs").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-tab]");
    if (!b) return;
    tab = b.dataset.tab as typeof tab;
    render();
  });

  // 時間軸：點一下跳到該時間；拖曳把手調整頭尾、拖曳上方橫條移動整段、拖曳播放頭跳轉；在其他地方拖曳 = 選取要刪除的範圍
  const tl = $("edTimeline");
  type DragKind = "start" | "end" | "playhead" | "range" | "select";
  let drag: { kind: DragKind; x0: number; t0: number; moved: boolean; orig: { start: number; end: number } } | undefined;
  const timeAt = (e: PointerEvent | WheelEvent) => {
    const r = tl.getBoundingClientRect();
    return clamp(view.a + ((e.clientX - r.left) / r.width) * (view.b - view.a), 0, duration);
  };
  tl.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || !duration) return;
    // 刪除片段上的 ×：還原
    if ((e.target as HTMLElement).closest("[data-remove]")) return;
    stopPreview();
    v.pause();
    const handle = (e.target as HTMLElement).closest<HTMLElement>("[data-handle]")?.dataset.handle as DragKind | undefined;
    drag = { kind: handle ?? "select", x0: e.clientX, t0: timeAt(e), moved: false, orig: { start: spec.start, end: spec.end } };
    tl.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  tl.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const t = timeAt(e);
    if (Math.abs(e.clientX - drag.x0) > 4) drag.moved = true;
    if (drag.kind === "start") {
      setStart(t);
      seek(spec.start); // 畫面跟著顯示新的開頭
    } else if (drag.kind === "end") {
      setEnd(t);
      seek(spec.end);
    } else if (drag.kind === "range") {
      // 整段平移，長度不變
      const len = drag.orig.end - drag.orig.start;
      const s = clamp(drag.orig.start + (t - drag.t0), 0, duration - len);
      spec.start = s;
      spec.end = s + len;
      seek(s);
      render();
    } else if (drag.kind === "playhead") seek(t);
    else if (drag.moved) {
      sel = [Math.min(drag.t0, t), Math.max(drag.t0, t)];
      seek(t);
      render();
    }
  });
  const endDrag = (e: PointerEvent) => {
    if (!drag) return;
    // 沒有拖曳的點擊：跳到該時間並取消選取
    if ((drag.kind === "select" || drag.kind === "range") && !drag.moved) {
      sel = undefined;
      seek(timeAt(e));
      render();
    }
    drag = undefined;
  };
  tl.addEventListener("pointerup", endDrag);
  tl.addEventListener("pointercancel", endDrag);
  tl.addEventListener("click", (e) => restore(e));
  // 滾輪：放大 / 縮小（以滑鼠位置為中心）；Shift 或左右滾動 = 平移（時間軸與標註軌都可以）
  const onWheel = (e: WheelEvent) => {
    if (!duration) return;
    e.preventDefault();
    const span = view.b - view.a;
    const horizontal = Math.abs(e.deltaX) > Math.abs(e.deltaY);
    if (e.shiftKey || horizontal) {
      const d = (horizontal ? e.deltaX : e.deltaY) / Math.max(1, tl.clientWidth);
      setView(view.a + d * span, view.b + d * span);
    } else zoom(Math.exp(e.deltaY * 0.0015), timeAt(e));
  };
  tl.addEventListener("wheel", onWheel, { passive: false });

  // 標註軌：點一下選取（並跳到它出現的時間）；拖曳移動出現時間，拖曳兩端調整開始 / 結束；點空白處跳到該時間
  const at = $("edAnnTrack");
  at.addEventListener("wheel", onWheel, { passive: false });
  let adrag: { id: number; edge: "start" | "end" | "body"; t0: number; x0: number; moved: boolean; orig: { start: number; end: number } } | undefined;
  at.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || !duration) return;
    stopPreview();
    v.pause();
    const bar = (e.target as HTMLElement).closest<HTMLElement>("[data-ann]");
    if (!bar) {
      annSel = undefined;
      seek(timeAt(e));
      return render();
    }
    const a = anns.find((o) => o.id === Number(bar.dataset.ann));
    if (!a) return;
    const edge = ((e.target as HTMLElement).closest<HTMLElement>("[data-edge]")?.dataset.edge as "start" | "end" | undefined) ?? "body";
    adrag = { id: a.id, edge, t0: timeAt(e), x0: e.clientX, moved: false, orig: { start: a.start, end: a.end } };
    laneFreeze = assignLanes();
    annSel = a.id;
    tab = "ann";
    at.setPointerCapture(e.pointerId);
    e.preventDefault();
    render();
  });
  at.addEventListener("pointermove", (e) => {
    if (!adrag) return;
    const a = anns.find((o) => o.id === adrag!.id);
    if (!a) return;
    if (Math.abs(e.clientX - adrag.x0) > 3) adrag.moved = true;
    if (!adrag.moved) return;
    const d = timeAt(e) - adrag.t0;
    const o = adrag.orig;
    if (adrag.edge === "body") {
      const len = o.end - o.start;
      a.start = clamp(o.start + d, 0, duration - len);
      a.end = a.start + len;
      seek(a.start);
    } else if (adrag.edge === "start") {
      a.start = clamp(o.start + d, 0, a.end - 0.1);
      seek(a.start);
    } else {
      a.end = clamp(o.end + d, a.start + 0.1, duration);
      seek(a.end - 0.001);
    }
    render();
  });
  const endAnnDrag = () => {
    if (!adrag) return;
    const a = anns.find((o) => o.id === adrag!.id);
    // 只是點一下：跳到它出現的時間（已經在範圍內就不動）
    if (a && !adrag.moved && (now() < a.start || now() > a.end)) seek(a.start);
    adrag = undefined;
    laneFreeze = undefined;
    render();
  };
  at.addEventListener("pointerup", endAnnDrag);
  at.addEventListener("pointercancel", endAnnDrag);
  $("edZoomAll").addEventListener("click", () => setView(0, duration));
  // 放大時的捲軸：拖曳移動顯示的範圍，點捲軸其他地方跳過去
  const scroll = $("edScroll");
  let scrollDrag: { x0: number; a0: number } | undefined;
  scroll.addEventListener("pointerdown", (e) => {
    const r = scroll.getBoundingClientRect();
    const span = view.b - view.a;
    if (!(e.target as HTMLElement).closest("#edScrollThumb")) {
      const t = ((e.clientX - r.left) / r.width) * duration;
      setView(t - span / 2, t + span / 2);
    }
    scrollDrag = { x0: e.clientX, a0: view.a };
    scroll.setPointerCapture(e.pointerId);
  });
  scroll.addEventListener("pointermove", (e) => {
    if (!scrollDrag) return;
    const r = scroll.getBoundingClientRect();
    const span = view.b - view.a;
    const a = scrollDrag.a0 + ((e.clientX - scrollDrag.x0) / r.width) * duration;
    setView(a, a + span);
  });
  scroll.addEventListener("pointerup", () => (scrollDrag = undefined));

  // 影片上：放置 / 移動 / 調整標註；裁切模式拖曳框選；其他時候點一下 = 播放 / 暫停
  const stage = $("edStage");
  type StageDrag =
    | { kind: "crop"; from: { x: number; y: number } }
    | { kind: "create"; ann: Ann; from: { x: number; y: number } }
    | { kind: "move"; ann: Ann; from: { x: number; y: number }; orig: Ann }
    | { kind: "resize"; ann: Ann; handle: number; orig: Ann };
  let sdrag: StageDrag | undefined;
  const toVideo = (e: PointerEvent | WheelEvent) => {
    const r = stage.getBoundingClientRect();
    return { x: clamp((e.clientX - r.left) / r.width, 0, 1) * vw, y: clamp((e.clientY - r.top) / r.height, 0, 1) * vh };
  };
  /** 點到的標註（目前時間看得到的，最上面的優先；選取中的也算） */
  const annAt = (p: { x: number; y: number }) => {
    const tol = (vw / Math.max(1, stage.clientWidth)) * 6;
    const t = now();
    return [...anns].reverse().find((a) => (a.id === annSel || (t >= a.start && t <= a.end)) && hit(a, p.x, p.y, tol));
  };
  const handleAt = (a: Ann, p: { x: number; y: number }) => {
    const tol = (vw / Math.max(1, stage.clientWidth)) * 9;
    return handles(a).findIndex(([hx, hy]) => Math.abs(hx - p.x) <= tol && Math.abs(hy - p.y) <= tol);
  };
  stage.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    if (!vw) {
      v.paused ? v.play().catch(() => {}) : v.pause();
      return;
    }
    const p = toVideo(e);
    if (tool) {
      v.pause();
      stopPreview();
      const kind: AnnKind = tool === "emoji" ? "text" : tool;
      const a = newAnn(kind, p.x, p.y);
      anns.push(a);
      annSel = a.id;
      tab = "ann";
      if (kind === "text" || kind === "step") {
        // 點一下就放好
        tool = undefined;
        render();
        if (kind === "text" && a.text === "說明文字") {
          const ta = $<HTMLTextAreaElement>("edAnnText");
          ta.focus();
          ta.select();
        }
        return;
      }
      sdrag = { kind: "create", ann: a, from: p };
      stage.setPointerCapture(e.pointerId);
      e.preventDefault();
      return render();
    }
    const cur = selected();
    const h = cur ? handleAt(cur, p) : -1;
    if (cur && h >= 0) {
      sdrag = { kind: "resize", ann: cur, handle: h, orig: { ...cur } };
      stage.setPointerCapture(e.pointerId);
      e.preventDefault();
      return;
    }
    const a = annAt(p);
    if (a) {
      v.pause();
      annSel = a.id;
      tab = "ann";
      sdrag = { kind: "move", ann: a, from: p, orig: { ...a } };
      stage.setPointerCapture(e.pointerId);
      e.preventDefault();
      return render();
    }
    if (annSel !== undefined) {
      // 點空白處：取消選取標註
      return selectAnn(undefined);
    }
    if (!cropOn) {
      v.paused ? v.play().catch(() => {}) : v.pause();
      return;
    }
    sdrag = { kind: "crop", from: p };
    stage.setPointerCapture(e.pointerId);
  });
  stage.addEventListener("pointermove", (e) => {
    if (!sdrag) return;
    const p = toVideo(e);
    if (sdrag.kind === "crop") {
      const f = sdrag.from;
      spec.crop = { x: Math.min(f.x, p.x), y: Math.min(f.y, p.y), width: Math.abs(p.x - f.x), height: Math.abs(p.y - f.y) };
      return render();
    }
    const a = sdrag.ann;
    if (sdrag.kind === "create") {
      const f = sdrag.from;
      if (a.kind === "arrow") {
        a.w = p.x - f.x;
        a.h = p.y - f.y;
      } else {
        a.x = Math.min(f.x, p.x);
        a.y = Math.min(f.y, p.y);
        a.w = Math.abs(p.x - f.x);
        a.h = Math.abs(p.y - f.y);
      }
    } else if (sdrag.kind === "move") {
      a.x = sdrag.orig.x + (p.x - sdrag.from.x);
      a.y = sdrag.orig.y + (p.y - sdrag.from.y);
    } else {
      const o = sdrag.orig;
      if (a.kind === "arrow") {
        if (sdrag.handle === 0) {
          // 移動起點，終點不動
          a.x = p.x;
          a.y = p.y;
          a.w = o.x + o.w - p.x;
          a.h = o.y + o.h - p.y;
        } else {
          a.w = p.x - o.x;
          a.h = p.y - o.y;
        }
      } else if (a.kind === "text" || a.kind === "step") {
        // 拉角：依寬度等比例調整字級 / 大小
        const ratio = Math.max(0.2, (p.x - o.x) / Math.max(1, o.w));
        a.size = Math.max(8, Math.round(o.size * ratio));
        measure(a);
      } else {
        a.w = Math.max(8, p.x - o.x);
        a.h = Math.max(8, p.y - o.y);
      }
    }
    drawAnns();
  });
  const endStage = () => {
    if (!sdrag) return;
    if (sdrag.kind === "crop") {
      if (spec.crop) setCrop(spec.crop);
    } else if (sdrag.kind === "create") {
      const a = sdrag.ann;
      // 拖曳太短：給個預設大小
      const k = (vh || 1080) / 1080;
      if (a.kind === "arrow" && Math.hypot(a.w, a.h) < 20 * k) {
        a.w = 160 * k;
        a.h = -100 * k;
      } else if (a.kind !== "arrow" && (a.w < 12 * k || a.h < 12 * k)) {
        a.w = 320 * k;
        a.h = 180 * k;
      }
      tool = undefined;
    }
    sdrag = undefined;
    render();
  };
  stage.addEventListener("pointerup", endStage);
  stage.addEventListener("pointercancel", endStage);
  // 影片上轉滾輪：前後一張（Shift 一秒）
  let wheelAt = 0;
  stage.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      const t = performance.now();
      if (t - wheelAt < 40) return; // 觸控板一次會送很多事件
      wheelAt = t;
      const forward = (Math.abs(e.deltaY) >= Math.abs(e.deltaX) ? e.deltaY : e.deltaX) > 0;
      step(e.shiftKey ? (forward ? "1" : "-1") : forward ? "frame" : "-frame");
    },
    { passive: false },
  );
  $<HTMLInputElement>("edCropOn").addEventListener("change", (e) => {
    cropOn = (e.target as HTMLInputElement).checked;
    if (cropOn && !spec.crop && vw) spec.crop = { x: Math.round(vw * 0.1 / 2) * 2, y: Math.round(vh * 0.1 / 2) * 2, width: Math.round(vw * 0.8 / 2) * 2, height: Math.round(vh * 0.8 / 2) * 2 };
    render();
  });
  for (const [id, key] of [["ecx", "x"], ["ecy", "y"], ["ecw", "width"], ["ech", "height"]] as const) {
    $<HTMLInputElement>(id).addEventListener("change", (e) => {
      const val = Number((e.target as HTMLInputElement).value);
      if (!Number.isFinite(val)) return;
      setCrop({ ...(spec.crop ?? { x: 0, y: 0, width: vw, height: vh }), [key]: val });
    });
  }

  // 快捷鍵：空白 = 播放/暫停、←/→ = 一張（Shift = 一秒）、I/O = 開頭/結尾、Delete = 刪除選取的片段或標註、Esc = 取消
  $<HTMLDialogElement>("editor").addEventListener("keydown", (e) => {
    if ((e.target as HTMLElement).matches("input, select, textarea")) return;
    const k = e.key.toLowerCase();
    if (k === " ") {
      if (previewing) stopPreview();
      else v.paused ? v.play().catch(() => {}) : v.pause();
    } else if (k === "arrowleft") step(e.shiftKey ? "-1" : "-frame");
    else if (k === "arrowright") step(e.shiftKey ? "1" : "frame");
    else if (k === "i") setStart();
    else if (k === "o") setEnd();
    else if ((k === "delete" || k === "backspace" || k === "d") && sel) deleteSelection();
    else if ((k === "delete" || k === "backspace") && annSel !== undefined) deleteAnn();
    else if (k === "escape" && (sel || tool || annSel !== undefined)) {
      // 有選取或正在放置時 Esc 只取消，不關閉對話框
      sel = undefined;
      tool = undefined;
      annSel = undefined;
      render();
    } else return;
    e.preventDefault();
  });
}
