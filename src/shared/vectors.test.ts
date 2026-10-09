import { expect, test } from "bun:test";
import saved from "./vectors.json";
import { computeVectors } from "./vectors.ts";

test("共用計算的結果與 vectors.json 一致（Rust 版用同一份資料測試）", () => {
  // 失敗時：確認改動是刻意的，執行 bun run scripts/shared-vectors.ts 更新，並讓 core/src/format.rs、edit.rs 跟上
  expect(computeVectors()).toEqual(saved as unknown as ReturnType<typeof computeVectors>);
});
