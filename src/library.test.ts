import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { listLibrary } from "./library.ts";

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
