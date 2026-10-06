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
let pendingCut: number | undefined;
let cropOn = false;
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
  pendingCut = undefined;
  cropOn = false;
  $("edName").textContent = e.name;
  const v = video();
  v.src = `/api/media?path=${encodeURIComponent(e.path)}`;
  v.currentTime = 0;
  setAspect();
  render();
  $<HTMLDialogElement>("editor").showModal();
}

/** 外框比例 = 影片比例（CSS 用 --ar / --ar-num 同時限制寬高） */
function setAspect() {
  const st = $("edStage").style;
  if (!vw || !vh) return;
  st.setProperty("--ar", `${vw} / ${vh}`);
  st.setProperty("--ar-num", String(vw / vh));
}

function close() {
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

  const markBtn = $("edMarkCut");
  markBtn.textContent = pendingCut === undefined ? "標記刪除起點" : `標記刪除終點（起點 ${videoClock(pendingCut)}）`;
  $("edCancelCut").hidden = pendingCut === undefined;
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
  const keep = keepRanges(duration, spec);
  const parts: string[] = [];
  // 先畫整條為「刪除」，再蓋上保留段
  parts.push(`<div class="ed-seg cut" style="left:0;width:100%"></div>`);
  for (const [a, b] of keep) parts.push(`<div class="ed-seg keep" style="left:${pct(a)};width:${pct(b - a)}"></div>`);
  if (pendingCut !== undefined) {
    const t = now();
    const [a, b] = [Math.min(pendingCut, t), Math.max(pendingCut, t)];
    parts.push(`<div class="ed-seg pending" style="left:${pct(a)};width:${pct(Math.max(b - a, duration / 1000))}"></div>`);
  }
  $("edTrack").innerHTML = parts.join("");
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

function setStart() {
  spec.start = Math.min(now(), spec.end - 0.1);
  render();
}
function setEnd() {
  spec.end = Math.max(now(), spec.start + 0.1);
  render();
}
function markCut() {
  const t = now();
  if (pendingCut === undefined) pendingCut = t;
  else {
    const r: Range = [Math.min(pendingCut, t), Math.max(pendingCut, t)];
    pendingCut = undefined;
    if (r[1] - r[0] < 0.05) deps.toast("刪除的片段太短，請移到另一個時間點再標記終點", true);
    else spec.removed = normalizeRanges([...spec.removed, r], duration);
  }
  render();
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
  for (const ev of ["timeupdate", "seeked", "play", "pause"]) v.addEventListener(ev, () => {
    renderTime();
    if (pendingCut !== undefined) renderTimeline();
  });
  // 播放中用 rAF 讓播放頭平順移動
  const loop = () => {
    if (!v.paused) renderTime();
    requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);

  $("edClose").addEventListener("click", close);
  $<HTMLDialogElement>("editor").addEventListener("close", () => {
    v.pause();
  });
  $("edPlay").addEventListener("click", () => (v.paused ? void v.play() : v.pause()));
  for (const b of document.querySelectorAll<HTMLButtonElement>("[data-step]")) b.addEventListener("click", () => step(b.dataset.step!));
  $("edSetStart").addEventListener("click", setStart);
  $("edSetEnd").addEventListener("click", setEnd);
  $("edMarkCut").addEventListener("click", markCut);
  $("edCancelCut").addEventListener("click", () => {
    pendingCut = undefined;
    render();
  });
  $("edRemoved").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-remove]");
    if (!b) return;
    const list = normalizeRanges(spec.removed, duration);
    list.splice(Number(b.dataset.remove), 1);
    spec.removed = list;
    render();
  });
  $("edReset").addEventListener("click", () => {
    spec = { start: 0, end: duration, removed: [] };
    pendingCut = undefined;
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

  // 時間軸：點選 / 拖曳跳轉
  const tl = $("edTimeline");
  let scrubbing = false;
  const scrub = (e: PointerEvent) => {
    const r = tl.getBoundingClientRect();
    seek(((e.clientX - r.left) / r.width) * duration);
  };
  tl.addEventListener("pointerdown", (e) => {
    scrubbing = true;
    tl.setPointerCapture(e.pointerId);
    v.pause();
    scrub(e);
  });
  tl.addEventListener("pointermove", (e) => scrubbing && scrub(e));
  tl.addEventListener("pointerup", () => (scrubbing = false));

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

  // 快捷鍵：空白 = 播放/暫停、←/→ = 一張（Shift = 一秒）、I/O = 開頭/結尾、D = 標記刪除
  $<HTMLDialogElement>("editor").addEventListener("keydown", (e) => {
    if ((e.target as HTMLElement).matches("input, select, textarea")) return;
    const k = e.key.toLowerCase();
    if (k === " ") v.paused ? void v.play() : v.pause();
    else if (k === "arrowleft") step(e.shiftKey ? "-1" : "-frame");
    else if (k === "arrowright") step(e.shiftKey ? "1" : "frame");
    else if (k === "i") setStart();
    else if (k === "o") setEnd();
    else if (k === "d") markCut();
    else return;
    e.preventDefault();
  });
}
