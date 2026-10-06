/**
 * 設定存檔：%LOCALAPPDATA%\ScreenRecorder\settings.json
 * - ui：網頁介面的完整設定（換瀏覽器或連接埠也不會遺失）
 * - config：最後一次的錄影設定，系統匣選單「開始錄影」直接使用
 * rev 每次變更 +1，網頁看到 rev 變了（例如從系統匣切換錄音）就重新讀取。
 */
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { logDir } from "./log.ts";
import type { RecordConfig } from "./shared/types.ts";

export interface SavedSettings {
  ui?: Record<string, unknown>;
  config?: RecordConfig;
  /** 「自動」編碼曾偵測到 CPU 跟不上：之後的錄影改用 GPU */
  preferGpu?: boolean;
  rev: number;
}

const file = join(logDir, "settings.json");
let cache: SavedSettings | undefined;

export function loadSettings(): SavedSettings {
  if (cache) return cache;
  try {
    const data = JSON.parse(readFileSync(file, "utf8")) as SavedSettings;
    cache = { ui: data.ui, config: data.config, preferGpu: data.preferGpu, rev: 1 };
  } catch {
    cache = { rev: 1 };
  }
  return cache;
}

export function saveSettings(patch: Partial<Omit<SavedSettings, "rev">>): SavedSettings {
  const cur = loadSettings();
  cache = { ...cur, ...patch, rev: cur.rev + 1 };
  try {
    writeFileSync(file, JSON.stringify({ ui: cache.ui, config: cache.config, preferGpu: cache.preferGpu }, null, 2), "utf8");
  } catch (e) {
    console.error(`無法儲存設定：${(e as Error).message}`);
  }
  return cache;
}
