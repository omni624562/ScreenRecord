/** 加速匯出對話框：選倍率、預覽輸出長度，送出後在右側「工作」區顯示進度。 */
import { exportFileName, formatBytes, humanDuration, LIMITS, SPEED_PRESETS, speedLabel, videoClock } from "../shared/format.ts";
import type { LibraryEntry } from "../shared/types.ts";
import { $, esc } from "./common.ts";

export interface ExportDeps {
  speed(): number;
  keepAudio(): boolean;
  setSpeed(v: number): void;
  setKeepAudio(v: boolean): void;
  start(source: string, speed: number, keepAudio: boolean): Promise<void>;
  toast(msg: string, error?: boolean): void;
}

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

function render() {
  if (!entry) return;
  const e = entry;
  const speed = deps.speed();
  $("exName").textContent = e.name;
  $("exInfo").textContent = [
    e.width && `${e.width}×${e.height}`,
    e.fps && `${e.fps} fps`,
    e.hasAudio ? "有聲音" : "無聲音",
    formatBytes(e.bytes),
  ].filter(Boolean).join("・");
  $("speedPresets").innerHTML = SPEED_PRESETS.map(
    (s) => `<button type="button" data-speed="${s}" aria-selected="${speed === s}">${s}×</button>`,
  ).join("");
  const sp = $<HTMLInputElement>("speed");
  if (document.activeElement !== sp) sp.value = speedLabel(speed);
  const dur = e.durationSec;
  $("calcFrom").textContent = dur ? videoClock(dur) : "—";
  $("calcTo").textContent = dur ? videoClock(dur / speed) : "—";
  $("calcHint").innerHTML = `${speedLabel(speed)}× 時，1 小時的錄影 ≈ ${humanDuration(3600 / speed)}；輸出 <span class="mono">${esc(exportFileName(e.name, speed))}</span>`;
  $("keepAudioRow").hidden = !e.hasAudio;
  $<HTMLInputElement>("keepAudio").checked = deps.keepAudio();
  $("exportBtn").textContent = `匯出 ${speedLabel(speed)}× 加速版`;
}

function bind() {
  bound = true;
  const dlg = $<HTMLDialogElement>("exportDlg");
  dlg.querySelector("[data-close]")!.addEventListener("click", () => dlg.close());
  $("speedPresets").addEventListener("click", (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLButtonElement>("button[data-speed]");
    if (!b) return;
    deps.setSpeed(Number(b.dataset.speed));
    render();
  });
  $<HTMLInputElement>("speed").addEventListener("input", (ev) => {
    const v = Number((ev.target as HTMLInputElement).value);
    if (!Number.isFinite(v) || v < LIMITS.speedMin || v > LIMITS.speedMax) return;
    deps.setSpeed(Math.round(v * 100) / 100);
    render();
  });
  $<HTMLInputElement>("keepAudio").addEventListener("change", (ev) => deps.setKeepAudio((ev.target as HTMLInputElement).checked));
  $("exportBtn").addEventListener("click", async () => {
    if (!entry) return;
    const btn = $<HTMLButtonElement>("exportBtn");
    btn.disabled = true;
    try {
      await deps.start(entry.path, deps.speed(), deps.keepAudio());
      dlg.close();
    } catch (err) {
      deps.toast((err as Error).message, true);
    } finally {
      btn.disabled = false;
    }
  });
}
