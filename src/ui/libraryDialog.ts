/**
 * 全部錄影對話框：搜尋、篩選、排序、分頁（每頁筆數依視窗高度決定，不出現捲軸）、
 * 勾選多筆移到資源回收筒。加速版以標籤顯示在原檔那一列，點標籤即播放。
 */
import { formatBytes, speedLabel, videoClock } from "../shared/format.ts";

/** 匯出檔的標籤：4×、GIF 4×、GIF */
export const exportTag = (x: { speed: number; format?: string }) =>
  x.format === "gif" ? `GIF${x.speed > 1 ? ` ${speedLabel(x.speed)}×` : ""}` : `${speedLabel(x.speed)}×`;
import type { LibraryEntry, LibraryPage } from "../shared/types.ts";
import { $, api, esc, icon, observeThumbs, shortDate, thumbUrl } from "./common.ts";

export type EntryAction = "play" | "reveal" | "edit" | "export";

export interface LibraryDeps {
  dir(): string;
  act(action: EntryAction, entry: LibraryEntry): void;
  /** 檔案被刪除後通知主畫面更新 */
  changed(): void;
  toast(msg: string, error?: boolean): void;
}

const ROW_H = 41;
let deps: LibraryDeps;
let bound = false;
let page = 1;
let data: LibraryPage | undefined;
const selected = new Set<string>();
/** 勾選的原檔底下有哪些加速版（刪除時一併詢問） */
const selectedExports = new Map<string, string[]>();
let searchTimer: number | undefined;

export function openLibrary(d: LibraryDeps) {
  deps = d;
  if (!bound) bind();
  page = 1;
  selected.clear();
  selectedExports.clear();
  $<HTMLDialogElement>("libraryDlg").showModal();
  void load();
}

export function isLibraryOpen() {
  return $<HTMLDialogElement>("libraryDlg").open;
}

/** 依目前可用高度算出一頁放幾筆 */
function pageSize() {
  const wrap = $("libTableWrap");
  const head = wrap.querySelector("thead")?.getBoundingClientRect().height ?? 30;
  return Math.max(3, Math.floor((wrap.clientHeight - head - 4) / ROW_H));
}

export async function load() {
  const params = new URLSearchParams({
    dir: deps.dir(),
    q: $<HTMLInputElement>("libSearch").value,
    filter: $<HTMLSelectElement>("libFilter").value,
    sort: $<HTMLSelectElement>("libSort").value,
    page: String(page),
    pageSize: String(pageSize()),
  });
  $("libEmpty").hidden = false;
  $("libEmpty").textContent = "讀取中…";
  const seq = ++loadSeq;
  try {
    const r = await api<LibraryPage>(`/api/library?${params}`);
    if (seq !== loadSeq) return; // 已有較新的查詢（例如還在輸入搜尋字），舊的回應不要蓋掉它
    data = r;
    page = data.page;
  } catch (e) {
    if (seq !== loadSeq) return;
    data = undefined;
    $("libEmpty").textContent = (e as Error).message;
  }
  render();
}
let loadSeq = 0;

function render() {
  $("libDir").textContent = deps.dir();
  const rows = data?.items ?? [];
  $("libEmpty").hidden = rows.length > 0;
  if (data && rows.length === 0) $("libEmpty").textContent = data.total === 0 && !$<HTMLInputElement>("libSearch").value ? "這個資料夾還沒有錄影。" : "沒有符合條件的錄影。";
  $("libRows").innerHTML = rows
    .map((e) => {
      const tags = [
        /_cut(_\d+)?\.mp4$/i.test(e.name) ? `<span class="tag cut">剪輯版</span>` : "",
        e.hasAudio ? `<span class="tag audio">聲音</span>` : "",
        ...e.exports.map((x) => `<button type="button" class="tag speed as-btn" data-play="${esc(x.path)}" title="播放 ${esc(x.name)}（${x.durationSec ? videoClock(x.durationSec) : ""}・${formatBytes(x.bytes)}）">${exportTag(x)}</button>`),
      ].join("");
      return `<tr data-path="${esc(e.path)}">
        <td class="c-check"><input type="checkbox" data-select="${esc(e.path)}" ${selected.has(e.path) ? "checked" : ""} aria-label="選取 ${esc(e.name)}" /></td>
        <td><div class="name"><img class="thumb lib-thumb" data-src="${thumbUrl(e)}" alt="" decoding="async" /><span class="fn" title="${esc(e.name)}">${esc(shortDate(e.name, e.mtime))}　${esc(e.name)}</span>${tags}</div></td>
        <td class="c-num">${e.durationSec !== undefined ? videoClock(e.durationSec) : "—"}</td>
        <td class="c-num">${e.width ? `${e.width}×${e.height}` : "—"}</td>
        <td class="c-num">${formatBytes(e.bytes)}</td>
        <td class="c-act"><span class="acts">
          <button class="btn ghost" data-act="play" title="播放" aria-label="播放">${icon("play")}</button>
          <button class="btn ghost" data-act="reveal" title="在資料夾中顯示" aria-label="在資料夾中顯示">${icon("folder")}</button>
          <button class="btn ghost" data-act="edit" title="剪輯" aria-label="剪輯">${icon("cut")}</button>
          <button class="btn ghost" data-act="export" title="加速匯出" aria-label="加速匯出">${icon("fast")}</button>
        </span></td>
      </tr>`;
    })
    .join("");
  observeThumbs($("libRows"));
  const pages = data?.pages ?? 1;
  $("libPage").textContent = data ? `第 ${page} / ${pages} 頁・共 ${data.total} 筆` : "";
  $<HTMLButtonElement>("libPrev").disabled = page <= 1;
  $<HTMLButtonElement>("libNext").disabled = page >= pages;
  const all = rows.length > 0 && rows.every((e) => selected.has(e.path));
  $<HTMLInputElement>("libCheckAll").checked = all;
  renderSelection();
}

function renderSelection() {
  const n = selected.size;
  $("libSelected").textContent = n ? `已選取 ${n} 筆` : "可勾選多筆一次刪除";
  const del = $<HTMLButtonElement>("libDelete");
  del.hidden = n === 0;
  del.innerHTML = `${icon("trash")}移到資源回收筒`;
}

function toggle(path: string, on: boolean) {
  const entry = data?.items.find((e) => e.path === path);
  if (on) {
    selected.add(path);
    if (entry) selectedExports.set(path, entry.exports.map((x) => x.path));
  } else {
    selected.delete(path);
    selectedExports.delete(path);
  }
}

async function removeSelected() {
  const paths = [...selected];
  const extra = paths.flatMap((p) => selectedExports.get(p) ?? []);
  let list = paths;
  if (extra.length) {
    const also = confirm(`選取的錄影另有 ${extra.length} 個加速版。\n\n按「確定」連同加速版一起移到資源回收筒；按「取消」只刪除選取的錄影。`);
    if (also) list = [...paths, ...extra];
  } else if (!confirm(`要把 ${paths.length} 個檔案移到資源回收筒嗎？（可從資源回收筒還原）`)) return;
  try {
    await api("/api/delete", { paths: list });
    deps.toast(`已將 ${list.length} 個檔案移到資源回收筒`);
    selected.clear();
    selectedExports.clear();
    deps.changed();
    await load();
  } catch (e) {
    deps.toast((e as Error).message, true);
  }
}

function bind() {
  bound = true;
  const dlg = $<HTMLDialogElement>("libraryDlg");
  dlg.querySelector("[data-close]")!.addEventListener("click", () => dlg.close());
  $("libSearchIcon").outerHTML = icon("search");
  $("libPrev").innerHTML = icon("chevL");
  $("libNext").innerHTML = icon("chevR");
  $("libSearch").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => {
      page = 1;
      void load();
    }, 250);
  });
  for (const id of ["libFilter", "libSort"]) $(id).addEventListener("change", () => {
    page = 1;
    void load();
  });
  $("libPrev").addEventListener("click", () => {
    page--;
    void load();
  });
  $("libNext").addEventListener("click", () => {
    page++;
    void load();
  });
  $<HTMLInputElement>("libCheckAll").addEventListener("change", (ev) => {
    const on = (ev.target as HTMLInputElement).checked;
    for (const e of data?.items ?? []) toggle(e.path, on);
    render();
  });
  $("libRows").addEventListener("change", (ev) => {
    const cb = (ev.target as HTMLElement).closest<HTMLInputElement>("[data-select]");
    if (!cb) return;
    toggle(cb.dataset.select!, cb.checked);
    render();
  });
  $("libRows").addEventListener("click", (ev) => {
    const t = ev.target as HTMLElement;
    const play = t.closest<HTMLElement>("[data-play]");
    const row = t.closest<HTMLElement>("tr[data-path]");
    const entry = data?.items.find((e) => e.path === row?.dataset.path);
    if (play && entry) {
      const x = entry.exports.find((y) => y.path === play.dataset.play);
      if (x) deps.act("play", { ...x, exports: [] });
      return;
    }
    const b = t.closest<HTMLElement>("[data-act]");
    if (b && entry) {
      const action = b.dataset.act as EntryAction;
      if (action === "edit" || action === "export") dlg.close();
      deps.act(action, entry);
    }
  });
  $("libDelete").addEventListener("click", () => void removeSelected());
  // 視窗大小改變時重新計算每頁筆數
  let resizeTimer: number | undefined;
  new ResizeObserver(() => {
    if (!dlg.open || !data) return;
    clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => {
      if (pageSize() !== data?.pageSize) void load();
    }, 200);
  }).observe($("libTableWrap"));
}
