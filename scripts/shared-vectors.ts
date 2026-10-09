/** 以 TypeScript 的計算結果重新產生 src/shared/vectors.json（說明見 src/shared/vectors.ts） */
import { resolve } from "node:path";
import { computeVectors } from "../src/shared/vectors.ts";

const file = resolve(import.meta.dir, "..", "src", "shared", "vectors.json");
// 每個測試案例一行，方便閱讀與比對差異
const v = computeVectors();
const body = Object.entries(v)
  .map(([name, cases]) => `  ${JSON.stringify(name)}: [\n${cases.map((c) => `    ${JSON.stringify(c)}`).join(",\n")}\n  ]`)
  .join(",\n");
await Bun.write(file, `{\n${body}\n}\n`);
console.log(`已產生 ${file}`);
