import { clock, formatBytes, humanDuration, LIMITS, outputSize, speedLabel, videoClock } from "../shared/format.ts";
import type {
  DownloadStatus,
  EnvInfo,
  ExportStatus,
  LibraryEntry,
  LibraryPage,
  MethodPreference,
  MonitorInfo,
  RecordConfig,
  RecorderStatus,
  Rect,
  ScalePercent,
  UpdateInfo,
} from "../shared/types.ts";
import { HOTKEY_LABELS } from "../shared/types.ts";
import { $, api, baseName, clamp, esc, guarded, icon, isDefaultName, observeThumbs, setHtml, shortDate, thumbUrl, toast } from "./common.ts";
import { openEditor } from "./editor.ts";
import { openExport } from "./exportDialog.ts";
import { exportTag, isLibraryOpen, load as reloadLibraryDialog, openLibrary, type EntryAction } from "./libraryDialog.ts";

// ───────────── 版面縮放：以 1280×800 設計，較小的視窗等比縮小（最小 0.8 倍），較大則撐滿 ─────────────

function fitToWindow() {
  const z = clamp(Math.min(innerWidth / 1280, innerHeight / 800), 0.8, 1);
  document.documentElement.style.setProperty("--z", String(Math.round(z * 1000) / 1000));
  // 低於 0.8 倍仍放不下時才允許捲動，避免內容被裁掉
  document.body.style.overflow = innerWidth / 1280 < 0.8 || innerHeight / 800 < 0.8 ? "auto" : "hidden";
}
addEventListener("resize", fitToWindow);
fitToWindow();

// ───────────── 設定（伺服器 settings.json 為主，localStorage 為備援） ─────────────

interface Settings {
  sourceType: "monitor" | "all" | "region";
  monitorId?: string;
  region: Rect;
  fps: number;
  scale: ScalePercent;
  drawMouse: boolean;
  maxMinutes: number;
  method: MethodPreference;
  outputDir: string;
  speed: number;
  audioSystem: boolean;
  audioMic: boolean;
  /** 空字串 = 預設麥克風 */
  micId: string;
  keepAudio: boolean;
  livePreview: boolean;
  encoder: "auto" | "cpu" | "gpu";
  countdownSec: number;
  hideUi: boolean;
  exportFormat: "mp4" | "gif";
  exportMode: "speed" | "target";
  exportTarget: string;
  gifWidth: number;
  gifFps: number;
}

const STORAGE_KEY = "screen-recorder.settings.v1";
const FPS_CHOICES = [10, 15, 20, 24, 25, 30, 50, 60];
let serverUi: Partial<Settings> | undefined;
let settingsRev = 0;
let saveTimer: number | undefined;

function loadStored(): Partial<Settings> {
  if (serverUi) return serverUi;
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<Settings>;
  } catch {
    return {};
  }
}

function save() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(S));
  } catch {
    // 無痕模式等情況下無法儲存，不影響使用
  }
  // 同步到伺服器：換瀏覽器也記得，系統匣的「開始錄影」也用這份設定
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(async () => {
    saveTimer = undefined;
    try {
      const r = await api<{ rev: number }>("/api/settings", { ui: S, config: buildConfig() });
      settingsRev = r.rev;
    } catch {
      // 下次變更時再試
    }
  }, 300);
}

/** 其他地方（系統匣選單）改了設定時重新載入 */
async function reloadSettings() {
  try {
    const saved = await api<{ ui?: Partial<Settings>; rev: number }>("/api/settings");
    settingsRev = saved.rev;
    if (!saved.ui) return;
    serverUi = saved.ui;
    initSettings();
    renderSettings();
    renderSource();
  } catch {
    // 忽略，下次輪詢再試
  }
}

let env: EnvInfo;
let S: Settings;
let rec: RecorderStatus | undefined;
let exp: ExportStatus | undefined;
let statusAt = 0;
let lastResultPath: string | undefined;
let lastExportState: string | undefined;
let disconnected = false;
let stopped = false;

const locked = () => !!rec && rec.state !== "idle";
const monitors = (): MonitorInfo[] => env?.monitors ?? [];
const selectedMonitor = () => monitors().find((m) => m.id === S.monitorId);
function sourceRect(): Rect | undefined {
  if (S.sourceType === "monitor") {
    const m = selectedMonitor();
    return m && { x: m.x, y: m.y, width: m.width, height: m.height };
  }
  if (S.sourceType === "all") return env.desktop.width ? env.desktop : undefined;
  return S.region;
}

function initSettings() {
  const st = loadStored();
  const primary = monitors().find((m) => m.primary) ?? monitors()[0];
  S = {
    sourceType: st.sourceType === "region" || st.sourceType === "all" ? st.sourceType : "monitor",
    monitorId: st.monitorId,
    region: st.region ?? (primary ? { x: primary.x, y: primary.y, width: Math.round(primary.width / 2), height: Math.round(primary.height / 2) } : { x: 0, y: 0, width: 1280, height: 720 }),
    fps: Number.isInteger(st.fps) ? st.fps! : 30,
    scale: ([100, 75, 50, 25] as const).includes(st.scale as ScalePercent) ? st.scale! : 100,
    drawMouse: st.drawMouse ?? true,
    maxMinutes: Number.isFinite(st.maxMinutes) ? st.maxMinutes! : 0,
    method: (["auto", "ddagrab", "gdigrab"] as const).includes(st.method as MethodPreference) ? st.method! : "auto",
    outputDir: st.outputDir?.trim() || env.defaultOutputDir,
    speed: Number(st.speed) >= 1 ? Number(st.speed) : 4,
    audioSystem: st.audioSystem ?? false,
    audioMic: st.audioMic ?? false,
    micId: typeof st.micId === "string" ? st.micId : "",
    keepAudio: st.keepAudio ?? true,
    livePreview: st.livePreview ?? true,
    encoder: st.encoder === "cpu" || st.encoder === "gpu" ? st.encoder : "auto",
    countdownSec: [0, 3, 5, 10].includes(Number(st.countdownSec)) ? Number(st.countdownSec) : 3,
    hideUi: st.hideUi ?? true,
    exportFormat: st.exportFormat === "gif" ? "gif" : "mp4",
    exportMode: st.exportMode === "target" ? "target" : "speed",
    exportTarget: typeof st.exportTarget === "string" ? st.exportTarget : "1:00",
    gifWidth: [320, 480, 640, 960, 1280].includes(Number(st.gifWidth)) ? Number(st.gifWidth) : 640,
    gifFps: [5, 10, 15, 20].includes(Number(st.gifFps)) ? Number(st.gifFps) : 10,
  };
  if (!selectedMonitor()) S.monitorId = primary?.id;
}

function buildConfig(): RecordConfig {
  return {
    source: S.sourceType === "monitor"
      ? { type: "monitor", monitorId: S.monitorId ?? "" }
      : S.sourceType === "all"
        ? { type: "all" }
        : { type: "region", ...S.region },
    fps: S.fps,
    scale: S.scale,
    drawMouse: S.drawMouse,
    maxMinutes: S.maxMinutes,
    method: S.method,
    outputDir: S.outputDir.trim() || env.defaultOutputDir,
    audio: { system: S.audioSystem, mic: S.audioMic, micId: S.micId },
    encoder: S.encoder,
    countdownSec: S.countdownSec,
    hideUi: S.hideUi,
  };
}

const outDir = () => S.outputDir.trim() || env.defaultOutputDir;

// ───────────── 環境（FFmpeg / 螢幕） ─────────────

function renderEnv() {
  const ff = env.ffmpeg;
  const chips: string[] = [];
  if (env.update)
    chips.push(`<a class="chip new" href="${esc(env.update.url)}" target="_blank" rel="noopener" title="點一下前往下載頁面">有新版本 v${esc(env.update.version)}</a>`);
  // 正常的項目合併成一個「已就緒」（細節在滑鼠提示），有問題的才個別顯示：單行放得下、不會被截斷
  const ok: string[] = [];
  if (ff.found) ok.push(`FFmpeg ${(ff.version ?? "").split("-")[0]}（${ff.path ?? ""}）`);
  else chips.push(`<span class="chip bad">找不到 FFmpeg</span>`);
  if (ff.found) {
    if (!ff.hasDdagrab) chips.push(`<span class="chip warn" title="此 FFmpeg 不含 ddagrab，將使用 gdigrab">gdigrab</span>`);
    else if (ff.ddagrabWorks === undefined) chips.push(`<span class="chip">ddagrab 測試中…</span>`);
    else if (ff.ddagrabWorks) ok.push("擷取：ddagrab（GPU 擷取）");
    else chips.push(`<span class="chip warn" title="${esc(ff.ddagrabError ?? "")}">ddagrab 不可用 → gdigrab</span>`);
    if (ff.encoder) ok.push(`編碼：${ff.encoder}`);
    else chips.push(`<span class="chip bad">無 H.264 編碼器</span>`);
  }
  if (ok.length) chips.push(`<span class="chip ok" title="${esc(ok.join("\n"))}">${chips.some((c) => /chip (bad|warn)/.test(c)) ? `FFmpeg ${esc((ff.version ?? "").split("-")[0]!)}` : "已就緒"}</span>`);
  setHtml($("envChips"), chips.join(""));
  // 版本號顯示在視窗標題列（Chrome / Edge app 模式的標題就是頁面標題）
  document.title = `螢幕錄影 v${env.appVersion}`;
  $("ffmpegBanner").hidden = ff.found;
  $("appDirText").textContent = env.appDir;
  const warns = [ff.found ? ff.error : undefined, env.monitorError].filter(Boolean) as string[];
  $("envWarn").hidden = warns.length === 0;
  $("envWarn").textContent = warns.join("　");
}

async function refreshEnv(withPreview = true) {
  env = await api<EnvInfo>("/api/env/refresh", {});
  if (!selectedMonitor()) S.monitorId = (monitors().find((m) => m.primary) ?? monitors()[0])?.id;
  renderEnv();
  renderSource();
  renderAudio();
  if (withPreview) loadPreview();
  watchDdagrabTest();
  void loadRecent();
}

let ddagrabWatch: number | undefined;
function watchDdagrabTest() {
  clearInterval(ddagrabWatch);
  const pending = () => (env.ffmpeg.hasDdagrab && env.ffmpeg.ddagrabWorks === undefined) || (env.ffmpeg.found && !env.ffmpeg.hwEncoders);
  if (!pending()) return;
  ddagrabWatch = window.setInterval(async () => {
    try {
      env = await api<EnvInfo>("/api/env");
      renderEnv();
      renderSettings();
      if (!pending()) clearInterval(ddagrabWatch);
    } catch {
      clearInterval(ddagrabWatch);
    }
  }, 1000);
}

/** 自動下載 FFmpeg 的進度（顯示在找不到 FFmpeg 的提示區） */
let lastDlPhase = "";
function renderDownload(d: DownloadStatus | undefined) {
  if (!d || env.ffmpeg.found) return;
  const busy = d.phase === "downloading" || d.phase === "verifying" || d.phase === "extracting";
  $<HTMLButtonElement>("dlBtn").hidden = busy;
  $<HTMLButtonElement>("dlBtn").textContent = d.phase === "error" || d.phase === "canceled" ? "重新下載" : "自動下載";
  $("dlCancel").hidden = !busy;
  $("dlBar").hidden = !busy;
  const pct = d.total ? Math.min(100, (d.received / d.total) * 100) : 0;
  ($("dlBar").firstElementChild as HTMLElement).style.width = `${d.phase === "downloading" ? pct : 100}%`;
  const eta = d.speed && d.total ? (d.total - d.received) / d.speed : undefined;
  if (d.phase === "downloading")
    $("dlText").textContent = `下載中 ${formatBytes(d.received)}${d.total ? ` / ${formatBytes(d.total)}（${Math.floor(pct)}%）` : ""}` +
      (d.speed ? `・${formatBytes(d.speed)}/秒` : "") + (eta !== undefined ? `・剩約 ${humanDuration(eta)}` : "");
  else if (d.phase === "verifying") $("dlText").textContent = "比對 SHA-256 校驗碼…";
  else if (d.phase === "extracting") $("dlText").textContent = "解壓縮並確認 ffmpeg.exe 可以執行…";
  else if (d.phase === "error" || d.phase === "canceled") $("dlText").textContent = d.message ?? "下載失敗";
  if (d.phase === "done" && lastDlPhase !== "done") {
    // 伺服器已重新偵測，更新整個環境
    void refreshEnv(true).then(() => toast(d.message ?? "FFmpeg 已安裝"));
  }
  lastDlPhase = d.phase;
}

// ───────────── 擷取範圍 ─────────────

/** 目前連線中的即時預覽張數（0 = 靜態截圖） */
let liveFps = 0;
let liveFailed = false;
/** 目前預覽圖對應的範圍（"" = 整個桌面，否則為螢幕 id）；切換時重新擷取 */
let previewKey: string | undefined;

/** 預覽區顯示的範圍：單一螢幕模式只顯示選到的那一台，其餘顯示整個桌面（自訂範圍要能跨螢幕框選） */
function previewMonitor(): MonitorInfo | undefined {
  return S.sourceType === "monitor" ? selectedMonitor() : undefined;
}
function viewRect(): Rect {
  return previewMonitor() ?? env.desktop;
}

/**
 * 預覽：即時模式以 multipart JPEG 串流（平常每秒 5 張、錄影中 2 張以免搶資源）；
 * 視窗隱藏時中斷連線，串流失敗或關閉即時預覽時改用單張截圖。
 */
function loadPreview(forceStatic = false) {
  const img = $<HTMLImageElement>("deskImg");
  const empty = $("deskEmpty");
  if (!env.ffmpeg.found) {
    img.hidden = true;
    liveFps = 0;
    empty.textContent = "需要 FFmpeg 才能顯示預覽";
    empty.hidden = false;
    return;
  }
  if (document.hidden) {
    // 看不到就不必擷取：中斷串流，保留最後一張
    if (liveFps) {
      img.removeAttribute("src");
      liveFps = 0;
    }
    return;
  }
  const live = S.livePreview && !liveFailed && !forceStatic;
  const fps = live ? (locked() ? 2 : 5) : 0;
  const key = previewMonitor()?.id ?? "";
  if (live && fps === liveFps && key === previewKey && img.src) return; // 已在串流
  if (key !== previewKey) img.hidden = true; // 換了範圍：舊畫面比例不對，先藏起來
  liveFps = fps;
  previewKey = key;
  if (img.hidden) {
    empty.textContent = live ? "正在連接即時預覽…" : "正在擷取預覽…";
    empty.hidden = false;
  }
  img.onload = () => {
    img.hidden = false;
    empty.hidden = true;
  };
  img.onerror = () => {
    if (liveFps) {
      // 串流失敗（例如 ddagrab 暫時無法使用）：這次改用單張截圖
      liveFailed = true;
      liveFps = 0;
      loadPreview(true);
      return;
    }
    img.hidden = true;
    empty.textContent = "無法取得預覽（仍可錄影）";
    empty.hidden = false;
  };
  const q = `${key ? `monitor=${encodeURIComponent(key)}&` : ""}t=${Date.now()}`;
  img.src = live ? `/api/preview/live?fps=${fps}&${q}` : `/api/preview?${q}`;
}

document.addEventListener("visibilitychange", () => {
  if (!env) return;
  loadPreview();
  if (!document.hidden) void poll();
});
function renderSource() {
  const d = viewRect();
  const desk = $("desk");
  if (d.width > 0) {
    desk.style.setProperty("--ar", `${d.width} / ${d.height}`);
    desk.style.setProperty("--ar-num", String(d.width / d.height));
  }
  desk.classList.toggle("region-mode", S.sourceType === "region" && !locked());
  for (const b of $("sourceTabs").querySelectorAll<HTMLButtonElement>("button")) {
    b.setAttribute("aria-selected", String(b.dataset.source === S.sourceType));
    b.disabled = locked();
  }

  const pct = (v: number, total: number) => `${(v / total) * 100}%`;
  const shown = previewMonitor();
  setHtml($("deskOverlay"), d.width
    ? (shown ? [shown] : monitors())
        .map(
          (m) => `<div class="mon-box${(S.sourceType === "monitor" && m.id === S.monitorId) || S.sourceType === "all" ? " selected" : ""}" data-id="${m.id}"
            style="left:${pct(m.x - d.x, d.width)};top:${pct(m.y - d.y, d.height)};width:${pct(m.width, d.width)};height:${pct(m.height, d.height)}">
            <div>螢幕 ${m.displayNumber}<small>${m.width} × ${m.height}</small></div></div>`,
        )
        .join("")
    : "");
  const region = $("deskRegion");
  region.hidden = S.sourceType !== "region" || !d.width;
  if (!region.hidden) {
    const r = S.region;
    Object.assign(region.style, { left: pct(r.x - d.x, d.width), top: pct(r.y - d.y, d.height), width: pct(r.width, d.width), height: pct(r.height, d.height) });
  }
  renderSourceDetail();
  renderSizeText();
  if (previewKey !== undefined && previewKey !== (shown?.id ?? "")) loadPreview();
}

/** 範圍分頁旁的細節：螢幕按鈕 / 座標輸入 / 說明文字 */
let detailMode = "";
function renderSourceDetail() {
  const box = $("sourceDetail");
  const dis = locked() ? "disabled" : "";
  if (S.sourceType === "monitor") {
    detailMode = "monitor";
    setHtml(box, monitors().length
      ? monitors()
          .map((m) => `<button type="button" class="mon-chip" data-id="${m.id}" aria-pressed="${m.id === S.monitorId}" ${dis} title="${esc(m.adapterName)}">螢幕 ${m.displayNumber}<small>${m.width}×${m.height}${m.primary ? "・主" : ""}</small></button>`)
          .join("")
      : `<span class="info">找不到螢幕資訊，請改用「自訂範圍」</span>`);
  } else if (S.sourceType === "all") {
    detailMode = "all";
    const list = monitors();
    const multiGpu = new Set(list.map((m) => m.adapter)).size > 1;
    const text = list.length <= 1
      ? "目前只有 1 個螢幕，與「單一螢幕」相同"
      : `${list.length} 個螢幕拼成 ${env.desktop.width}×${env.desktop.height}` + (multiGpu ? "（不同顯示卡，將用 gdigrab）" : "");
    setHtml(box, `<span class="info${multiGpu ? " warn" : ""}" title="${esc(text)}">${esc(text)}</span>`);
  } else {
    // 座標輸入框：正在輸入時不要重建，避免游標跳走
    if (detailMode !== "region") {
      detailMode = "region";
      box.innerHTML = (["x", "y", "width", "height"] as const)
        .map((k) => `<label>${{ x: "X", y: "Y", width: "寬", height: "高" }[k]}<input type="number" data-region="${k}" step="1" /></label>`)
        .join("") + `<span class="info">在預覽圖上拖曳框選（可跨螢幕）</span>`;
    }
    for (const input of box.querySelectorAll<HTMLInputElement>("input[data-region]")) {
      if (document.activeElement !== input) input.value = String(S.region[input.dataset.region as keyof Rect]);
      input.disabled = locked();
    }
  }
}

function selectMonitor(id: string) {
  if (locked()) return;
  S.monitorId = id;
  save();
  renderSource();
}

function setupRegionDrag() {
  const desk = $("desk");
  let start: { x: number; y: number } | undefined;
  const toDesktop = (e: PointerEvent) => {
    const d = env.desktop;
    const r = desk.getBoundingClientRect();
    return {
      x: Math.round(d.x + clamp((e.clientX - r.left) / r.width, 0, 1) * d.width),
      y: Math.round(d.y + clamp((e.clientY - r.top) / r.height, 0, 1) * d.height),
    };
  };
  desk.addEventListener("pointerdown", (e) => {
    if (S.sourceType !== "region" || locked() || !env.desktop.width || e.button !== 0) return;
    start = toDesktop(e);
    desk.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  desk.addEventListener("pointermove", (e) => {
    if (!start) return;
    const p = toDesktop(e);
    S.region = { x: Math.min(start.x, p.x), y: Math.min(start.y, p.y), width: Math.max(1, Math.abs(p.x - start.x)), height: Math.max(1, Math.abs(p.y - start.y)) };
    renderSource();
  });
  const end = () => {
    if (!start) return;
    start = undefined;
    S.region.width = Math.max(16, S.region.width);
    S.region.height = Math.max(16, S.region.height);
    save();
    renderSource();
  };
  desk.addEventListener("pointerup", end);
  desk.addEventListener("pointercancel", end);
  $("deskOverlay").addEventListener("click", (e) => {
    const box = (e.target as HTMLElement).closest<HTMLElement>(".mon-box");
    if (box && S.sourceType === "monitor") selectMonitor(box.dataset.id!);
  });
}

// ───────────── 錄影設定（下方一列 + 下拉面板） ─────────────

function renderSizeText() {
  const r = sourceRect();
  const el = $("sizeText");
  if (!r) return void (el.textContent = "");
  const o = outputSize(r.width, r.height, S.scale);
  const big = o.width * o.height > 3840 * 2160 * 1.05;
  el.textContent = `輸出 ${o.width}×${o.height}` + (big ? "（很大，建議 50%）" : "");
  el.classList.toggle("warn", big);
}

function renderAudio() {
  const a = env.audio;
  $("renderName").textContent = a.render ? `（${a.render}）` : "（找不到播放裝置）";
  $<HTMLInputElement>("audioSystem").checked = S.audioSystem;
  $<HTMLInputElement>("audioMic").checked = S.audioMic;
  const sel = $<HTMLSelectElement>("micId");
  const def = a.captures.find((d) => d.isDefault);
  setHtml(sel, [
    `<option value="">預設麥克風${def ? `（${esc(def.name)}）` : ""}</option>`,
    ...a.captures.map((d) => `<option value="${esc(d.id)}">${esc(d.name)}</option>`),
  ].join(""));
  if (S.micId && !a.captures.some((d) => d.id === S.micId)) S.micId = "";
  sel.value = S.micId;
  sel.disabled = !S.audioMic || locked();
  $("audioHint").textContent = a.error ?? (S.audioSystem || S.audioMic
    ? "聲音與畫面以同一個時鐘對齊。系統聲音會受 Windows 音量影響。"
    : a.captures.length === 0 ? "找不到麥克風。" : "");
  const label = [S.audioSystem && "系統", S.audioMic && "麥克風"].filter(Boolean).join("＋") || "不錄";
  setHtml($("audioBtn"), `${icon(S.audioMic && !S.audioSystem ? "mic" : "speaker")}聲音：${label}${icon("down")}`);
}

function audioSummary(): string {
  const parts = [S.audioSystem && "系統聲音", S.audioMic && "麥克風"].filter(Boolean);
  return parts.length ? parts.join(" + ") : "不錄聲音";
}

function renderSettings() {
  const fps = $<HTMLSelectElement>("fps");
  const choices = FPS_CHOICES.includes(S.fps) ? FPS_CHOICES : [...FPS_CHOICES, S.fps].sort((a, b) => a - b);
  setHtml(fps, choices.map((f) => `<option value="${f}">${f}</option>`).join(""));
  fps.value = String(S.fps);
  $<HTMLSelectElement>("scale").value = String(S.scale);
  $<HTMLInputElement>("drawMouse").checked = S.drawMouse;
  $<HTMLInputElement>("liveToggle").checked = S.livePreview;
  const maxInput = $<HTMLInputElement>("maxMinutes");
  if (document.activeElement !== maxInput) maxInput.value = String(S.maxMinutes);
  $<HTMLSelectElement>("method").value = S.method;
  renderEncoder();
  $<HTMLSelectElement>("countdownSec").value = String(S.countdownSec);
  $<HTMLInputElement>("hideUi").checked = S.hideUi;
  renderHotkeys();
  const more = [
    S.maxMinutes > 0 ? `最長 ${humanDuration(S.maxMinutes * 60)}` : "不限時",
    S.countdownSec === 3 ? "" : S.countdownSec ? `倒數 ${S.countdownSec} 秒` : "不倒數",
    S.method === "auto" ? "" : S.method,
    S.encoder === "auto" ? "" : S.encoder === "gpu" ? "GPU 編碼" : "CPU 編碼",
  ].filter(Boolean).join("・");
  setHtml($("moreBtn"), `${esc(more)}${icon("down")}`);
  const dir = $<HTMLInputElement>("outputDir");
  if (document.activeElement !== dir) dir.value = S.outputDir;
  setHtml($("dirBtn"), `${icon("folder")}<span class="txt">${esc(outDir())}</span>`);
  $("dirBtn").title = `儲存位置：${outDir()}`;
  for (const el of $("settingsBar").querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLButtonElement>("select, input, button")) {
    if (el.id !== "openDirBtn") el.disabled = locked();
  }
  renderAudio();
  renderSizeText();
}

/** 「更多」面板的快捷鍵說明（被其他程式占用時提示） */
function renderHotkeys() {
  const hk = env.hotkeys;
  const line = (keys: string, what: string, ok?: boolean) =>
    `<div>${keys.split("+").map((k) => `<kbd>${esc(k)}</kbd>`).join("+")} ${what}${ok === false ? `<span class="warn">（已被其他程式使用）</span>` : ""}</div>`;
  setHtml($("hotkeyInfo"), hk
    ? line(HOTKEY_LABELS.record, "開始 / 停止錄影", hk.record) + line(HOTKEY_LABELS.pause, "暫停 / 繼續", hk.pause)
    : `<div>全域快捷鍵需要系統匣常駐時才能使用</div>`);
}

/** 編碼器選項：GPU 依實測結果顯示可用的編碼器 */
function renderEncoder() {
  const ff = env.ffmpeg;
  const hw = ff.hwEncoders;
  const gpuOpt = $<HTMLSelectElement>("encoder").querySelector<HTMLOptionElement>("option[value=gpu]")!;
  gpuOpt.textContent = hw === undefined ? "GPU（偵測中…）" : hw.length ? `GPU（${hw.join("、")}）` : "GPU（這台電腦沒有可用的）";
  gpuOpt.disabled = !hw?.length;
  if (S.encoder === "gpu" && hw && !hw.length) {
    // 這台電腦沒有可用的 GPU：改回自動並存檔，系統匣「開始錄影」才不會拿到無效的設定
    S.encoder = "auto";
    save();
  }
  $<HTMLSelectElement>("encoder").value = S.encoder;
  $("encoderHint").textContent = S.encoder !== "auto" ? ""
    : !hw?.length ? "這台電腦只能用 CPU 編碼。"
      : ff.preferGpu ? `先前偵測到 CPU 編碼跟不上，目前會使用 GPU（${hw[0]}）。`
        : `超過 1080p60 的畫面量，或偵測到 CPU 跟不上時，改用 ${hw[0]}。`;
  $("resetGpuBtn").hidden = !(S.encoder === "auto" && ff.preferGpu && hw?.length);
}

/** 下拉面板：點按鈕開關、點外面或按 Esc 關閉 */
function setupDrops() {
  const pairs = [["audioBtn", "audioPanel"], ["moreBtn", "morePanel"], ["dirBtn", "dirPanel"]] as const;
  const closeAll = (except?: string) => {
    for (const [, p] of pairs) if (p !== except) $(p).hidden = true;
  };
  for (const [b, p] of pairs) {
    $(b).addEventListener("click", (e) => {
      e.stopPropagation();
      const panel = $(p);
      closeAll(p);
      panel.hidden = !panel.hidden;
    });
    $(p).addEventListener("click", (e) => e.stopPropagation());
  }
  document.addEventListener("click", () => closeAll());
  document.addEventListener("keydown", (e) => e.key === "Escape" && closeAll());
}

function bindSettings() {
  $("sourceTabs").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-source]");
    if (!b || locked()) return;
    S.sourceType = b.dataset.source as Settings["sourceType"];
    save();
    renderSource();
  });
  $("sourceDetail").addEventListener("click", (e) => {
    const b = (e.target as HTMLElement).closest<HTMLButtonElement>(".mon-chip");
    if (b) selectMonitor(b.dataset.id!);
  });
  $("sourceDetail").addEventListener("input", (e) => {
    const input = (e.target as HTMLElement).closest<HTMLInputElement>("input[data-region]");
    if (!input) return;
    const v = Math.round(Number(input.value));
    if (!Number.isFinite(v)) return;
    S.region = { ...S.region, [input.dataset.region!]: v };
    save();
    renderSource();
  });
  const onChange = (id: string, fn: (el: HTMLInputElement) => void) =>
    $(id).addEventListener("change", (e) => {
      fn(e.target as HTMLInputElement);
      save();
      renderSettings();
    });
  onChange("fps", (el) => (S.fps = clamp(Math.round(Number(el.value)) || 30, LIMITS.fpsMin, LIMITS.fpsMax)));
  onChange("scale", (el) => (S.scale = Number(el.value) as ScalePercent));
  onChange("drawMouse", (el) => (S.drawMouse = el.checked));
  onChange("audioSystem", (el) => (S.audioSystem = el.checked));
  onChange("audioMic", (el) => (S.audioMic = el.checked));
  onChange("micId", (el) => (S.micId = el.value));
  onChange("method", (el) => (S.method = el.value as MethodPreference));
  onChange("encoder", (el) => (S.encoder = el.value as Settings["encoder"]));
  onChange("countdownSec", (el) => (S.countdownSec = Number(el.value)));
  onChange("hideUi", (el) => (S.hideUi = el.checked));
  $<HTMLInputElement>("checkUpdates").addEventListener("change", (e) =>
    guarded(() => api("/api/update", { enabled: (e.target as HTMLInputElement).checked })),
  );
  $("checkUpdateBtn").addEventListener("click", () =>
    guarded(async () => {
      const r = await api<{ update?: UpdateInfo }>("/api/update", { check: true }).catch((e: Error) => {
        renderUpdateStatus(e.message);
        throw e;
      });
      renderUpdateStatus();
      env.update = r.update;
      renderEnv();
      toast(r.update ? `有新版本 v${r.update.version}，點右下角的標籤前往下載` : `目前已是最新版本（v${env.appVersion}）`);
    }),
  );
  $("resetGpuBtn").addEventListener("click", () =>
    guarded(async () => {
      await api("/api/encoder/reset-learned", {});
      env.ffmpeg.preferGpu = false;
      renderSettings();
      toast("已重設，之後的錄影平常會用 CPU 編碼");
    }),
  );
  onChange("maxMinutes", (el) => {
    const v = Number(el.value);
    S.maxMinutes = Number.isFinite(v) && v > 0 ? Math.min(Math.round(v), LIMITS.maxMinutesMax) : 0;
  });
  let dirTimer: number | undefined;
  $<HTMLInputElement>("outputDir").addEventListener("input", (e) => {
    S.outputDir = (e.target as HTMLInputElement).value;
    save();
    $("dirBtn").innerHTML = `${icon("folder")}<span class="txt">${esc(outDir())}</span>`;
    clearTimeout(dirTimer);
    dirTimer = window.setTimeout(() => {
      recentPage = 1;
      void loadRecent();
    }, 600);
  });
  $("openDirBtn").addEventListener("click", () => guarded(() => api("/api/open", { action: "folder", path: outDir() })));
  $("refreshPreviewBtn").innerHTML = icon("refresh");
  $("refreshPreviewBtn").addEventListener("click", () => {
    liveFailed = false;
    liveFps = 0;
    void guarded(() => refreshEnv(true));
  });
  $<HTMLInputElement>("liveToggle").addEventListener("change", (e) => {
    S.livePreview = (e.target as HTMLInputElement).checked;
    liveFailed = false;
    save();
    loadPreview(!S.livePreview);
  });
  $("dlBtn").addEventListener("click", () => guarded(() => api("/api/ffmpeg/download", {})));
  $("dlCancel").addEventListener("click", () => guarded(() => api("/api/ffmpeg/cancel", {})));
  $("recheckBtn").addEventListener("click", () =>
    guarded(async () => {
      await refreshEnv(true);
      toast(env.ffmpeg.found ? `已找到 FFmpeg ${env.ffmpeg.version}` : "仍然找不到 ffmpeg.exe", !env.ffmpeg.found);
    }),
  );
  setupDrops();
}

// ───────────── 錄影狀態 ─────────────

const STATE_TEXT = { idle: "待命", countdown: "倒數中", recording: "錄影中", paused: "已暫停", stopping: "處理中" } as const;

function liveRecordedMs() {
  if (!rec) return 0;
  return rec.state === "recording" ? rec.recordedMs + (Date.now() - statusAt) : rec.recordedMs;
}

function renderTimer() {
  if (!rec || document.hidden) return; // 看不到就不必更新；回到前景的那一輪會補上
  const ms = liveRecordedMs();
  const text = clock(ms);
  if ($("timer").textContent !== text) $("timer").textContent = text;
  const cd = $("countdown");
  cd.hidden = rec.state !== "countdown";
  if (!cd.hidden) {
    const left = Math.max(0, (rec.countdownMs ?? 0) - (Date.now() - statusAt));
    const n = String(Math.max(1, Math.ceil(left / 1000)));
    if ($("countdownNum").textContent !== n) $("countdownNum").textContent = n;
  }
  const bar = $("maxBar");
  bar.hidden = !(rec.maxMs > 0 && rec.state !== "idle" && rec.state !== "countdown");
  if (!bar.hidden) (bar.firstElementChild as HTMLElement).style.width = `${Math.min(100, (ms / rec.maxMs) * 100)}%`;
}

const actionButtons = (path: string, opts: { edit?: boolean; export?: boolean } = { edit: true, export: true }) =>
  `<button class="btn small" data-act="play" data-path="${esc(path)}">${icon("play")}播放</button>
   <button class="btn small" data-act="reveal" data-path="${esc(path)}">${icon("folder")}顯示</button>
   ${opts.edit ? `<button class="btn small" data-act="edit" data-path="${esc(path)}">${icon("cut")}剪輯</button>` : ""}
   ${opts.export ? `<button class="btn small" data-act="export" data-path="${esc(path)}">${icon("fast")}加速</button>` : ""}`;

function renderRecorder() {
  if (!rec) return;
  const r = rec;
  const pill = $("statePill");
  pill.textContent = STATE_TEXT[r.state];
  pill.className = `state-pill ${r.state}`;
  let busy = r.busy ?? "";
  if (!busy && r.maxMs > 0 && r.state !== "idle") busy = `剩餘 ${clock(Math.max(0, r.maxMs - liveRecordedMs()))}`;
  $("busyText").textContent = busy;
  renderTimer();

  const active = r.state !== "idle";
  $("statVideo").textContent = videoClock(r.videoSec);
  $("statSize").textContent = active || r.bytes ? formatBytes(r.bytes) : "—";
  if (active) {
    $("statOut").textContent = `${r.outWidth}×${r.outHeight} · ${r.fps}fps`;
    $("statMethod").textContent = `${r.method ?? "—"}${r.tiles && r.tiles > 1 ? ` ×${r.tiles}` : ""} / ${r.encoder ?? "—"}`;
    $("statAudio").textContent = r.audio ?? "不錄聲音";
  } else {
    const src = sourceRect();
    const o = src ? outputSize(src.width, src.height, S.scale) : undefined;
    $("statOut").textContent = o ? `${o.width}×${o.height} · ${S.fps}fps` : "—";
    const encLabel = S.encoder === "gpu" ? env.ffmpeg.hwEncoders?.[0] ?? "GPU" : S.encoder === "cpu" ? env.ffmpeg.encoder ?? "CPU" : "自動";
    $("statMethod").textContent = env.ffmpeg.encoder ? `${S.method === "auto" ? "自動" : S.method} / ${encLabel}` : "—";
    $("statAudio").textContent = audioSummary();
  }

  const start = $<HTMLButtonElement>("startBtn");
  start.hidden = active;
  start.disabled = !env.ffmpeg.found || !env.ffmpeg.encoder || exp?.state === "running";
  start.title = exp?.state === "running" ? "轉檔進行中，完成後才能錄影" : "";
  $("pauseBtn").hidden = r.state !== "recording";
  $("resumeBtn").hidden = r.state !== "paused";
  const stop = $<HTMLButtonElement>("stopBtn");
  stop.hidden = !active;
  stop.disabled = r.state === "stopping";
  setHtml(stop, r.state === "countdown" ? `${icon("stop")}取消倒數` : `${icon("stop")}停止`);
  const hk = env.hotkeys?.record ? `，或按 ${HOTKEY_LABELS.record} 取消` : "";
  $("countdownHint").textContent = `即將開始錄影${S.hideUi ? "，這個視窗若在錄影範圍內會自動縮小" : ""}${hk}`;
  $<HTMLButtonElement>("pauseBtn").disabled = !!r.busy && r.state !== "recording";
  $<HTMLButtonElement>("resumeBtn").disabled = !!r.busy;

  const msgs: string[] = [];
  if (r.retrying) msgs.push(r.retrying);
  if (r.slow) msgs.push(`電腦跟不上：實際約 ${r.actualFps?.toFixed(1) ?? "?"} fps（設定 ${r.fps}），建議調低解析度或 FPS`);
  $("recNotice").hidden = msgs.length === 0;
  $("recNotice").textContent = msgs.join("　");

  const box = $("recResult");
  const res = r.result;
  box.hidden = active || !res;
  if (res && !active) {
    box.className = `result ${res.ok ? "ok" : "bad"}`;
    setHtml(box, res.ok && res.path
      ? `<strong>錄影已儲存・${videoClock(res.videoSec)}・${formatBytes(res.bytes ?? 0)}</strong>
         <div class="path" title="${esc(res.path)}">${esc(res.path.split(/[\\/]/).pop()!)}</div>
         <div class="actions">${actionButtons(res.path)}</div>`
      : `<strong>錄影未完成</strong><div class="small">${esc(res.message)}</div>`);
  }

  setHtml($("logList"), r.log
    .slice(-20)
    .reverse()
    .map((l) => `<li class="${l.level}"><time>${new Date(l.t).toLocaleTimeString("zh-TW", { hour12: false })}</time><span title="${esc(l.text)}">${esc(l.text)}</span></li>`)
    .join(""));

  if (res?.ok && res.path && res.path !== lastResultPath) {
    lastResultPath = res.path;
    void loadRecent();
  }
}

let dismissedJob: number | undefined;

/** 右側「工作」卡：加速匯出 / 剪輯的進度 */
function renderJob() {
  const card = $("jobCard");
  card.hidden = !exp || (dismissedJob === exp.id && exp.state !== "running");
  if (!exp) return;
  const name = exp.output.split(/[\\/]/).pop() ?? "";
  card.className = `job ${exp.state}`;
  const pct = Math.floor(exp.progress * 100);
  const label = exp.kind === "cut" ? "剪輯" : exp.kind === "gif" ? `GIF${exp.speed > 1 ? ` ${speedLabel(exp.speed)}×` : ""} 匯出` : `加速 ${speedLabel(exp.speed)}×`;
  const title = { running: `${label}中`, done: `${label}完成`, error: `${label}失敗`, canceled: `${label}已取消` }[exp.state];
  $("jobTitle").textContent = title;
  $("jobMeta").textContent = exp.state === "running"
    ? `${pct}%${exp.etaSec !== undefined ? `・剩 ${humanDuration(exp.etaSec)}` : ""}`
    : exp.state === "done" ? `${videoClock(exp.expectedSec)}・${formatBytes(exp.bytes ?? 0)}` : "";
  $("jobBar").hidden = exp.state !== "running";
  ($("jobBar").firstElementChild as HTMLElement).style.width = `${pct}%`;
  setHtml($("jobActions"), exp.state === "running"
    ? `<span class="muted small job-name">${esc(name)}</span><button class="btn small" id="cancelJobBtn">取消</button>`
    : exp.state === "done"
      ? actionButtons(exp.output, { edit: exp.kind === "cut", export: exp.kind === "cut" }) + `<button class="btn ghost small" id="dismissJobBtn">關閉</button>`
      : `<span class="small">${esc(exp.message ?? "")}</span><button class="btn ghost small" id="dismissJobBtn">關閉</button>`);

  if (exp.state !== lastExportState) {
    const finished = lastExportState === "running" && exp.state !== "running";
    lastExportState = exp.state;
    if (finished) {
      void loadRecent();
      if (isLibraryOpen()) void reloadLibraryDialog();
      if (exp.state === "done") toast(exp.kind === "cut" ? "剪輯完成" : exp.kind === "gif" ? "GIF 匯出完成" : "加速版匯出完成");
    }
  }
}

function bindRecorder() {
  $("pauseBtn").innerHTML = `${icon("pause")}暫停`;
  $("resumeBtn").innerHTML = `${icon("play")}繼續`;

  $("startBtn").addEventListener("click", () => guarded(async () => applyStatus(await api("/api/record/start", buildConfig()))));
  $("pauseBtn").addEventListener("click", () => guarded(async () => applyStatus(await api("/api/record/pause", {}))));
  $("resumeBtn").addEventListener("click", () => guarded(async () => applyStatus(await api("/api/record/resume", {}))));
  $("countdownCancel").addEventListener("click", () => guarded(async () => applyStatus(await api("/api/record/stop", {}))));
  $("stopBtn").addEventListener("click", () =>
    guarded(async () => {
      ($("stopBtn") as HTMLButtonElement).disabled = true;
      applyStatus(await api("/api/record/stop", {}));
    }),
  );
  $("jobCard").addEventListener("click", (e) => {
    const id = (e.target as HTMLElement).closest("button")?.id;
    if (id === "cancelJobBtn") void guarded(async () => applyStatus(await api("/api/export/cancel", {})));
    if (id === "dismissJobBtn" && exp) {
      dismissedJob = exp.id;
      $("jobCard").hidden = true;
    }
  });
}

// ───────────── 最近錄影（橫向、分頁，不捲動） ─────────────

let recent: LibraryPage | undefined;
let recentPage = 1;
/** 已知的錄影（最近錄影 + 全部錄影對話框），讓操作按鈕找得到完整資訊 */
const known = new Map<string, LibraryEntry>();

function recentCount() {
  const w = $("recentCards").clientWidth;
  return clamp(Math.floor((w + 10) / 290), 1, 8);
}

// 縮圖產生失敗（例如檔案損壞）：改顯示空白底色，不要出現破圖示
document.addEventListener("error", (ev) => {
  const t = ev.target as HTMLElement;
  if (t instanceof HTMLImageElement && t.classList.contains("thumb")) t.classList.add("missing");
}, true);

let recentSeq = 0;
async function loadRecent() {
  if (!env) return;
  const seq = ++recentSeq;
  try {
    const q = new URLSearchParams({ dir: outDir(), page: String(recentPage), pageSize: String(recentCount()) });
    const r = await api<LibraryPage>(`/api/library?${q}`);
    if (seq !== recentSeq) return; // 較舊的請求晚回來（換頁、調整視窗大小時常見）：丟掉
    recent = r;
    recentPage = recent.page;
    for (const e of recent.items) known.set(e.path, e);
  } catch {
    if (seq !== recentSeq) return;
    recent = undefined;
  }
  renderRecent();
}

function renderRecent() {
  const items = recent?.items ?? [];
  setHtml($("recentCards"), items.length
    ? items
        .map((e) => {
          const tags = [
            e.hasAudio ? `<span class="tag audio">聲音</span>` : "",
            /_cut(_\d+)?\.mp4$/i.test(e.name) ? `<span class="tag cut">剪輯版</span>` : "",
            // 卡片放不下各個倍率，只顯示數量；倍率在滑鼠提示與「全部錄影」的子列
            e.exports.length
              ? `<span class="tag speed" title="${esc(`已匯出 ${e.exports.map(exportTag).join("、")}`)}">${e.exports.some((x) => x.format === "gif") ? "匯出" : "加速"} ${e.exports.length}</span>`
              : "",
          ].join("");
          return `<div class="rcard" title="${esc(e.name)}">
            <img class="thumb rcard-thumb" data-src="${thumbUrl(e)}" alt="" decoding="async" />
            <div class="rcard-body">
            <div class="rcard-top"><span class="rcard-date">${esc(isDefaultName(e.name) ? shortDate(e.name, e.mtime) : baseName(e.name))}</span><span>${tags}</span></div>
            <div class="rcard-meta">${e.durationSec !== undefined ? videoClock(e.durationSec) : "—"}・${formatBytes(e.bytes)}</div>
            <div class="rcard-actions">
              <button class="btn ghost" data-act="play" data-path="${esc(e.path)}" title="播放" aria-label="播放">${icon("play")}</button>
              <button class="btn ghost" data-act="reveal" data-path="${esc(e.path)}" title="在資料夾中顯示" aria-label="在資料夾中顯示">${icon("folder")}</button>
              <button class="btn ghost" data-act="edit" data-path="${esc(e.path)}" title="剪輯" aria-label="剪輯">${icon("cut")}</button>
              <button class="btn ghost rcard-export" data-act="export" data-path="${esc(e.path)}" title="加速匯出（MP4 / GIF）">${icon("fast")}匯出</button>
            </div>
            </div>
          </div>`;
        })
        .join("")
    : `<span class="recent-empty">「${esc(outDir())}」還沒有錄影，按「開始錄影」試試看。</span>`);
  observeThumbs($("recentCards"));
  const pages = recent?.pages ?? 1;
  $("recentInfo").textContent = recent && recent.total ? `${recentPage} / ${pages} 頁` : "";
  $<HTMLButtonElement>("recentPrev").disabled = recentPage <= 1;
  $<HTMLButtonElement>("recentNext").disabled = recentPage >= pages;
  setHtml($("libraryBtn"), `${icon("list")}全部錄影${recent?.total ? `（${recent.total}）` : ""}`);
}

function bindRecent() {
  $("recentPrev").innerHTML = icon("chevL");
  $("recentNext").innerHTML = icon("chevR");
  $("recentPrev").addEventListener("click", () => {
    recentPage--;
    void loadRecent();
  });
  $("recentNext").addEventListener("click", () => {
    recentPage++;
    void loadRecent();
  });
  $("libraryBtn").addEventListener("click", () =>
    openLibrary({
      dir: outDir,
      act: (a, e) => {
        known.set(e.path, e);
        act(a, e);
      },
      changed: () => void loadRecent(),
      toast,
    }),
  );
  // 寬度改變時重新計算一頁放幾張（與目前資料實際的每頁張數比較：第一次載入時版面可能還沒定型）
  let timer: number | undefined;
  new ResizeObserver(() => {
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      if (recent && recentCount() !== recent.pageSize) void loadRecent();
    }, 200);
  }).observe($("recentCards"));
}

// ───────────── 檔案操作（播放 / 顯示 / 剪輯 / 加速） ─────────────

async function findEntry(path: string): Promise<LibraryEntry | undefined> {
  const hit = known.get(path);
  if (hit?.durationSec !== undefined) return hit;
  // 剛錄好或剛轉檔完成的檔案可能還不在清單裡：重新讀取最近錄影
  recentPage = 1;
  await loadRecent();
  return known.get(path);
}

/** 播放 / 在資料夾中顯示：交給 Windows 開啟。播放器第一次啟動可能要一兩秒，先給回饋 */
function openFile(action: "play" | "reveal", path: string) {
  if (action === "play") toast("正在以預設播放器開啟…");
  void guarded(() => api("/api/open", { action, path }));
}

function act(action: EntryAction, entry: LibraryEntry) {
  if (action === "play" || action === "reveal") return openFile(action, entry.path);
  if (locked()) return toast(`錄影中無法${action === "edit" ? "剪輯" : "匯出"}，請先停止錄影`, true);
  if (exp?.state === "running") return toast("目前有轉檔工作進行中，請等它完成", true);
  if (!entry.durationSec) return toast("無法讀取影片長度", true);
  if (action === "edit") {
    openEditor(entry, {
      toast,
      save: async (source, spec) => {
        applyStatus(await api("/api/cut/start", { source, spec }));
        dismissedJob = undefined;
        toast("已開始剪輯，進度顯示在右側");
      },
    });
  } else {
    openExport(entry, {
      prefs: () => ({
        speed: S.speed,
        keepAudio: S.keepAudio,
        format: S.exportFormat,
        mode: S.exportMode,
        target: S.exportTarget,
        gifWidth: S.gifWidth,
        gifFps: S.gifFps,
      }),
      setPrefs: (p) => {
        if (p.speed !== undefined) S.speed = p.speed;
        if (p.keepAudio !== undefined) S.keepAudio = p.keepAudio;
        if (p.format) S.exportFormat = p.format;
        if (p.mode) S.exportMode = p.mode;
        if (p.target !== undefined) S.exportTarget = p.target;
        if (p.gifWidth) S.gifWidth = p.gifWidth;
        if (p.gifFps) S.gifFps = p.gifFps;
        save();
      },
      start: async (req) => {
        applyStatus(await api("/api/export/start", req));
        dismissedJob = undefined;
        toast("已開始匯出，進度顯示在右側");
      },
      toast,
    });
  }
}

document.addEventListener("click", (e) => {
  const el = (e.target as HTMLElement).closest<HTMLElement>("[data-act][data-path]");
  if (!el || el.closest("#libraryDlg")) return; // 全部錄影對話框自己處理
  const action = el.dataset.act as EntryAction;
  const path = el.dataset.path!;
  if (action === "play" || action === "reveal") return openFile(action, path);
  void findEntry(path).then((entry) => (entry ? act(action, entry) : toast("清單中找不到這個檔案", true)));
});

// ───────────── 輪詢 ─────────────

function applyStatus(data: { recorder: RecorderStatus; export?: ExportStatus; download?: DownloadStatus; settingsRev?: number; update?: string }) {
  renderDownload(data.download);
  if (env && data.update !== env.update?.version) void refreshUpdate();
  if (data.settingsRev && settingsRev && data.settingsRev !== settingsRev && saveTimer === undefined) void reloadSettings();
  const prevState = rec?.state;
  rec = data.recorder;
  exp = data.export;
  statusAt = Date.now();
  renderRecorder();
  renderJob();
  if (prevState !== rec.state) {
    renderSource();
    renderSettings();
    // 開始 / 結束錄影時調整即時預覽的張數
    if (prevState !== undefined && ((prevState === "idle") !== (rec.state === "idle"))) loadPreview();
  }
}

let pollTimer: number | undefined;
let polling = false;
function renderUpdateStatus(error?: string) {
  $("updateStatus").hidden = !error;
  $("updateStatus").textContent = error ? `無法檢查新版本：${error.replace(/^無法檢查新版本：/, "")}` : "";
}

/** 伺服器發現新版本（或狀態改變）時重新取得詳細資訊 */
async function refreshUpdate() {
  try {
    const r = await api<{ enabled: boolean; update?: UpdateInfo; error?: string }>("/api/update");
    env.update = r.update;
    $<HTMLInputElement>("checkUpdates").checked = r.enabled;
    renderUpdateStatus(r.error);
    renderEnv();
  } catch {
    // 下次輪詢再試
  }
}

async function poll() {
  if (polling) return; // 已有一輪在等回應，結束後自然會排下一輪
  clearTimeout(pollTimer);
  if (stopped) return;
  polling = true;
  try {
    applyStatus(await api("/api/status"));
    if (disconnected) {
      disconnected = false;
      toast("已重新連線");
    }
  } catch {
    if (!disconnected) {
      disconnected = true;
      toast("程式已結束或連線中斷，可以關閉這個視窗", true);
    }
  }
  polling = false;
  // 視窗隱藏（最小化 / 切到別的分頁）時放慢，回到前景時立即更新
  pollTimer = window.setTimeout(poll, document.hidden ? 3000 : 500);
}

// ───────────── 更新說明 ─────────────

/** CHANGELOG.md 的簡易轉換：## / ### 標題、- 清單、`程式碼`、[文字](網址)，其餘為段落 */
function renderChangelog(md: string): string {
  const inline = (s: string) =>
    esc(s)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, `<a href="$2" target="_blank" rel="noopener">$1</a>`);
  const out: string[] = [];
  let list = false;
  const closeList = () => list && (out.push("</ul>"), (list = false));
  for (const line of md.split(/\r?\n/)) {
    if (/^# /.test(line)) continue; // 頁面標題已在對話框上方
    const h = /^(#{2,3}) (.*)$/.exec(line);
    if (h) {
      closeList();
      const isCurrent = h[1] === "##" && h[2]!.startsWith(env.appVersion);
      out.push(`<h${h[1]!.length}>${inline(h[2]!)}${isCurrent ? `<span class="tag audio">目前版本</span>` : ""}</h${h[1]!.length}>`);
    } else if (/^- /.test(line)) {
      if (!list) (out.push("<ul>"), (list = true));
      out.push(`<li>${inline(line.slice(2))}</li>`);
    } else if (line.trim()) {
      closeList();
      out.push(`<p>${inline(line)}</p>`);
    } else closeList();
  }
  closeList();
  return out.join("");
}

async function openChangelog() {
  const dlg = $<HTMLDialogElement>("changelogDlg");
  $("clVersion").textContent = `目前版本 ${env.appVersion}`;
  try {
    const md = await (await fetch("/api/changelog", { cache: "no-store" })).text();
    $("changelogBody").innerHTML = renderChangelog(md);
  } catch {
    $("changelogBody").textContent = "無法讀取更新說明";
  }
  dlg.showModal();
}

function bindChangelog() {
  // 系統匣選單「更新說明」會以 #changelog 開啟操作視窗
  // （視窗已開著時只會改 hash、不會重新載入，所以也要監聽 hashchange）
  const fromHash = () => {
    if (location.hash !== "#changelog") return;
    history.replaceState(null, "", location.pathname);
    void openChangelog();
  };
  fromHash();
  window.addEventListener("hashchange", fromHash);
  $("changelogDlg").querySelector("[data-close]")!.addEventListener("click", () => $<HTMLDialogElement>("changelogDlg").close());
}

// ───────────── 啟動 ─────────────

async function main() {
  env = await api<EnvInfo>("/api/env");
  try {
    const saved = await api<{ ui?: Partial<Settings>; rev: number }>("/api/settings");
    serverUi = saved.ui;
    settingsRev = saved.rev;
  } catch {
    // 讀不到就用 localStorage
  }
  initSettings();
  renderEnv();
  bindSettings();
  bindRecorder();
  bindRecent();
  bindChangelog();
  setupRegionDrag();
  renderSettings();
  renderSource();
  loadPreview();
  watchDdagrabTest();
  await loadRecent();
  void poll();
  void refreshUpdate();
  setInterval(renderTimer, 250);
}

main().catch((e) => toast(`初始化失敗：${(e as Error).message}`, true));
