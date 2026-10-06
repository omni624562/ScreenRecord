import { describe, expect, test } from "bun:test";
import {
  AUTO_GPU_PIXELS_PER_SEC,
  chooseEncoder,
  encoderSpec,
  atempoChain,
  concatList,
  ConfigError,
  ENCODERS,
  exportArgs,
  parseMediaInfo,
  planTiles,
  previewArgs,
  resolvePlan,
  segmentArgs,
} from "./args.ts";
import { exportFileName, outputSize, parseExportName } from "./shared/format.ts";
import type { MonitorInfo, RecordConfig } from "./shared/types.ts";

const mon = (id: string, x: number, y: number, w: number, h: number, primary = false): MonitorInfo => {
  const [adapter, output] = id.split(":").map(Number) as [number, number];
  return { id, adapter, output, adapterName: "GPU", deviceName: `\\\\.\\DISPLAY${adapter * 10 + output + 1}`, displayNumber: output + 1, x, y, width: w, height: h, primary, rotation: 1 };
};
// 同一張顯示卡：主螢幕 1920×1080，右邊接一台較高的 2560×1440（上緣比主螢幕高 180）
const SAME_GPU = [mon("0:0", 0, 0, 1920, 1080, true), mon("0:1", 1920, -180, 2560, 1440)];
// 混合顯卡：外接螢幕在另一張卡上
const TWO_GPUS = [mon("0:0", 0, 0, 1920, 1080, true), mon("1:0", 1920, -180, 2560, 1440)];
const x264 = ENCODERS[0]!;

const cfg = (over: Partial<RecordConfig> = {}): RecordConfig => ({
  source: { type: "monitor", monitorId: "0:0" },
  fps: 30,
  scale: 100,
  drawMouse: true,
  maxMinutes: 0,
  method: "auto",
  outputDir: "C:\\out",
  audio: { system: false, mic: false, micId: "" },
  ...over,
});
const graphOf = (args: string[]) => args[args.indexOf("-filter_complex") + 1]!;
const after = (args: string[], k: string) => args[args.indexOf(k) + 1];

describe("resolvePlan", () => {
  test("單一螢幕：一塊完整的 ddagrab", () => {
    const p = resolvePlan(cfg({ source: { type: "monitor", monitorId: "0:1" } }), SAME_GPU);
    expect(p.rect).toEqual({ x: 1920, y: -180, width: 2560, height: 1440 });
    expect(p.dda).toEqual({ adapter: 0, tiles: [{ output: 1, offsetX: 0, offsetY: 0, width: 2560, height: 1440, full: true, x: 0, y: 0 }] });
  });

  test("自訂範圍落在單一螢幕內：換算成相對座標", () => {
    const p = resolvePlan(cfg({ source: { type: "region", x: 2000, y: -100, width: 801, height: 601 }, scale: 50 }), SAME_GPU);
    expect(p.dda?.tiles).toEqual([{ output: 1, offsetX: 80, offsetY: 80, width: 801, height: 601, full: false, x: 0, y: 0 }]);
    expect([p.outWidth, p.outHeight]).toEqual([400, 300]);
  });

  test("所有螢幕（同一張顯示卡）：每個螢幕一塊，位置相對於整個桌面", () => {
    const p = resolvePlan(cfg({ source: { type: "all" } }), SAME_GPU);
    expect(p.rect).toEqual({ x: 0, y: -180, width: 4480, height: 1440 });
    expect(p.dda?.tiles.map((t) => [t.output, t.x, t.y, t.width, t.height, t.full])).toEqual([
      [0, 0, 180, 1920, 1080, true],
      [1, 1920, 0, 2560, 1440, true],
    ]);
  });

  test("跨螢幕的自訂範圍：各取交集", () => {
    const p = resolvePlan(cfg({ source: { type: "region", x: 1800, y: 0, width: 400, height: 300 } }), SAME_GPU);
    expect(p.dda?.tiles.map((t) => [t.output, t.offsetX, t.offsetY, t.width, t.height, t.x, t.y])).toEqual([
      [0, 1800, 0, 120, 300, 0, 0],
      [1, 0, 180, 280, 300, 120, 0],
    ]);
  });

  test("跨不同顯示卡：無法用 ddagrab，只能 gdigrab", () => {
    const p = resolvePlan(cfg({ source: { type: "all" } }), TWO_GPUS);
    expect(p.dda).toBeUndefined();
    expect(p.monitors.length).toBe(2);
    expect(() => segmentArgs(p, cfg(), "ddagrab", x264, "o.mp4")).toThrow(ConfigError);
  });

  test("不合法的設定會丟出 ConfigError", () => {
    expect(() => resolvePlan(cfg({ fps: 0 }), SAME_GPU)).toThrow(ConfigError);
    expect(() => resolvePlan(cfg({ fps: 61 }), SAME_GPU)).toThrow(ConfigError);
    expect(() => resolvePlan(cfg({ scale: 60 as never }), SAME_GPU)).toThrow(ConfigError);
    expect(() => resolvePlan(cfg({ source: { type: "monitor", monitorId: "9:9" } }), SAME_GPU)).toThrow(ConfigError);
    expect(() => resolvePlan(cfg({ source: { type: "region", x: 99999, y: 0, width: 100, height: 100 } }), SAME_GPU)).toThrow(ConfigError);
  });
});

describe("segmentArgs", () => {
  test("ddagrab 單一區塊：指定顯示卡、output，下載後轉 yuv420p", () => {
    const c = cfg({ source: { type: "region", x: 2000, y: -100, width: 801, height: 601 } });
    const args = segmentArgs(resolvePlan(c, SAME_GPU), c, "ddagrab", x264, "C:\\p\\seg.mp4");
    expect(args).toContain("d3d11va=dda:0");
    expect(graphOf(args)).toBe(
      "ddagrab=output_idx=1:framerate=30:draw_mouse=1:offset_x=80:offset_y=80:video_size=801x601,hwdownload,format=bgra," +
        "crop=800:600:0:0,scale=800:600:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]",
    );
    expect(after(args, "-map")).toBe("[vout]");
    expect(args).not.toContain("-nostdin"); // 必須能從 stdin 收到 q
    expect(args.slice(-2)).toEqual(["-y", "C:\\p\\seg.mp4"]);
  });

  test("ddagrab 多螢幕：xstack 依位置拼接", () => {
    const c = cfg({ source: { type: "all" }, scale: 50 });
    const graph = graphOf(segmentArgs(resolvePlan(c, SAME_GPU), c, "ddagrab", x264, "o.mp4"));
    expect(graph).toBe(
      "ddagrab=output_idx=0:framerate=30:draw_mouse=1,hwdownload,format=bgra[t0];" +
        "ddagrab=output_idx=1:framerate=30:draw_mouse=1,hwdownload,format=bgra[t1];" +
        "[t0][t1]xstack=inputs=2:layout=0_180|1920_0:fill=black," +
        "scale=2240:720:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]",
    );
  });

  test("ddagrab 多螢幕且範圍超出螢幕：pad 補到完整大小", () => {
    // 範圍從主螢幕上方的空白處開始
    const c = cfg({ source: { type: "region", x: 1000, y: -180, width: 1500, height: 400 } });
    const graph = graphOf(segmentArgs(resolvePlan(c, SAME_GPU), c, "ddagrab", x264, "o.mp4"));
    expect(graph).toContain("xstack=inputs=2:layout=0_180|920_0:fill=black,scale=1500:400");
    expect(graph).not.toContain("pad="); // 右邊螢幕已涵蓋整個高度
    const c2 = cfg({ source: { type: "region", x: 1000, y: 900, width: 1500, height: 400 } });
    expect(graphOf(segmentArgs(resolvePlan(c2, SAME_GPU), c2, "ddagrab", x264, "o.mp4"))).toContain("pad=1500:400:0:0:color=black");
  });

  test("gdigrab：使用虛擬桌面絕對座標（可為負值）", () => {
    const c = cfg({ source: { type: "monitor", monitorId: "1:0" }, drawMouse: false, scale: 25 });
    const args = segmentArgs(resolvePlan(c, TWO_GPUS), c, "gdigrab", x264, "o.mp4");
    expect(["-offset_x", "-offset_y", "-video_size", "-draw_mouse", "-framerate"].map((k) => after(args, k))).toEqual(["1920", "-180", "2560x1440", "0", "30"]);
    expect(graphOf(args)).toBe("[0:v]scale=640:360:flags=bicubic:out_color_matrix=bt709:out_range=tv,format=yuv420p[vout]");
  });

  test("錄聲音：加入音訊輸入、showinfo 與 AAC", () => {
    const audioIn = ["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "tcp://127.0.0.1:5000"];
    const c = cfg();
    const dda = segmentArgs(resolvePlan(c, SAME_GPU), c, "ddagrab", x264, "o.mp4", audioIn);
    expect(after(dda, "-loglevel")).toBe("level+info");
    expect(graphOf(dda)).toContain("format=bgra,showinfo=checksum=0,");
    expect(dda.join(" ")).toContain("-map [vout] -map 0:a -c:a aac");
    const gdi = segmentArgs(resolvePlan(c, SAME_GPU), c, "gdigrab", x264, "o.mp4", audioIn);
    expect(gdi.join(" ")).toContain("-map [vout] -map 1:a"); // gdigrab 是第 0 個輸入，聲音是第 1 個
    expect(graphOf(gdi).startsWith("[0:v]showinfo=checksum=0,")).toBe(true);
  });
});

describe("預覽", () => {
  test("同一張顯示卡用 ddagrab 合成；跨顯示卡用 gdigrab", () => {
    expect(graphOf(previewArgs(SAME_GPU, true))).toContain("xstack=inputs=2");
    expect(previewArgs(TWO_GPUS, true)).toContain("gdigrab");
    expect(previewArgs(SAME_GPU, false)).toContain("gdigrab");
  });
  test("單一螢幕預覽只擷取那一台", () => {
    const second = SAME_GPU[1]!;
    const dda = graphOf(previewArgs([second], true));
    expect(dda).not.toContain("xstack");
    expect(dda).toContain(`output_idx=${second.output}`);
    const gdi = previewArgs([second], false).join(" ");
    expect(gdi).toContain(`-offset_x ${second.x} -offset_y ${second.y} -video_size ${second.width}x${second.height}`);
    expect(previewArgs(SAME_GPU, false)).not.toContain("-offset_x");
  });
  test("planTiles 會忽略與範圍不相交的螢幕", () => {
    expect(planTiles({ x: 0, y: 0, width: 100, height: 100 }, SAME_GPU).involved.map((m) => m.id)).toEqual(["0:0"]);
  });
});

describe("匯出", () => {
  test("檔名與倍率互轉", () => {
    expect(exportFileName("Rec_2026-10-05_14-30-00.mp4", 4)).toBe("Rec_2026-10-05_14-30-00_4x.mp4");
    expect(exportFileName("Rec_a.mp4", 1.5)).toBe("Rec_a_1.5x.mp4");
    expect(parseExportName("Rec_a_1.5x.mp4")).toEqual({ base: "Rec_a.mp4", speed: 1.5 });
    expect(parseExportName("Rec_a_8x_2.mp4")).toEqual({ base: "Rec_a.mp4", speed: 8 });
    expect(parseExportName("Rec_2026-10-05_14-30-00.mp4")).toBeUndefined();
  });

  test("exportArgs：setpts 壓縮時間、維持原 fps", () => {
    const args = exportArgs("in.mp4", "out.mp4", 4, 30, x264);
    expect(after(args, "-vf")).toBe("setpts=PTS/4,fps=30,format=yuv420p");
    expect(args).toContain("-an");
    expect(() => exportArgs("in.mp4", "out.mp4", 1, 30, x264)).toThrow(ConfigError);
  });

  test("exportArgs 保留聲音：atempo 每段不超過 2 倍", () => {
    const args = exportArgs("in.mp4", "out.mp4", 16, 30, x264, true);
    expect(after(args, "-af")).toBe("atempo=2,atempo=2,atempo=2,atempo=2");
    expect(args.join(" ")).toContain("-map 0:v:0 -map 0:a:0");
    expect(atempoChain(1.5)).toBe("atempo=1.5");
    expect(atempoChain(3)).toBe("atempo=2,atempo=1.5");
  });

  test("parseMediaInfo", () => {
    const stderr = `Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x.mp4':
  Duration: 00:01:02.50, start: 0.000000, bitrate: 1234 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 1920x1080 [SAR 1:1 DAR 16:9], 1200 kb/s, 30 fps, 30 tbr, 15360 tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 160 kb/s (default)`;
    expect(parseMediaInfo(stderr)).toEqual({ durationSec: 62.5, width: 1920, height: 1080, fps: 30, hasAudio: true });
    expect(parseMediaInfo(stderr.split("\n").slice(0, 3).join("\n")).hasAudio).toBe(false);
  });
});

test("outputSize 一律取偶數", () => {
  expect(outputSize(1921, 1081, 100)).toEqual({ width: 1920, height: 1080 });
  expect(outputSize(1366, 768, 75)).toEqual({ width: 1024, height: 576 });
  expect(outputSize(10, 10, 25)).toEqual({ width: 2, height: 2 });
});

test("concatList 跳脫單引號並使用正斜線", () => {
  expect(concatList(["C:\\Users\\O'Neil\\seg_000.mp4"])).toBe("file 'C:/Users/O'\\''Neil/seg_000.mp4'\n");
});

describe("chooseEncoder", () => {
  const cpu = encoderSpec("libx264")!;
  const qsv = encoderSpec("h264_qsv")!;
  test("自動：1080p30 用 CPU", () => {
    expect(chooseEncoder("auto", 1920, 1080, 30, cpu, [qsv]).spec.name).toBe("libx264");
  });
  test("自動：超過 1080p60（例如 4K30、雙螢幕拼接）改用 GPU", () => {
    expect(3840 * 2160 * 30).toBeGreaterThan(AUTO_GPU_PIXELS_PER_SEC);
    expect(chooseEncoder("auto", 3840, 2160, 30, cpu, [qsv]).spec.name).toBe("h264_qsv");
    expect(chooseEncoder("auto", 4480, 1440, 30, cpu, [qsv]).spec.name).toBe("h264_qsv");
    expect(chooseEncoder("auto", 1920, 1080, 60, cpu, [qsv]).spec.name).toBe("libx264"); // 剛好 1080p60 仍用 CPU
  });
  test("自動：先前偵測到 CPU 跟不上就改用 GPU", () => {
    expect(chooseEncoder("auto", 1280, 720, 30, cpu, [qsv], true).spec.name).toBe("h264_qsv");
  });
  test("自動：沒有 GPU 時一律 CPU", () => {
    expect(chooseEncoder("auto", 3840, 2160, 60, cpu, [], true).spec.name).toBe("libx264");
  });
  test("指定 GPU / CPU", () => {
    expect(chooseEncoder("gpu", 640, 360, 30, cpu, [qsv]).spec.name).toBe("h264_qsv");
    expect(chooseEncoder("cpu", 3840, 2160, 60, cpu, [qsv], true).spec.name).toBe("libx264");
    expect(() => chooseEncoder("gpu", 640, 360, 30, cpu, [])).toThrow(ConfigError);
  });
});