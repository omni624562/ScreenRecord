/**
 * 檢查新版本：查詢 GitHub Releases 的最新版本，比目前新就在系統匣與操作視窗提示（不會自動下載或安裝）。
 * 只送出一個不帶任何個人資料的 GET 請求；網路不通或被限流時安靜略過，之後再試。
 */
import type { UpdateInfo } from "./shared/types.ts";

export const RELEASES_API = "https://api.github.com/repos/omni624562/ScreenRecord/releases/latest";

/** 比較 "1.2.0" 這類版本號：a > b 回傳正數 */
export function compareVersions(a: string, b: string): number {
  const parse = (v: string) => v.replace(/^v/i, "").split("-")[0]!.split(".").map((n) => Number.parseInt(n, 10) || 0);
  const x = parse(a), y = parse(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d) return d;
  }
  return 0;
}

interface Release {
  tag_name?: string;
  html_url?: string;
  published_at?: string;
  draft?: boolean;
  prerelease?: boolean;
}

/** 有比 current 新的正式版就回傳它；沒有或查不到回傳 undefined */
export async function checkForUpdate(
  current: string,
  fetchImpl: (url: string, init: RequestInit) => Promise<Response> = fetch,
): Promise<UpdateInfo | undefined> {
  const r = await fetchImpl(RELEASES_API, {
    headers: { Accept: "application/vnd.github+json", "User-Agent": `ScreenRecorder/${current}` },
    signal: AbortSignal.timeout(15_000),
  });
  // 儲存庫設為私人時，未登入的查詢一律得到 404
  if (r.status === 404) throw new Error("GitHub 上找不到公開的發佈版本（儲存庫可能設為私人）");
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  const rel = (await r.json()) as Release;
  if (!rel.tag_name || rel.draft || rel.prerelease) return undefined;
  const version = rel.tag_name.replace(/^v/i, "");
  if (compareVersions(version, current) <= 0) return undefined;
  // 只接受指向本專案的網址（之後會交給 explorer 開啟）
  const url = rel.html_url?.startsWith("https://github.com/omni624562/ScreenRecord/") ? rel.html_url : "https://github.com/omni624562/ScreenRecord/releases/latest";
  return { version, url, publishedAt: rel.published_at };
}
