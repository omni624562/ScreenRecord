/** 主執行緒與系統匣 Worker 之間的訊息。 */
import type { RecorderState } from "./shared/types.ts";

export type TrayCommand =
  | "open"
  | "start-last"
  | `start-monitor:${string}`
  | "start-all"
  | "pause"
  | "resume"
  | "stop"
  | "toggle-system"
  | "toggle-mic"
  | "open-folder"
  | "play-last"
  | "autostart"
  | "quit";

export interface TrayState {
  rec: RecorderState;
  /** 滑鼠停在圖示上的提示文字（最多 127 字） */
  tip: string;
  /** 「開始錄影」旁顯示的上次範圍，例如「螢幕 1」 */
  lastSource: string;
  monitors: { id: string; label: string }[];
  audio: { system: boolean; mic: boolean };
  /** 是否能開始錄影（有 FFmpeg、沒有轉檔工作） */
  canRecord: boolean;
  lastResult?: string;
  /** null = 無法設定（開發模式） */
  autostart: boolean | null;
}

export type MainToTray =
  | { type: "state"; state: TrayState }
  | { type: "balloon"; title: string; text: string; warn?: boolean }
  | { type: "dispose" }
  | { type: "debug-menu" };

export type TrayToMain =
  | { type: "hwnd"; hwnd: number }
  | { type: "ready" }
  | { type: "error"; message: string }
  | { type: "cmd"; cmd: TrayCommand }
  | { type: "disposed" }
  | { type: "log"; text: string };
