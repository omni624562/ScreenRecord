/**
 * 把 exe 的 PE 標頭改成 Windows GUI 子系統（2），啟動時就不會建立主控台視窗。
 *
 * bun build 的 --windows-hide-console 只是在程式開始執行後才把主控台藏起來：
 * 視窗仍會閃一下，若預設終端機是 Windows Terminal 則完全藏不掉。
 * 標成 GUI 程式後 Windows 根本不配置主控台；子行程（FFmpeg）都以 windowsHide 啟動，不會跳出視窗。
 *
 * 用法：bun run scripts/set-gui-subsystem.ts dist/ScreenRecorder.exe
 */
import { readFileSync, writeFileSync } from "node:fs";

const file = process.argv[2];
if (!file) throw new Error("請指定 exe 路徑");
const b = readFileSync(file);
const pe = b.readInt32LE(0x3c);
if (b.toString("latin1", pe, pe + 4) !== "PE\0\0") throw new Error("不是有效的 PE 檔");
const magic = b.readUInt16LE(pe + 24);
if (magic !== 0x20b && magic !== 0x10b) throw new Error("無法辨識的 Optional Header");
const at = pe + 24 + 68; // Subsystem 欄位在 PE32 / PE32+ 的位置相同
const before = b.readUInt16LE(at);
b.writeUInt16LE(2, at); // IMAGE_SUBSYSTEM_WINDOWS_GUI
writeFileSync(file, b);
console.log(`${file}：subsystem ${before} → 2（GUI，不顯示主控台）`);
