/**
 * 全部錄影對話框：搜尋、篩選、排序、分頁（每頁筆數依視窗高度決定，不出現捲軸）、
 * 勾選多筆移到資源回收筒。加速版（含 GIF）以子列列在原檔底下，各自有長度、大小與播放 / 資料夾按鈕；
 * 勾選原檔時連同底下的加速版一起勾選，可再個別取消。
 */
import { formatBytes, speedLabel, videoClock } from "../shared/format.ts";

/** 匯出檔的標籤：4×、GIF 4×、GIF */
export const exportTag = (x: { speed: number; format?: string }) =>
  x.format === "gif" ? `GIF${x.speed > 1 ? ` ${speedLabel(x.speed)}×` : ""}` : `${speedLabel(x.speed)}×`;
/** 子列的名稱：4× 加速版、GIF 4×、GIF（原速） */
const exportLabel = (x: { speed: number; format?: string }) =>
  x.format === "gif" ? `GIF ${x.speed > 1 ? `${speedLabel(x.speed)}×` : "（原速）"}` : `${speedLabel(x.speed)}× 加速版`;
import { LIBRARY_ROW_PX, type LibraryEntry, type LibraryPage } from "../shared/types.ts";
import { $, api, esc, icon, observeThumbs, shortDate, thumbUrl } from "./common.ts";

export type EntryAction = "play" | "reveal" | "edit" | "export";

export interface LibraryDeps {
  dir(): string;
  act(action: EntryAction, entry: LibraryEntry): void;
  /** 檔案被刪除後通知主畫面更新 */
  changed(): void;
  toast(msg: string, error?: boolean): void;
}

let deps: LibraryDeps;
let bound = false;
let page = 1;
let data: LibraryPage | undefined;
/** 勾選的檔案（原檔與加速版的路徑） */
const selected = new Set<string>();
let fitPx = 0;
let searchTimer: number | undefined;

export function openLibrary(d: LibraryDeps) {
  deps = d;
  if (!bound) bind();
  page = 1;
  selected.clear();
  $<HTMLDialogElement>("libraryDlg").showModal();
  void load();
}

export function isLibraryOpen() {
  return $<HTMLDialogElement>("libraryDlg").open;
}

/** 表格可用的高度：由伺服器依每筆的子列數分頁，一頁剛好放滿、不出現捲軸 */
function availablePx() {
  const wrap = $("libTableWrap");
  const head = wrap.querySelector("thead")?.getBoundingClientRect().height ?? 30;
  return Math.max(LIBRARY_ROW_PX.main * 3, Math.floor(wrap.clientHeight - head - 4));
}

export async function load() {
  const params = new URLSearchParams({
    dir: deps.dir(),
    q: $<HTMLInputElement>("libSearch").value,
    filter: $<HTMLSelectElement>("libFilter").value,
    sort: $<HTMLSelectElement>("libSort").value,
    page: String(page),
    fitPx: String((fitPx = availablePx())),
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
      ].join("");
      const subs = e.exports.map((x) => `<tr class="sub" data-path="${esc(e.path)}" data-export="${esc(x.path)}">
        <td class="c-check"><input type="checkbox" data-select="${esc(x.path)}" ${selected.has(x.path) ? "checked" : ""} aria-label="選取 ${esc(x.name)}" /></td>
        <td><div class="name"><span class="branch" aria-hidden="true">└</span><span class="tag speed">${exportLabel(x)}</span><span class="fn" title="${esc(x.name)}">${esc(x.name)}</span></div></td>
        <td class="c-num">${x.durationSec !== undefined ? videoClock(x.durationSec) : "—"}</td>
        <td class="c-num">${x.width ? `${x.width}×${x.height}` : "—"}</td>
        <td class="c-num">${formatBytes(x.bytes)}</td>
        <td class="c-act"><span class="acts">
          <button class="btn ghost" data-act="play" title="播放" aria-label="播放 ${esc(x.name)}">${icon("play")}</button>
          <button class="btn ghost" data-act="reveal" title="在資料夾中顯示" aria-label="在資料夾中顯示 ${esc(x.name)}">${icon("folder")}</button>
          <span class="btn-ph"></span><span class="btn-ph"></span>
        </span></td>
      </tr>`);
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
      </tr>${subs.join("")}`;
    })
    .join("");
  observeThumbs($("libRows"));
  const pages = data?.pages ?? 1;
  $("libPage").textContent = data ? `第 ${page} / ${pages} 頁・共 ${data.total} 筆` : "";
  $<HTMLButtonElement>("libPrev").disabled = page <= 1;
  $<HTMLButtonElement>("libNext").disabled = page >= pages;
  const all = rows.length > 0 && rows.every((e) => selected.has(e.path) && e.exports.every((x) => selected.has(x.path)));
  $<HTMLInputElement>("libCheckAll").checked = all;
  renderSelection();
}

function renderSelection() {
  const n = selected.size;
  $("libSelected").textContent = n ? `已選取 ${n} 個檔案` : "可勾選多筆一次刪除";
  const del = $<HTMLButtonElement>("libDelete");
  del.hidden = n === 0;
  del.innerHTML = `${icon("trash")}移到資源回收筒`;
}

/** 勾選 / 取消一個檔案；原檔連同底下的加速版一起（之後可個別取消加速版） */
function toggle(path: string, on: boolean) {
  const entry = data?.items.find((e) => e.path === path);
  for (const p of [path, ...(entry?.exports.map((x) => x.path) ?? [])]) {
    if (on) selected.add(p);
    else selected.delete(p);
  }
}

async function removeSelected() {
  const list = [...selected];
  // 原檔要刪、但取消勾選了部分加速版：提醒這些會保留
  const kept = (data?.items ?? []).filter((e) => selected.has(e.path)).reduce((n, e) => n + e.exports.filter((x) => !selected.has(x.path)).length, 0);
  const note = kept ? `\n\n未勾選的 ${kept} 個加速版會保留。` : "";
  if (!confirm(`要把 ${list.length} 個檔案移到資源回收筒嗎？（可從資源回收筒還原）${note}`)) return;
  try {
    await api("/api/delete", { paths: list });
    deps.toast(`已將 ${list.length} 個檔案移到資源回收筒`);
    selected.clear();
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
    const row = t.closest<HTMLElement>("tr[data-path]");
    const entry = data?.items.find((e) => e.path === row?.dataset.path);
    const b = t.closest<HTMLElement>("[data-act]");
    if (b && entry && row?.dataset.export) {
      const x = entry.exports.find((y) => y.path === row.dataset.export);
      const action = b.dataset.act as EntryAction;
      if (x && (action === "play" || action === "reveal")) deps.act(action, { ...x, exports: [] });
      return;
    }
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
      if (availablePx() !== fitPx) void load();
    }, 200);
  }).observe($("libTableWrap"));
}
