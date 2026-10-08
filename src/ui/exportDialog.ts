/**
 * 製作加速版 / GIF 對話框：MP4 或 GIF；加速方式可「指定倍率」或「指定長度」（自動換算倍率），
 * 送出後在右側「工作」區顯示進度。
 */
import {
  exportFileName,
  formatBytes,
  humanDuration,
  LIMITS,
  parseClock,
  SPEED_PRESETS,
  speedForTarget,
  speedLabel,
  videoClock,
  type ExportFormat,
} from "../shared/format.ts";
import type { LibraryEntry } from "../shared/types.ts";
import { $, esc } from "./common.ts";

export interface ExportPrefs {
  speed: number;
  keepAudio: boolean;
  format: ExportFormat;
  mode: "speed" | "target";
  /** 指定長度模式的輸入（例如 "1:00"） */
  target: string;
  gifWidth: number;
  gifFps: number;
}

export interface ExportRequest {
  source: string;
  speed: number;
  keepAudio: boolean;
  format: ExportFormat;
  gifWidth: number;
  gifFps: number;
}

export interface ExportDeps {
  prefs(): ExportPrefs;
  setPrefs(p: Partial<ExportPrefs>): void;
  start(req: ExportRequest): Promise<void>;
  toast(msg: string, error?: boolean): void;
}

const TARGET_PRESETS = ["0:30", "1:00", "3:00", "5:00"];

let deps: ExportDeps;
let entry: LibraryEntry | undefined;
let bound = false;

export function openExport(e: LibraryEntry, d: ExportDeps) {
  deps = d;
  entry = e;
  if (!bound) bind();
  render();
  $<HTMLDialogElement>("exportDlg").showModal();
}

/** 目前設定實際會用的倍率；指定長度但輸入無效時回傳 undefined */
function effectiveSpeed(): number | undefined {
  const p = deps.prefs();
  const gif = p.format === "gif";
  if (p.mode === "speed") return gif ? Math.max(1, p.speed) : Math.max(LIMITS.speedMin, p.speed);
  const target = parseClock(p.target);
  const dur = entry?.durationSec;
  if (!target || !dur) return undefined;
  return speedForTarget(dur, target, gif);
}

function render() {
  if (!entry) return;
  const e = entry;
  const p = deps.prefs();
  const gif = p.format === "gif";
  const speed = effectiveSpeed();
  const dur = e.durationSec;

  $("exTitle").textContent = gif ? "製作 GIF" : "製作加速版";
  $("exName").textContent = e.name;
  $("exInfo").textContent = [
    e.width && `${e.width}×${e.height}`,
    e.fps && `${e.fps} fps`,
    e.hasAudio ? "有聲音" : "無聲音",
    formatBytes(e.bytes),
  ].filter(Boolean).join("・");
  for (const b of $("exFormat").querySelectorAll<HTMLButtonElement>("button")) b.setAttribute("aria-selected", String(b.dataset.format === p.format));
  for (const b of $("exMode").querySelectorAll<HTMLButtonElement>("button")) b.setAttribute("aria-selected", String(b.dataset.mode === p.mode));
  $("speedField").hidden = p.mode !== "speed";
  $("targetField").hidden = p.mode !== "target";

  // 倍率
  const presets = gif ? [1, ...SPEED_PRESETS] : SPEED_PRESETS;
  $("speedPresets").innerHTML = presets
    .map((s) => `<button type="button" data-speed="${s}" aria-selected="${p.speed === s}">${s === 1 ? "原速" : `${s}×`}</button>`)
    .join("");
  const sp = $<HTMLInputElement>("speed");
  sp.min = gif ? "1" : String(LIMITS.speedMin);
  if (document.activeElement !== sp) sp.value = speedLabel(p.speed);

  // 指定長度
  $("targetPresets").innerHTML = TARGET_PRESETS
    .map((t) => `<button type="button" data-target="${t}" aria-selected="${p.target === t}">${t}</button>`)
    .join("");
  const tl = $<HTMLInputElement>("targetLen");
  if (document.activeElement !== tl) tl.value = p.target;
  const target = parseClock(p.target);
  tl.classList.toggle("invalid", !target);
  $("targetHint").textContent = !target
    ? "請輸入長度，例如 1:00、90（秒）或 1:02:03"
    : !dur ? "無法讀取原片長度"
      : speed !== undefined && dur / speed > target + 0.5
        ? `最多只能加速到 ${speedLabel(LIMITS.speedMax)}×，實際約 ${videoClock(dur / speed)}`
        : speed !== undefined && dur / speed < target - 0.5
          ? `原片只有 ${videoClock(dur)}，${gif ? "以原速製作" : `以最低倍率 ${speedLabel(speed)}× 製作`}`
          : `需要加速 ${speedLabel(speed ?? 0)}×`;

  // GIF 選項
  $("gifOpts").hidden = !gif;
  $<HTMLSelectElement>("gifWidth").value = String(p.gifWidth);
  $<HTMLSelectElement>("gifFps").value = String(p.gifFps);

  // 換算結果
  $("calcFrom").textContent = dur ? videoClock(dur) : "—";
  $("calcTo").textContent = dur && speed ? videoClock(dur / speed) : "—";
  const outName = speed ? exportFileName(e.name, speed, p.format) : "—";
  const gifNote = gif ? "；GIF 沒有聲音，檔案較大，建議 1 分鐘以內" : "";
  $("calcHint").innerHTML = speed
    ? `${speed > 1 ? `${speedLabel(speed)}× 時，1 小時的錄影 ≈ ${humanDuration(3600 / speed)}${speed >= 8 ? "，適合做成縮時影片" : ""}；` : ""}存成 <span class="mono">${esc(outName)}</span>${gifNote}`
    : "";
  $("keepAudioRow").hidden = gif || !e.hasAudio;
  $<HTMLInputElement>("keepAudio").checked = p.keepAudio;
  const btn = $<HTMLButtonElement>("exportBtn");
  btn.disabled = !speed;
  btn.textContent = !speed ? "製作" : gif ? `製作 GIF${speed > 1 ? `（${speedLabel(speed)}×）` : ""}` : `製作 ${speedLabel(speed)}× 加速版`;
}

function bind() {
  bound = true;
  const dlg = $<HTMLDialogElement>("exportDlg");
  dlg.querySelector("[data-close]")!.addEventListener("click", () => dlg.close());
  const set = (p: Partial<ExportPrefs>) => {
    deps.setPrefs(p);
    render();
  };
  $("exFormat").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-format]");
    if (!b) return;
    const format = b.dataset.format as ExportFormat;
    // MP4 不能原速（那就是原檔）
    set(format === "mp4" && deps.prefs().speed < LIMITS.speedMin ? { format, speed: 4 } : { format });
  });
  $("exMode").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-mode]");
    if (b) set({ mode: b.dataset.mode as ExportPrefs["mode"] });
  });
  $("speedPresets").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-speed]");
    if (b) set({ speed: Number(b.dataset.speed) });
  });
  $<HTMLInputElement>("speed").addEventListener("input", (ev) => {
    const v = Number((ev.target as HTMLInputElement).value);
    const min = deps.prefs().format === "gif" ? 1 : LIMITS.speedMin;
    if (!Number.isFinite(v) || v < min || v > LIMITS.speedMax) return;
    set({ speed: Math.round(v * 100) / 100 });
  });
  $("targetPresets").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-target]");
    if (b) set({ target: b.dataset.target! });
  });
  $<HTMLInputElement>("targetLen").addEventListener("input", (ev) => set({ target: (ev.target as HTMLInputElement).value }));
  $<HTMLSelectElement>("gifWidth").addEventListener("change", (ev) => set({ gifWidth: Number((ev.target as HTMLSelectElement).value) }));
  $<HTMLSelectElement>("gifFps").addEventListener("change", (ev) => set({ gifFps: Number((ev.target as HTMLSelectElement).value) }));
  $<HTMLInputElement>("keepAudio").addEventListener("change", (ev) => deps.setPrefs({ keepAudio: (ev.target as HTMLInputElement).checked }));
  $("exportBtn").addEventListener("click", async () => {
    const speed = effectiveSpeed();
    if (!entry || !speed) return;
    const p = deps.prefs();
    const btn = $<HTMLButtonElement>("exportBtn");
    btn.disabled = true;
    try {
      await deps.start({ source: entry.path, speed, keepAudio: p.keepAudio, format: p.format, gifWidth: p.gifWidth, gifFps: p.gifFps });
      dlg.close();
    } catch (err) {
      deps.toast((err as Error).message, true);
    } finally {
      btn.disabled = false;
    }
  });
}
