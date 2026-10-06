/** 前端共用工具：DOM、API、提示訊息、圖示。 */

export const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

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
export function shortDate(name: string, mtime: number): string {
  const m = /(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})/.exec(name);
  if (m) return `${m[2]}/${m[3]} ${m[4]}:${m[5]}`;
  const d = new Date(mtime);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** 線條圖示（currentColor） */
const path = {
  play: '<path d="M5 3.5v9l7.5-4.5z" fill="currentColor" stroke="none"/>',
  cut: '<circle cx="4.5" cy="4.5" r="2"/><circle cx="4.5" cy="11.5" r="2"/><path d="M6.2 5.6 13.5 12M6.2 10.4 13.5 4"/>',
  fast: '<path d="M2.5 4v8l5-4zM8.5 4v8l5-4z" fill="currentColor" stroke="none"/>',
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
