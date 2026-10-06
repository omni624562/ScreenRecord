/**
 * 主控台輸出同時寫入記錄檔。隱藏主控台視窗的版本看不到訊息，出問題時可從這裡查：
 * %LOCALAPPDATA%\ScreenRecorder\ScreenRecorder.log（超過 1 MB 時改名為 .old.log 重新開始）
 */
import { appendFileSync, existsSync, mkdirSync, renameSync, statSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";
import { format } from "node:util";

export const logDir = join(process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local"), "ScreenRecorder");
export const logFile = join(logDir, "ScreenRecorder.log");

export function setupLogFile() {
  try {
    mkdirSync(logDir, { recursive: true });
    if (existsSync(logFile) && statSync(logFile).size > 1024 * 1024) renameSync(logFile, join(logDir, "ScreenRecorder.old.log"));
  } catch {
    return; // 無法寫記錄檔不影響使用
  }
  const stamp = () => new Date().toLocaleString("zh-TW", { hour12: false });
  const write = (level: string, args: unknown[]) => {
    try {
      appendFileSync(logFile, `${stamp()} ${level} ${format(...args)}\n`, "utf8");
    } catch {
      // 忽略
    }
  };
  for (const [name, level] of [["log", "INFO "], ["warn", "WARN "], ["error", "ERROR"]] as const) {
    const orig = console[name].bind(console);
    console[name] = (...args: unknown[]) => {
      orig(...args);
      write(level, args);
    };
  }
  write("INFO ", ["──── 程式啟動 ────"]);
}
