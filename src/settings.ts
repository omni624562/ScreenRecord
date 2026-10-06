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
  /** 自動檢查新版本（未指定視為開啟） */
  checkUpdates?: boolean;
  /** 已用系統匣通知過的新版本（同一版只通知一次） */
  updateNotified?: string;
  rev: number;
}

const file = join(logDir, "settings.json");
let cache: SavedSettings | undefined;

export function loadSettings(): SavedSettings {
  if (cache) return cache;
  try {
    const { rev: _rev, ...data } = JSON.parse(readFileSync(file, "utf8")) as SavedSettings;
    cache = { ...data, rev: 1 };
  } catch {
    cache = { rev: 1 };
  }
  return cache;
}

export function saveSettings(patch: Partial<Omit<SavedSettings, "rev">>): SavedSettings {
  const cur = loadSettings();
  cache = { ...cur, ...patch, rev: cur.rev + 1 };
  try {
    const { rev: _rev, ...data } = cache;
    writeFileSync(file, JSON.stringify(data, null, 2), "utf8");
  } catch (e) {
    console.error(`無法儲存設定：${(e as Error).message}`);
  }
  return cache;
}
