/** 剪輯對話框：剪頭尾、刪除中間片段、裁切畫面，另存為新檔。 */
import { keepRanges, normalizeCrop, normalizeRanges, totalLength, cutFileName, type EditSpec, type Range } from "../shared/edit.ts";
import { videoClock } from "../shared/format.ts";
import type { LibraryEntry } from "../shared/types.ts";

export interface EditorDeps {
  /** 送出剪輯工作；成功後回傳 */
  save(source: string, spec: EditSpec): Promise<void>;
  toast(msg: string, error?: boolean): void;
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

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
/** 縮圖產生的序號：重新開啟或關閉時讓舊的停止 */
let stripSeq = 0;
let bound = false;

const video = () => $<HTMLVideoElement>("edVideo");
const now = () => video().currentTime;

export function openEditor(e: LibraryEntry, d: EditorDeps) {
  deps = d;
  entry = e;
  if (!bound) bind();
  duration = e.durationSec ?? 0;
  fps = e.fps ?? 30;
  vw = e.width ?? 0;
  vh = e.height ?? 0;
  spec = { start: 0, end: duration, removed: [] };
  sel = undefined;
  previewing = false;
  cropOn = false;
  $("edName").textContent = e.name;
  const v = video();
  v.src = mediaUrl(e.path);
  v.currentTime = 0;
  setAspect();
  render();
  $<HTMLDialogElement>("editor").showModal();
  void drawStrip(e.path);
}

const mediaUrl = (path: string) => `/api/media?path=${encodeURIComponent(path)}`;

/**
 * 時間軸縮圖：用另一個（不顯示的）video 依序跳到各個時間點，畫到時間軸的 canvas 上。
 * 一張一張畫，先看到的部分先出現；關閉或換影片時停止並釋放檔案。
 */
async function drawStrip(path: string) {
  const seq = ++stripSeq;
  const canvas = $<HTMLCanvasElement>("edStrip");
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  const tl = $("edTimeline");
  const dpr = window.devicePixelRatio || 1;
  // 對話框剛開啟時版面可能還沒算好：等一個畫格
  await new Promise((r) => requestAnimationFrame(r));
  const w = Math.max(1, Math.round(tl.clientWidth * dpr));
  const h = Math.max(1, Math.round(tl.clientHeight * dpr));
  canvas.width = w;
  canvas.height = h;
  ctx.clearRect(0, 0, w, h);
  const tv = document.createElement("video");
  tv.muted = true;
  tv.preload = "auto";
  tv.src = mediaUrl(path);
  const release = () => {
    tv.removeAttribute("src");
    tv.load(); // 釋放檔案
  };
  try {
    await new Promise<void>((resolve, reject) => {
      tv.addEventListener("loadeddata", () => resolve(), { once: true });
      tv.addEventListener("error", () => reject(new Error("無法讀取影片")), { once: true });
    });
    const ar = tv.videoWidth && tv.videoHeight ? tv.videoWidth / tv.videoHeight : 16 / 9;
    const tileW = Math.max(1, h * ar);
    const n = Math.max(1, Math.ceil(w / tileW));
    const len = Number.isFinite(tv.duration) && tv.duration > 0 ? tv.duration : duration;
    for (let i = 0; i < n; i++) {
      if (seq !== stripSeq) return;
      const t = Math.min(len - 0.05, ((i + 0.5) / n) * len);
      await new Promise<void>((resolve) => {
        tv.addEventListener("seeked", () => resolve(), { once: true });
        tv.currentTime = Math.max(0, t);
      });
      if (seq !== stripSeq) return;
      ctx.drawImage(tv, Math.round(i * tileW), 0, Math.ceil(tileW), h);
    }
  } catch {
    // 縮圖只是輔助，失敗就不顯示
  } finally {
    release();
  }
}

/** 外框比例 = 影片比例（CSS 用 --ar / --ar-num 同時限制寬高） */
function setAspect() {
  const st = $("edStage").style;
  if (!vw || !vh) return;
  st.setProperty("--ar", `${vw} / ${vh}`);
  st.setProperty("--ar-num", String(vw / vh));
}

function close() {
  stripSeq++;
  previewing = false;
  const v = video();
  v.pause();
  v.removeAttribute("src");
  v.load(); // 釋放檔案，避免之後無法刪除或覆寫
  $<HTMLDialogElement>("editor").close();
}

// ───────────── 繪製 ─────────────

function render() {
  renderTimeline();
  renderTime();
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
  $("edStage").classList.toggle("crop-mode", cropOn);
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
    (entry ? `<br><span class="muted small">另存為 ${cutFileName(entry.name)}</span>` : "");
  const unchanged = keep.length === 1 && keep[0]![0] === 0 && keep[0]![1] >= duration - 0.05 && !crop;
  const save = $<HTMLButtonElement>("edSave");
  save.disabled = unchanged || length < 0.1;
  save.title = unchanged ? "還沒有任何剪輯或裁切" : "";
}

function renderTimeline() {
  if (!duration) return;
  const pct = (t: number) => `${(t / duration) * 100}%`;
  const parts: string[] = [];
  // 剪掉的頭尾
  if (spec.start > 0) parts.push(`<div class="ed-seg cut" style="left:0;width:${pct(spec.start)}"></div>`);
  if (spec.end < duration) parts.push(`<div class="ed-seg cut" style="left:${pct(spec.end)};width:${pct(duration - spec.end)}"></div>`);
  // 刪除的中間片段（點 × 還原）
  normalizeRanges(spec.removed, duration).forEach(([a, b], i) => {
    parts.push(`<div class="ed-seg cut removed" style="left:${pct(a)};width:${pct(b - a)}"><button type="button" class="ed-restore" data-remove="${i}" title="還原這段（${videoClock(a)} – ${videoClock(b)}）">×</button></div>`);
  });
  $("edTrack").innerHTML = parts.join("");
  $("edHStart").style.left = pct(spec.start);
  $("edHEnd").style.left = pct(spec.end);
  const selEl = $("edSel");
  selEl.hidden = !sel;
  if (sel) Object.assign(selEl.style, { left: pct(sel[0]), width: pct(Math.max(sel[1] - sel[0], duration / 1000)) });
}

function renderTime() {
  const t = now();
  $("edTime").textContent = `${videoClock(t)} / ${videoClock(duration)}`;
  $("edPlayhead").style.left = duration ? `${(t / duration) * 100}%` : "0";
  $("edPlay").textContent = video().paused ? "播放" : "暫停";
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
  void v.play();
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

function bind() {
  bound = true;
  const v = video();
  v.addEventListener("loadedmetadata", () => {
    // 以實際讀到的長度為準（清單中的長度來自 FFmpeg，可能有幾十毫秒差異）
    if (Number.isFinite(v.duration) && v.duration > 0 && Math.abs(v.duration - duration) > 0.05) {
      const wasFull = spec.end >= duration - 0.001;
      duration = v.duration;
      if (wasFull) spec.end = duration;
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

  $("edClose").addEventListener("click", close);
  // 按 Esc 關閉也要釋放檔案（否則之後移到資源回收筒會顯示「正在使用中」）
  $<HTMLDialogElement>("editor").addEventListener("close", () => {
    v.pause();
    if (v.getAttribute("src")) {
      v.removeAttribute("src");
      v.load();
    }
  });
  $("edPlay").addEventListener("click", () => {
    if (previewing) return stopPreview();
    v.paused ? void v.play() : v.pause();
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
    render();
  });
  $("edSave").addEventListener("click", async () => {
    if (!entry) return;
    const btn = $<HTMLButtonElement>("edSave");
    btn.disabled = true;
    try {
      await deps.save(entry.path, { ...spec, crop: cropOn ? normalizeCrop(spec.crop, vw, vh) : undefined });
      close();
    } catch (err) {
      deps.toast((err as Error).message, true);
      btn.disabled = false;
    }
  });

  // 時間軸：點一下跳到該時間；拖曳把手調整頭尾、拖曳播放頭跳轉；在其他地方拖曳 = 選取要刪除的範圍
  const tl = $("edTimeline");
  let drag: { kind: "start" | "end" | "playhead" | "select"; x0: number; t0: number; moved: boolean } | undefined;
  const timeAt = (e: PointerEvent) => {
    const r = tl.getBoundingClientRect();
    return clamp(((e.clientX - r.left) / r.width) * duration, 0, duration);
  };
  tl.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || !duration) return;
    // 刪除片段上的 ×：還原
    if ((e.target as HTMLElement).closest("[data-remove]")) return;
    stopPreview();
    v.pause();
    const handle = (e.target as HTMLElement).closest<HTMLElement>("[data-handle]")?.dataset.handle as "start" | "end" | "playhead" | undefined;
    drag = { kind: handle ?? "select", x0: e.clientX, t0: timeAt(e), moved: false };
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
    if (drag.kind === "select" && !drag.moved) {
      sel = undefined;
      seek(timeAt(e));
      render();
    }
    drag = undefined;
  };
  tl.addEventListener("pointerup", endDrag);
  tl.addEventListener("pointercancel", endDrag);
  tl.addEventListener("click", (e) => restore(e));

  // 裁切：在影片上拖曳框選（未開啟裁切時，點影片 = 播放 / 暫停）
  const stage = $("edStage");
  let dragFrom: { x: number; y: number } | undefined;
  const toVideo = (e: PointerEvent) => {
    const r = stage.getBoundingClientRect();
    return { x: clamp((e.clientX - r.left) / r.width, 0, 1) * vw, y: clamp((e.clientY - r.top) / r.height, 0, 1) * vh };
  };
  stage.addEventListener("pointerdown", (e) => {
    if (!cropOn || !vw) {
      v.paused ? void v.play() : v.pause();
      return;
    }
    dragFrom = toVideo(e);
    stage.setPointerCapture(e.pointerId);
  });
  stage.addEventListener("pointermove", (e) => {
    if (!dragFrom) return;
    const p = toVideo(e);
    spec.crop = { x: Math.min(dragFrom.x, p.x), y: Math.min(dragFrom.y, p.y), width: Math.abs(p.x - dragFrom.x), height: Math.abs(p.y - dragFrom.y) };
    render();
  });
  stage.addEventListener("pointerup", () => {
    if (!dragFrom || !spec.crop) return;
    dragFrom = undefined;
    setCrop(spec.crop);
  });
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

  // 快捷鍵：空白 = 播放/暫停、←/→ = 一張（Shift = 一秒）、I/O = 開頭/結尾、Delete = 刪除選取、Esc = 取消選取
  $<HTMLDialogElement>("editor").addEventListener("keydown", (e) => {
    if ((e.target as HTMLElement).matches("input, select, textarea")) return;
    const k = e.key.toLowerCase();
    if (k === " ") {
      if (previewing) stopPreview();
      else v.paused ? void v.play() : v.pause();
    }
    else if (k === "arrowleft") step(e.shiftKey ? "-1" : "-frame");
    else if (k === "arrowright") step(e.shiftKey ? "1" : "frame");
    else if (k === "i") setStart();
    else if (k === "o") setEnd();
    else if ((k === "delete" || k === "backspace" || k === "d") && sel) deleteSelection();
    else if (k === "escape" && sel) {
      // 有選取時 Esc 只取消選取，不關閉對話框
      sel = undefined;
      render();
    } else return;
    e.preventDefault();
  });
}
