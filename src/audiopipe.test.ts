import { expect, test } from "bun:test";
import { CHANNELS } from "./audio.ts";
import { Ring } from "./audiopipe.ts";

/** 第 i 個取樣的左右聲道 = [i, -i] */
const seq = (from: number, frames: number) => {
  const a = new Float32Array(frames * CHANNELS);
  for (let i = 0; i < frames; i++) {
    a[i * 2] = from + i;
    a[i * 2 + 1] = -(from + i);
  }
  return a;
};

test("環狀緩衝：繞回、靜音、依序取出", () => {
  const r = new Ring(10);
  const out = new Float32Array(20 * CHANNELS);
  r.push(seq(0, 6), 6);
  r.take(out, 4, false);
  expect([...out.subarray(0, 8)]).toEqual([...seq(0, 4)]);
  r.push(seq(6, 5), 5); // 寫入時繞回開頭
  r.push(null, 1); // 靜音
  expect(r.size).toBe(8);
  r.take(out, 8, false);
  expect([...out.subarray(0, 14)]).toEqual([...seq(4, 7)]);
  expect([...out.subarray(14, 16)]).toEqual([0, 0]);
  expect(r.size).toBe(0);
});

test("環狀緩衝：超過容量時自動加大，資料不亂", () => {
  const r = new Ring(4);
  r.push(seq(0, 3), 3);
  const out = new Float32Array(20 * CHANNELS);
  r.take(out, 2, false);
  r.push(seq(3, 12), 12); // 遠超過容量 4
  expect(r.size).toBe(13);
  r.take(out, 13, false);
  expect([...out.subarray(0, 26)]).toEqual([...seq(2, 13)]);
});

test("環狀緩衝：混音（相加）", () => {
  const a = new Ring(8), b = new Ring(8);
  a.push(seq(1, 3), 3);
  b.push(seq(10, 3), 3);
  const out = new Float32Array(3 * CHANNELS);
  a.take(out, 3, false);
  b.take(out, 3, true);
  expect([...out]).toEqual([11, -11, 13, -13, 15, -15]);
});
