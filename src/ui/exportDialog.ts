/**
 * 製作加速版 / GIF 對話框：MP4 或 GIF；加速方式可「指定倍率」或「指定長度」（自動換算倍率），
 * 送出後在右側「工作」區顯示進度。
 */
import {
  estimateBytes,
  exportFileName,
  formatBytes,
  humanDuration,
  LIMITS,
  MP4_WIDTHS,
  parseClock,
  scaledSize,
  SPEED_PRESETS,
  speedForTarget,
  speedLabel,
  videoClock,
  type ExportFormat,
} from "../shared/format.ts";
import type { LibraryEntry } from "../shared/types.ts";
import { $, baseName, esc, icon, isDefaultName, shortDate } from "./common.ts";

export interface ExportPrefs {
  speed: number;
  keepAudio: boolean;
  format: ExportFormat;
  mode: "speed" | "target";
  /** 指定長度模式的輸入（例如 "1:00"） */
  target: string;
  gifWidth: number;
  gifFps: number;
  /** 加速版縮小後的寬度（0 = 原尺寸） */
  mp4Width: number;
}

export interface ExportRequest {
  source: string;
  speed: number;
  keepAudio: boolean;
  format: ExportFormat;
  gifWidth: number;
  gifFps: number;
  mp4Width: number;
}

export interface ExportDeps {
  prefs(): ExportPrefs;
  setPrefs(p: Partial<ExportPrefs>): void;
  /** 播放已經做好的加速版 */
  play(path: string): void;
  start(req: ExportRequest): Promise<void>;
  toast(msg: string, error?: boolean): void;
}

const TARGET_PRESETS = ["0:30", "1:00", "3:00", "5:00"];
/** 加速後短於這個秒數時提醒可能太快 */
const TOO_SHORT_SEC = 3;

let deps: ExportDeps;
let entry: LibraryEntry | undefined;
let bound = false;

export function openExport(e: LibraryEntry, d: ExportDeps) {
  deps = d;
  entry = e;
  if (!bound) bind();
  render();
  $<HTMLDialogElement>("exportDlg").showModal();
  // 對話框預設把焦點放在第一個按鈕（「關閉」），看起來像主要按鈕；改放在製作鈕，按 Enter 即可開始
  $("exportBtn").focus();
}

/** 這支錄影已經有同格式、同倍率的成品 */
function existingExport(e: LibraryEntry, speed: number, format: ExportFormat) {
  return e.exports.find((x) => (x.format ?? "mp4") === format && Math.abs(x.speed - speed) < 0.005);
}

/** 實際會存成的檔名：同名已存在時伺服器會加上 _2、_3（與 uniquePath 相同規則） */
function outputName(e: LibraryEntry, speed: number, format: ExportFormat) {
  const name = exportFileName(e.name, speed, format);
  const taken = new Set(e.exports.map((x) => x.name.toLowerCase()));
  if (!taken.has(name.toLowerCase())) return name;
  const dot = name.lastIndexOf(".");
  for (let i = 2; ; i++) {
    const n = `${name.slice(0, dot)}_${i}${name.slice(dot)}`;
    if (!taken.has(n.toLowerCase())) return n;
  }
}

/** 目前設定的輸出尺寸（讀不到原片尺寸時 undefined） */
function outputDims(e: LibraryEntry, p: ExportPrefs) {
  if (!e.width || !e.height) return undefined;
  if (p.format === "mp4") return scaledSize(e.width, e.height, p.mp4Width);
  return scaledSize(e.width, e.height, p.gifWidth);
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
  // 與清單一致：預設檔名只顯示日期，改過名的顯示名稱；完整檔名在滑鼠提示
  $("exName").textContent = isDefaultName(e.name) ? `${shortDate(e.name, e.mtime)} 的錄影` : baseName(e.name);
  $("exName").title = e.name;
  // 尺寸與大小在下方「原片 → 加速後」裡
  $("exInfo").textContent = [e.fps && `${e.fps} fps`, e.hasAudio ? "有聲音" : "無聲音"].filter(Boolean).join("・");
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

  // 加速版尺寸：只列出比原片小的選項
  $("mp4Opts").hidden = gif || !e.width;
  if (e.width && e.height) {
    const opts = MP4_WIDTHS.filter((w) => w === 0 || w < e.width!);
    const sel = $<HTMLSelectElement>("mp4Width");
    sel.innerHTML = opts
      .map((w) => {
        const d = scaledSize(e.width!, e.height!, w);
        return `<option value="${w}">${w === 0 ? "原尺寸" : `寬 ${w}`}（${d.width}×${d.height}）</option>`;
      })
      .join("");
    sel.value = String((opts as number[]).includes(p.mp4Width) ? p.mp4Width : 0);
  }

  // 換算結果：長度、尺寸、預估大小
  const dims = outputDims(e, p);
  $("calcFrom").textContent = dur ? videoClock(dur) : "—";
  $("calcTo").textContent = dur && speed ? videoClock(dur / speed) : "—";
  $("calcFromInfo").textContent = [e.width && `${e.width}×${e.height}`, formatBytes(e.bytes)].filter(Boolean).join("・");
  let toInfo = "";
  if (dims && dur && speed) {
    const [lo, hi] = estimateBytes({
      format: p.format, srcBytes: e.bytes, srcSec: dur, srcWidth: e.width!, srcHeight: e.height!,
      speed, width: dims.width, height: dims.height, gifFps: p.gifFps,
    });
    toInfo = `${dims.width}×${dims.height}・約 ${formatBytes(lo)}～${formatBytes(hi)}`;
  }
  $("calcToInfo").textContent = toInfo;

  // 提醒：已有同倍率的成品、加速後太短
  const existing = speed ? existingExport(e, speed, p.format) : undefined;
  const outName = speed ? outputName(e, speed, p.format) : "—";
  const what = gif ? `GIF${speed && speed > 1 ? ` ${speedLabel(speed)}×` : "（原速）"}` : `${speedLabel(speed ?? 0)}× 加速版`;
  const notes: string[] = [];
  if (existing)
    notes.push(`<div class="ex-note"><span>已經有 ${esc(what)}（${existing.durationSec !== undefined ? `${videoClock(existing.durationSec)}・` : ""}${formatBytes(existing.bytes)}）。再製作會另存一份。</span><button type="button" class="btn small" data-play-existing="${esc(existing.path)}">${icon("play")}播放現有的</button></div>`);
  if (dur && speed && dur / speed < TOO_SHORT_SEC && speed > 1)
    notes.push(`<div class="ex-note warn"><span>加速後只有 ${(dur / speed).toFixed(1)} 秒，可能太快看不清楚。${p.mode === "speed" ? "可以改用「指定長度」。" : ""}</span>${p.mode === "speed" ? `<button type="button" class="btn small" data-use-target>改用指定長度</button>` : ""}</div>`);
  $("exNotes").innerHTML = notes.join("");
  const gifNote = gif ? "；GIF 沒有聲音，檔案較大，建議 1 分鐘以內" : "";
  $("calcHint").innerHTML = speed
    ? `${speed > 1 ? `${speedLabel(speed)}× 時，1 小時的錄影 ≈ ${humanDuration(3600 / speed)}${speed >= 8 ? "，適合做成縮時影片" : ""}；` : ""}存成 <span class="mono">${esc(outName)}</span>${gifNote}`
    : "";
  $("keepAudioRow").hidden = gif || !e.hasAudio;
  $<HTMLInputElement>("keepAudio").checked = p.keepAudio;
  const btn = $<HTMLButtonElement>("exportBtn");
  btn.disabled = !speed;
  const label = !speed ? "製作" : gif ? `製作 GIF${speed > 1 ? `（${speedLabel(speed)}×）` : ""}` : `製作 ${speedLabel(speed)}× 加速版`;
  btn.textContent = existing ? label.replace(/^製作/, "再製作一份") : label;
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
  $<HTMLSelectElement>("mp4Width").addEventListener("change", (ev) => set({ mp4Width: Number((ev.target as HTMLSelectElement).value) }));
  $("exNotes").addEventListener("click", (ev) => {
    const t = ev.target as HTMLElement;
    const play = t.closest<HTMLElement>("[data-play-existing]");
    if (play) return deps.play(play.dataset.playExisting!);
    if (t.closest("[data-use-target]")) {
      // 改用指定長度：預設 1:00，原片不到 1 分鐘時用原片長度的一半（至少 5 秒）
      const dur = entry?.durationSec ?? 0;
      const half = Math.max(5, Math.round(dur / 2));
      const target = dur && dur < 60 ? `0:${String(half).padStart(2, "0")}` : "1:00";
      set({ mode: "target", target });
    }
  });
  $<HTMLInputElement>("keepAudio").addEventListener("change", (ev) => deps.setPrefs({ keepAudio: (ev.target as HTMLInputElement).checked }));
  $("exportBtn").addEventListener("click", async () => {
    const speed = effectiveSpeed();
    if (!entry || !speed) return;
    const p = deps.prefs();
    const btn = $<HTMLButtonElement>("exportBtn");
    btn.disabled = true;
    try {
      await deps.start({ source: entry.path, speed, keepAudio: p.keepAudio, format: p.format, gifWidth: p.gifWidth, gifFps: p.gifFps, mp4Width: p.mp4Width });
      dlg.close();
    } catch (err) {
      deps.toast((err as Error).message, true);
    } finally {
      btn.disabled = false;
    }
  });
}
