/** 介面使用的共用計算（shared/format.ts、shared/edit.ts）；後端的 Rust 版本有對應的單元測試。 */
import { describe, expect, test } from "bun:test";
import { cutFileName, keepRanges, normalizeCrop, normalizeRanges, totalLength } from "./edit.ts";
import { estimateBytes, exportFileName, outputSize, parseClock, parseExportName, scaledSize, speedForTarget } from "./format.ts";

describe("keepRanges", () => {
  test("只剪頭尾", () => {
    expect(keepRanges(60, { start: 5, end: 50, removed: [] })).toEqual([[5, 50]]);
  });
  test("刪除中間多段（亂序、重疊、超出範圍都能處理）", () => {
    const keep = keepRanges(60, { start: 2, end: 58, removed: [[40, 45], [10, 20], [15, 25], [55, 70]] });
    expect(keep).toEqual([[2, 10], [25, 40], [45, 55]]);
    expect(totalLength(keep)).toBe(33);
  });
  test("刪除範圍涵蓋開頭時只留後段", () => {
    expect(keepRanges(30, { start: 0, end: 30, removed: [[0, 12.5]] })).toEqual([[12.5, 30]]);
  });
  test("開頭結尾超出影片長度會被限制", () => {
    expect(keepRanges(10, { start: -3, end: 99, removed: [] })).toEqual([[0, 10]]);
  });
  test("normalizeRanges 合併相鄰片段並忽略極短片段", () => {
    expect(normalizeRanges([[1, 2], [2, 3], [5, 5.001]], 10)).toEqual([[1, 3]]);
  });
});

describe("normalizeCrop", () => {
  test("寬高取偶數、限制在畫面內", () => {
    expect(normalizeCrop({ x: 101, y: 50, width: 301, height: 9999 }, 1920, 1080)).toEqual({ x: 101, y: 50, width: 300, height: 1030 });
  });
  test("等於整個畫面時視為不裁切", () => {
    expect(normalizeCrop({ x: 0, y: 0, width: 1920, height: 1080 }, 1920, 1080)).toBeUndefined();
  });
  test("最小 16px", () => {
    expect(normalizeCrop({ x: 10, y: 10, width: 3, height: 3 }, 1920, 1080)).toEqual({ x: 10, y: 10, width: 16, height: 16 });
  });
});

test("剪輯版檔名，且之後的加速版會歸在剪輯版底下", () => {
  expect(cutFileName("Rec_2026-10-06_08-00-00.mp4")).toBe("Rec_2026-10-06_08-00-00_cut.mp4");
  expect(parseExportName("Rec_2026-10-06_08-00-00_cut.mp4")).toBeUndefined(); // 剪輯版本身是獨立的錄影
  expect(parseExportName("Rec_2026-10-06_08-00-00_cut_4x.mp4")).toEqual({ base: "Rec_2026-10-06_08-00-00_cut.mp4", speed: 4, format: "mp4" });
});

describe("匯出", () => {
  test("檔名與倍率互轉", () => {
    expect(exportFileName("Rec_2026-10-05_14-30-00.mp4", 4)).toBe("Rec_2026-10-05_14-30-00_4x.mp4");
    expect(exportFileName("Rec_a.mp4", 1.5)).toBe("Rec_a_1.5x.mp4");
    expect(parseExportName("Rec_a_1.5x.mp4")).toEqual({ base: "Rec_a.mp4", speed: 1.5, format: "mp4" });
    expect(parseExportName("Rec_a_8x_2.mp4")).toEqual({ base: "Rec_a.mp4", speed: 8, format: "mp4" });
    expect(parseExportName("Rec_2026-10-05_14-30-00.mp4")).toBeUndefined();
  });

  test("GIF 檔名", () => {
    expect(exportFileName("Rec_a.mp4", 4, "gif")).toBe("Rec_a_4x.gif");
    expect(exportFileName("Rec_a.mp4", 1, "gif")).toBe("Rec_a.gif");
    expect(parseExportName("Rec_a_4x.gif")).toEqual({ base: "Rec_a.mp4", speed: 4, format: "gif" });
    expect(parseExportName("Rec_a.gif")).toEqual({ base: "Rec_a.mp4", speed: 1, format: "gif" });
    expect(parseExportName("Rec_a_2.gif")).toEqual({ base: "Rec_a.mp4", speed: 1, format: "gif" });
  });

  test("依目標長度算倍率", () => {
    expect(parseClock("1:30")).toBe(90);
    expect(parseClock("90")).toBe(90);
    expect(parseClock("1:02:03")).toBe(3723);
    expect(parseClock("abc")).toBeUndefined();
    expect(parseClock("0")).toBeUndefined();
    expect(speedForTarget(3600, 60)).toBe(60);
    expect(speedForTarget(100, 30)).toBe(3.33);
    expect(speedForTarget(10, 60)).toBe(1.1); // 目標比原片長：用最小倍率
    expect(speedForTarget(10, 60, true)).toBe(1); // GIF 可原速
    expect(speedForTarget(36000, 1)).toBe(1000);
  });
});

test("scaledSize：只縮不放大、高度等比取偶數", () => {
  expect(scaledSize(3840, 1080, 1920)).toEqual({ width: 1920, height: 540 });
  expect(scaledSize(1920, 1080, 1280)).toEqual({ width: 1280, height: 720 });
  expect(scaledSize(1366, 768, 1280)).toEqual({ width: 1280, height: 720 });
  expect(scaledSize(1280, 720, 1920)).toEqual({ width: 1280, height: 720 });
  expect(scaledSize(1920, 1080, 0)).toEqual({ width: 1920, height: 1080 });
});

test("estimateBytes：範圍合理、縮小尺寸與加速會變小", () => {
  const base = { format: "mp4" as const, srcBytes: 60e6, srcSec: 600, srcWidth: 1920, srcHeight: 1080, width: 1920, height: 1080 };
  const [lo, hi] = estimateBytes({ ...base, speed: 4 });
  expect(lo).toBeLessThan(hi);
  expect(lo).toBeGreaterThan(0);
  // 10 分鐘 60 MB、4× → 約 2.5 分鐘；以原位元率 × 2 為中心
  expect((lo + hi) / 2).toBeGreaterThan(15e6);
  expect((lo + hi) / 2).toBeLessThan(60e6);
  expect(estimateBytes({ ...base, speed: 4, width: 1280, height: 720 })[1]).toBeLessThan(hi);
  expect(estimateBytes({ ...base, speed: 16 })[1]).toBeLessThan(hi);
  const gif = estimateBytes({ ...base, format: "gif", speed: 1, width: 640, height: 360, gifFps: 10, srcSec: 10 });
  expect(gif[0]).toBeCloseTo(640 * 360 * 100 * 0.03, -3);
});

test("outputSize 一律取偶數", () => {
  expect(outputSize(1921, 1081, 100)).toEqual({ width: 1920, height: 1080 });
  expect(outputSize(1366, 768, 75)).toEqual({ width: 1024, height: 576 });
  expect(outputSize(10, 10, 25)).toEqual({ width: 2, height: 2 });
});
