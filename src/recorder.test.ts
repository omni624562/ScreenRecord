/**
 * 開始錄影的「準備 / 倒數 / 縮小視窗」階段：取消與連按的競態。
 * 用假的依賴，不會真的啟動 FFmpeg（測試都在擷取開始前就取消）。
 */
import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ENCODERS } from "./args.ts";
import { Recorder, type RecorderDeps } from "./recorder.ts";
import type { MonitorInfo, RecordConfig } from "./shared/types.ts";

const MON: MonitorInfo = {
  id: "0:0", adapter: 0, output: 0, adapterName: "GPU", deviceName: "\\\\.\\DISPLAY1",
  displayNumber: 1, x: 0, y: 0, width: 1920, height: 1080, primary: true, rotation: 1,
};
const dirs: string[] = [];
afterEach(() => {
  for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

function setup(over: Partial<RecorderDeps> = {}) {
  const outputDir = mkdtempSync(join(tmpdir(), "sr-rec-test-"));
  dirs.push(outputDir);
  const calls = { before: 0, after: 0 };
  let releaseEncoders = () => {};
  const encodersGate = new Promise<void>((r) => (releaseEncoders = r));
  const deps: RecorderDeps = {
    ffmpegPath: () => "ffmpeg-not-used.exe",
    encoders: async () => {
      await encodersGate;
      return { cpu: ENCODERS[0], gpu: [] };
    },
    preferGpu: () => false,
    learnGpu: () => {},
    monitors: () => [MON],
    ddagrabUsable: async () => true,
    beforeCapture: async () => {
      calls.before++;
    },
    afterStop: () => {
      calls.after++;
    },
    ...over,
  };
  const config = (o: Partial<RecordConfig> = {}): RecordConfig => ({
    source: { type: "monitor", monitorId: "0:0" }, fps: 30, scale: 100, drawMouse: true, maxMinutes: 0,
    method: "auto", outputDir, audio: { system: false, mic: false, micId: "" }, countdownSec: 3, hideUi: true, ...o,
  });
  return { rec: new Recorder(deps), config, calls, outputDir, releaseEncoders };
}

const partsLeft = (dir: string) => existsSync(join(dir, ".parts")) && readdirSync(join(dir, ".parts")).length > 0;

describe("開始錄影的取消與連按", () => {
  test("準備中（狀態仍是待命）按停止：取消，不進入倒數也不錄影", async () => {
    const { rec, config, outputDir, releaseEncoders, calls } = setup();
    const started = rec.start(config());
    expect(rec.status().state).toBe("idle");
    expect(rec.starting).toBe(true);
    await rec.stop();
    releaseEncoders();
    await started;
    expect(rec.status().state).toBe("idle");
    expect(rec.starting).toBe(false);
    expect(calls.before).toBe(0);
    expect(partsLeft(outputDir)).toBe(false);
  });

  test("準備中再按一次開始：拒絕，不會排出第二次錄影", async () => {
    const { rec, config, releaseEncoders } = setup();
    const first = rec.start(config());
    await expect(rec.start(config())).rejects.toThrow("正在準備開始");
    await rec.stop();
    releaseEncoders();
    await first;
    expect(rec.status().state).toBe("idle");
  });

  test("倒數中取消：回到待命、不縮小視窗、不留分段資料夾", async () => {
    const { rec, config, outputDir, releaseEncoders, calls } = setup();
    releaseEncoders();
    await rec.start(config()); // 進入倒數就回覆
    expect(rec.status().state).toBe("countdown");
    expect(rec.status().countdownMs).toBeGreaterThan(2000);
    await rec.stop();
    await Bun.sleep(20);
    expect(rec.status().state).toBe("idle");
    expect(calls.before).toBe(0);
    expect(partsLeft(outputDir)).toBe(false);
  });

  test("縮小視窗的那一刻取消：還原視窗，不開始擷取", async () => {
    let rec!: Recorder;
    const s = setup({
      beforeCapture: async () => {
        void rec.stop(); // 倒數剛結束、正在縮小視窗時按下取消
        await Bun.sleep(10);
      },
    });
    rec = s.rec;
    s.releaseEncoders();
    await rec.start(s.config({ countdownSec: 0 }));
    expect(rec.status().state).toBe("idle");
    expect(s.calls.after).toBe(1); // 已縮小的視窗被還原
    expect(partsLeft(s.outputDir)).toBe(false);
  });

  test("結束程式時正在準備：等它收拾好再結束", async () => {
    const { rec, config, outputDir, releaseEncoders } = setup();
    const started = rec.start(config());
    const done = rec.shutdown();
    releaseEncoders();
    await done;
    await started;
    expect(rec.status().state).toBe("idle");
    expect(partsLeft(outputDir)).toBe(false);
  });
});
