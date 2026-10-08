import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { listLibrary, pageStarts } from "./library.ts";
import type { LibraryEntry } from "./shared/types.ts";

const dirs: string[] = [];
afterEach(() => {
  for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

test("匯出檔歸到正確的原片底下（原速 GIF 先找同名 MP4）", async () => {
  const dir = mkdtempSync(join(tmpdir(), "sr-lib-test-"));
  dirs.push(dir);
  for (const n of ["Rec_X.mp4", "Rec_X_4x.mp4", "Rec_X_2.gif", "Rec_X_cut.mp4", "Rec_X_cut_2.mp4", "Rec_X_cut_2.gif", "Orphan.gif"])
    writeFileSync(join(dir, n), "x");
  const page = await listLibrary(undefined, dir, { pageSize: 50 });
  const byName = Object.fromEntries(page.items.map((e) => [e.name, e.exports.map((x) => `${x.name}:${x.format}:${x.speed}`)]));
  expect(Object.keys(byName).sort()).toEqual(["Rec_X.mp4", "Rec_X_cut.mp4", "Rec_X_cut_2.mp4"]); // 找不到原片的 GIF 不列出
  expect(byName["Rec_X.mp4"]).toEqual(["Rec_X_2.gif:gif:1", "Rec_X_4x.mp4:mp4:4"]);
  expect(byName["Rec_X_cut_2.mp4"]).toEqual(["Rec_X_cut_2.gif:gif:1"]);
  expect(byName["Rec_X_cut.mp4"]).toEqual([]);
});

test("依高度分頁：加速版子列也算高度，一筆放不下時自己一頁", () => {
  const e = (n: number): LibraryEntry => ({ name: "a.mp4", path: "a.mp4", bytes: 0, mtime: 0, exports: Array.from({ length: n }, () => ({ name: "a_4x.mp4", path: "a_4x.mp4", bytes: 0, mtime: 0, speed: 4 })) });
  // 原片 41px、子列 33px；高度 200：41+41+(41+33*2=107) = 189 放得下，下一筆換頁
  expect(pageStarts([e(0), e(0), e(2), e(0)], 200)).toEqual([0, 3]);
  expect(pageStarts([e(0), e(10), e(0)], 200)).toEqual([0, 1, 2]); // 子列多到放不下：自己一頁
  expect(pageStarts([], 200)).toEqual([0]);
});

test("指定 fitPx 時依高度分頁", async () => {
  const dir = mkdtempSync(join(tmpdir(), "sr-lib-test-"));
  dirs.push(dir);
  ["Rec_A.mp4", "Rec_A_4x.mp4", "Rec_A_8x.mp4", "Rec_B.mp4", "Rec_C.mp4"].forEach((n, i) => {
    writeFileSync(join(dir, n), "x");
    utimesSync(join(dir, n), 1_000_000 + i, 1_000_000 + i); // 依序遞增，排序結果固定
  });
  const p1 = await listLibrary(undefined, dir, { sort: "old", fitPx: 41 * 2 + 33 * 2 });
  expect(p1.total).toBe(3);
  expect(p1.pages).toBe(2);
  expect(p1.items.map((e) => e.name)).toEqual(["Rec_A.mp4", "Rec_B.mp4"]);
  const p2 = await listLibrary(undefined, dir, { sort: "old", fitPx: 41 * 2 + 33 * 2, page: 2 });
  expect(p2.items.map((e) => e.name)).toEqual(["Rec_C.mp4"]);
});
