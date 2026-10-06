import { describe, expect, test } from "bun:test";
import { ConfigError, cutArgs, ENCODERS } from "./args.ts";
import { cutFileName, keepRanges, normalizeCrop, normalizeRanges, totalLength } from "./shared/edit.ts";
import { parseExportName } from "./shared/format.ts";

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

describe("cutArgs", () => {
  const x264 = ENCODERS[0]!;
  test("select 保留多段、重新接時間、裁切", () => {
    const args = cutArgs("in.mp4", "out.mp4", [[1, 2], [4, 5.5]], { x: 40, y: 40, width: 200, height: 160 }, 30, x264, true);
    expect(args[args.indexOf("-vf") + 1]).toBe(
      "select='gte(t,1)*lt(t,2)+gte(t,4)*lt(t,5.5)',setpts=N/(30*TB),crop=200:160:40:40,format=yuv420p",
    );
    expect(args[args.indexOf("-af") + 1]).toBe("aselect='gte(t,1)*lt(t,2)+gte(t,4)*lt(t,5.5)',asetpts=N/SR/TB");
  });
  test("沒有聲音時不加音訊參數", () => {
    const args = cutArgs("in.mp4", "out.mp4", [[0, 3]], undefined, 30, x264, false);
    expect(args).toContain("-an");
    expect(args).not.toContain("-af");
  });
  test("沒有保留任何片段會報錯", () => {
    expect(() => cutArgs("in.mp4", "out.mp4", [], undefined, 30, x264, false)).toThrow(ConfigError);
  });
});

test("剪輯版檔名，且之後的加速版會歸在剪輯版底下", () => {
  expect(cutFileName("Rec_2026-10-06_08-00-00.mp4")).toBe("Rec_2026-10-06_08-00-00_cut.mp4");
  expect(parseExportName("Rec_2026-10-06_08-00-00_cut.mp4")).toBeUndefined(); // 剪輯版本身是獨立的錄影
  expect(parseExportName("Rec_2026-10-06_08-00-00_cut_4x.mp4")).toEqual({ base: "Rec_2026-10-06_08-00-00_cut.mp4", speed: 4, format: "mp4" });
});
