/** 產生 exe 圖示 assets/icon.ico（與系統匣待命圖示相同的設計） */
import { mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { icoFile } from "../src/icon.ts";

const dir = resolve(import.meta.dir, "..", "assets");
mkdirSync(dir, { recursive: true });
writeFileSync(resolve(dir, "icon.ico"), icoFile("idle"));
console.log(`已產生 ${resolve(dir, "icon.ico")}`);
