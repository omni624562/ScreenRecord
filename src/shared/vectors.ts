/**
 * 介面（TypeScript）與後端（Rust，core/src/format.rs、edit.rs）共用的計算，兩邊結果必須一致。
 * 這裡列出測試輸入，以 TypeScript 的結果為準寫進 vectors.json；
 * bun test 確認 TypeScript 的結果沒變、cargo test（core/tests/shared_vectors.rs）確認 Rust 算出一樣的結果。
 * 改了計算規則時：bun run scripts/shared-vectors.ts 重新產生 vectors.json，再讓 Rust 版跟上。
 */
import { cutFileName, keepRanges, normalizeCrop, normalizeRanges, totalLength, type Range } from "./edit.ts";
import {
  checkRecordingName, clock, estimateBytes, exportFileName, formatBytes, humanDuration, outputSize, parseClock,
  parseExportName, scaledSize, speedForTarget, speedLabel, videoClock, type EstimateInput,
} from "./format.ts";
import type { Rect } from "./types.ts";

type Case = { args: unknown[]; out: unknown };

/** undefined 存成 null（JSON 沒有 undefined） */
const norm = (v: unknown): unknown => (v === undefined ? null : JSON.parse(JSON.stringify(v)));
const run = <A extends unknown[]>(inputs: A[], fn: (...a: A) => unknown): Case[] => inputs.map((args) => ({ args: norm(args) as unknown[], out: norm(fn(...args)) }));

export function computeVectors(): Record<string, Case[]> {
  const estimate = (o: EstimateInput) => estimateBytes(o);
  return {
    outputSize: run([[1921, 1081, 100], [1366, 768, 75], [10, 10, 25], [3840, 2160, 50], [2560, 1440, 33]] as [number, number, number][], outputSize),
    speedLabel: run([[1.5], [2], [4], [3.333], [13.69], [1000], [1.1], [2.005]] as [number][], speedLabel),
    exportFileName: run(
      [["Rec_a.mp4", 4, "mp4"], ["Rec_a.mp4", 1.5, "mp4"], ["Rec_a.mp4", 1, "gif"], ["Rec_a.mp4", 4, "gif"], ["Rec_b.MP4", 3.33, "mp4"], ["我的錄影.mp4", 16, "gif"]] as [string, number, "mp4" | "gif"][],
      exportFileName,
    ),
    parseExportName: run(
      [["Rec_a_4x.mp4"], ["Rec_a_1.5x.mp4"], ["Rec_a_8x_2.mp4"], ["Rec_a.gif"], ["Rec_a_2.gif"], ["Rec_a_4x.gif"], ["Rec_a.mp4"], ["Rec_a_cut.mp4"], ["Rec_a_cut_4x.mp4"], ["X_0.5x.MP4"]] as [string][],
      parseExportName,
    ),
    scaledSize: run([[3840, 1080, 1920], [1920, 1080, 1280], [1366, 768, 1280], [1280, 720, 1920], [1920, 1080, 0], [1921, 1081, 1280]] as [number, number, number][], scaledSize),
    estimateBytes: run(
      [
        [{ format: "mp4", srcBytes: 60e6, srcSec: 600, srcWidth: 1920, srcHeight: 1080, speed: 4, width: 1920, height: 1080 }],
        [{ format: "mp4", srcBytes: 60e6, srcSec: 600, srcWidth: 1920, srcHeight: 1080, speed: 16, width: 1280, height: 720 }],
        [{ format: "mp4", srcBytes: 1e6, srcSec: 10, srcWidth: 0, srcHeight: 0, speed: 1.5, width: 640, height: 360 }],
        [{ format: "gif", srcBytes: 60e6, srcSec: 10, srcWidth: 1920, srcHeight: 1080, speed: 1, width: 640, height: 360, gifFps: 10 }],
        [{ format: "gif", srcBytes: 60e6, srcSec: 0.01, srcWidth: 1920, srcHeight: 1080, speed: 2, width: 320, height: 180 }],
      ] as [EstimateInput][],
      estimate,
    ),
    checkRecordingName: run(
      [[""], ["  "], ["a/b"], ["名稱?"], ["CON"], ["com1"], ["正常名稱"], ["a."], ["a "], ["x".repeat(121)], ["x".repeat(120)], ["Rec_a_4x"], ["Rec_a_cut"]] as [string][],
      checkRecordingName,
    ),
    parseClock: run([["1:30"], ["90"], ["1:02:03"], ["abc"], ["0"], [""], [" 2:00 "], ["00:05"], ["1:5"], ["1.5"], ["1:2:3:4"]] as [string][], parseClock),
    speedForTarget: run([[3600, 60, false], [100, 30, false], [10, 60, false], [10, 60, true], [36000, 1, false], [125, 7, false]] as [number, number, boolean][], speedForTarget),
    humanDuration: run([[0], [0.44], [5], [9.96], [59.6], [60], [61], [3599], [3600], [3725], [86400]] as [number][], humanDuration),
    clock: run([[0], [999], [1000], [61000], [3600000], [3725500], [-5]] as [number][], clock),
    videoClock: run([[0], [0.05], [9.99], [61.25], [3725.5]] as [number][], videoClock),
    formatBytes: run([[0], [1023], [1024], [1536], [1048576], [1572864000], [5e9]] as [number][], formatBytes),
    normalizeRanges: run(
      [[[[1, 2], [2, 3], [5, 5.001]], 10], [[[40, 45], [10, 20], [15, 25], [55, 70]], 60], [[[-5, 3], [8, 4]], 10], [[], 10]] as [Range[], number][],
      normalizeRanges,
    ),
    keepRanges: run(
      [
        [60, { start: 5, end: 50, removed: [] }],
        [60, { start: 2, end: 58, removed: [[40, 45], [10, 20], [15, 25], [55, 70]] }],
        [30, { start: 0, end: 30, removed: [[0, 12.5]] }],
        [10, { start: -3, end: 99, removed: [] }],
        [10, { start: 4, end: 6, removed: [[0, 10]] }],
      ] as [number, { start: number; end: number; removed: Range[] }][],
      keepRanges,
    ),
    totalLength: run([[[[2, 10], [25, 40], [45, 55]]], [[]]] as [Range[]][], totalLength),
    normalizeCrop: run(
      [
        [{ x: 101, y: 50, width: 301, height: 9999 }, 1920, 1080],
        [{ x: 0, y: 0, width: 1920, height: 1080 }, 1920, 1080],
        [{ x: 10, y: 10, width: 3, height: 3 }, 1920, 1080],
        [{ x: -20, y: 2000, width: 500, height: 400 }, 1920, 1080],
        [undefined, 1920, 1080],
      ] as [Rect | undefined, number, number][],
      normalizeCrop,
    ),
    cutFileName: run([["Rec_2026-10-06_08-00-00.mp4"], ["Rec_a.MP4"], ["我的錄影.mp4"]] as [string][], cutFileName),
  };
}
