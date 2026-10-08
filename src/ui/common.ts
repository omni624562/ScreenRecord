/** 前端共用工具：DOM、API、提示訊息、圖示。 */

export const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

/** 縮圖網址（已跳脫，可直接放進 HTML 屬性）：帶修改時間，檔案變了網址就不同，瀏覽器可放心快取 */
export const thumbUrl = (e: { path: string; mtime: number }) => `/api/thumb?path=${encodeURIComponent(e.path)}&amp;v=${Math.round(e.mtime)}`;

/**
 * 縮圖依序載入：看得到的才載入、同時最多 2 張。瀏覽器對同一主機最多 6 條連線，
 * 一次丟出幾十張縮圖請求（第一次要等 FFmpeg 產生）會讓狀態輪詢、按鈕操作排在後面卡住。
 * 圖片用 data-src 標記，畫完 HTML 後呼叫 observeThumbs()。
 */
const THUMB_PARALLEL = 2;
const thumbQueue: HTMLImageElement[] = [];
let thumbActive = 0;
const thumbObserver = new IntersectionObserver(
  (entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      thumbObserver.unobserve(e.target);
      thumbQueue.push(e.target as HTMLImageElement);
    }
    pumpThumbs();
  },
  { rootMargin: "120px" },
);
function pumpThumbs() {
  while (thumbActive < THUMB_PARALLEL && thumbQueue.length) {
    const img = thumbQueue.shift()!;
    if (!img.isConnected || img.getAttribute("src")) continue; // 已被重畫掉或已載入
    thumbActive++;
    const done = () => {
      thumbActive--;
      pumpThumbs();
    };
    img.addEventListener("load", done, { once: true });
    img.addEventListener("error", done, { once: true });
    img.src = img.dataset.src!;
  }
}
export function observeThumbs(root: ParentNode) {
  for (const img of root.querySelectorAll<HTMLImageElement>("img.thumb[data-src]:not([src])")) thumbObserver.observe(img);
}

const lastHtml = new WeakMap<Element, string>();
/**
 * 內容有變才重建。輪詢每 0.5 秒更新一次狀態，若每次都重設 innerHTML，
 * 按鈕會在按下與放開之間被換掉，點擊就不會生效（也會讓 hover 閃動）。
 */
export function setHtml(el: Element, html: string) {
  if (lastHtml.get(el) === html) return;
  lastHtml.set(el, html);
  el.innerHTML = html;
}

export const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

export async function api<T = any>(path: string, body?: unknown): Promise<T> {
  const init: RequestInit = body === undefined
    ? { cache: "no-store" }
    : { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
  const res = await fetch(path, init);
  const data = await res.json().catch(() => ({}));
  if (!res.ok || data?.ok === false) throw new Error(data?.error ?? `HTTP ${res.status}`);
  return data as T;
}

let toastTimer: number | undefined;
export function toast(msg: string, error = false) {
  const el = $("toast");
  el.textContent = msg;
  el.classList.toggle("error", error);
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), error ? 6000 : 3000);
}

export async function guarded(fn: () => Promise<unknown>) {
  try {
    await fn();
  } catch (e) {
    toast((e as Error).message, true);
  }
}

/** 由檔名 Rec_2026-10-06_08-17-18 取出「10/06 08:17」；不符合格式時用修改時間 */
export function shortDate(name: string, mtime: number, withSec = false): string {
  const m = /(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})(?:-(\d{2}))?/.exec(name);
  if (m) return `${m[2]}/${m[3]} ${m[4]}:${m[5]}${withSec && m[6] ? `:${m[6]}` : ""}`;
  const d = new Date(mtime);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}${withSec ? `:${p(d.getSeconds())}` : ""}`;
}

/** 一組錄影的日期標籤：同一分鐘的有好幾支時加上秒數，才分得出來 */
export function dateLabels(items: { name: string; mtime: number }[]): Map<string, string> {
  const count = new Map<string, number>();
  for (const e of items) {
    const k = shortDate(e.name, e.mtime);
    count.set(k, (count.get(k) ?? 0) + 1);
  }
  return new Map(items.map((e) => {
    const k = shortDate(e.name, e.mtime);
    return [e.name, (count.get(k) ?? 0) > 1 ? shortDate(e.name, e.mtime, true) : k];
  }));
}

/** 程式產生的預設檔名（Rec_日期時間，可能帶 _2、_cut）：只看日期就夠，不必再列出檔名 */
export const isDefaultName = (name: string) => /^Rec_\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}(_\d+)?(_cut(_\d+)?)?\.mp4$/i.test(name);
/** 顯示用名稱：去掉 .mp4 */
export const baseName = (name: string) => name.replace(/\.mp4$/i, "");

/** 線條圖示（currentColor） */
const path = {
  play: '<path d="M5 3.5v9l7.5-4.5z" fill="currentColor" stroke="none"/>',
  cut: '<circle cx="4.5" cy="4.5" r="2"/><circle cx="4.5" cy="11.5" r="2"/><path d="M6.2 5.6 13.5 12M6.2 10.4 13.5 4"/>',
  /** 製作加速版 / GIF：方框加往右上的箭頭（⏩ 容易被看成快轉播放） */
  export: '<path d="M9 2.5h4.5V7M13.5 2.5 7.5 8.5"/><path d="M11.5 9.5v3a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1h3"/>',
  edit: '<path d="M10.5 2.5l3 3L6 13H3v-3z"/><path d="M9 4l3 3"/>',
  /** 設定：三條滑桿 */
  settings: '<path d="M2.5 4h11M2.5 8h11M2.5 12h11"/><circle cx="5.5" cy="4" r="1.5" fill="var(--surface, #fff)"/><circle cx="10.5" cy="8" r="1.5" fill="var(--surface, #fff)"/><circle cx="6.5" cy="12" r="1.5" fill="var(--surface, #fff)"/>',
  folder: '<path d="M1.5 4.5v8a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1v-6a1 1 0 0 0-1-1H8L6.5 3.5h-4a1 1 0 0 0-1 1Z"/>',
  refresh: '<path d="M13.6 6.2A6 6 0 1 0 14 9"/><path d="M14 2.5v3.8h-3.8"/>',
  chevL: '<path d="M10 3 5 8l5 5"/>',
  chevR: '<path d="M6 3l5 5-5 5"/>',
  list: '<path d="M5.5 4h8M5.5 8h8M5.5 12h8"/><circle cx="2.5" cy="4" r=".6"/><circle cx="2.5" cy="8" r=".6"/><circle cx="2.5" cy="12" r=".6"/>',
  pause: '<path d="M5 3v10M11 3v10"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1" fill="currentColor" stroke="none"/>',
  trash: '<path d="M2.5 4.5h11M6 4.5V3h4v1.5M4 4.5l.7 9h6.6l.7-9"/>',
  down: '<path d="M4 6l4 4 4-4"/>',
  search: '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5 14 14"/>',
  mic: '<rect x="6" y="2" width="4" height="7.5" rx="2"/><path d="M3.5 8a4.5 4.5 0 0 0 9 0M8 12.5V14"/>',
  speaker: '<path d="M2.5 6h2.5l3.5-3v10L5 10H2.5z"/><path d="M11 5.5a3.5 3.5 0 0 1 0 5"/>',
};
export type IconName = keyof typeof path;
export const icon = (name: IconName, cls = "") =>
  `<svg viewBox="0 0 16 16" class="ic ${cls}" aria-hidden="true">${path[name]}</svg>`;

export interface AskOptions {
  title: string;
  message?: string;
  /** 列出相關檔案（最多顯示 12 個） */
  list?: string[];
  ok?: string;
  /** 危險動作（刪除）：確定鈕用紅色 */
  danger?: boolean;
  /** 有 input 時是輸入對話框，回傳輸入的文字 */
  input?: { label: string; value: string; select?: [number, number] };
  /** 檢查輸入；回傳錯誤訊息則不關閉，或回傳 Promise（例如送出到伺服器）期間停用按鈕 */
  validate?: (value: string) => string | undefined | Promise<string | undefined>;
}

/** 程式內的確認 / 輸入對話框；取消回傳 undefined，確認回傳 true（或輸入的文字） */
export function ask(o: AskOptions & { input: AskOptions["input"] & {} }): Promise<string | undefined>;
export function ask(o: AskOptions): Promise<true | undefined>;
export function ask(o: AskOptions): Promise<string | true | undefined> {
  const dlg = $<HTMLDialogElement>("askDlg");
  const input = $<HTMLInputElement>("askInput");
  const okBtn = $<HTMLButtonElement>("askOk");
  const err = $("askError");
  $("askTitle").textContent = o.title;
  $("askMsg").textContent = o.message ?? "";
  $("askMsg").hidden = !o.message;
  const list = o.list ?? [];
  const shown = list.slice(0, 12).map((x) => `<li>${esc(x)}</li>`);
  if (list.length > 12) shown.push(`<li>…以及另外 ${list.length - 12} 個</li>`);
  $("askList").innerHTML = shown.join("");
  $("askList").hidden = list.length === 0;
  $("askField").hidden = !o.input;
  $("askLabel").textContent = o.input?.label ?? "";
  input.value = o.input?.value ?? "";
  err.hidden = true;
  okBtn.textContent = o.ok ?? "確定";
  okBtn.classList.toggle("danger", !!o.danger);
  okBtn.disabled = false;
  dlg.showModal();
  if (o.input) {
    input.focus();
    const [a, b] = o.input.select ?? [0, input.value.length];
    input.setSelectionRange(a, b);
  } else (o.danger ? $("askCancel") : okBtn).focus(); // 刪除預設停在「取消」，避免誤按 Enter

  return new Promise((resolve) => {
    const done = (v: string | true | undefined) => {
      $("askForm").removeEventListener("submit", onSubmit);
      $("askCancel").removeEventListener("click", onCancel);
      dlg.removeEventListener("cancel", onCancel);
      if (dlg.open) dlg.close();
      resolve(v);
    };
    const onCancel = (ev: Event) => {
      ev.preventDefault();
      if (!okBtn.disabled) done(undefined); // 送出中不關閉
    };
    const onSubmit = async (ev: Event) => {
      ev.preventDefault();
      if (okBtn.disabled) return;
      const value = input.value;
      if (o.validate) {
        okBtn.disabled = true;
        let msg: string | undefined;
        try {
          msg = await o.validate(value);
        } catch (e) {
          msg = (e as Error).message;
        }
        okBtn.disabled = false;
        if (msg) {
          err.textContent = msg;
          err.hidden = false;
          if (o.input) input.focus();
          return;
        }
      }
      done(o.input ? value : true);
    };
    $("askForm").addEventListener("submit", onSubmit);
    $("askCancel").addEventListener("click", onCancel);
    dlg.addEventListener("cancel", onCancel);
  });
}
