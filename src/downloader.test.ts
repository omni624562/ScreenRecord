import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { isEncoderFault, startupFallback } from "./args.ts";
import { FfmpegDownloader, type DownloaderDeps } from "./downloader.ts";
import type { RunResult } from "./ffmpeg.ts";

// ───────────── 下載器（假的網路與指令，不真的連網） ─────────────

const ZIP = new TextEncoder().encode("fake zip content ".repeat(1000));
const sha = (b: Uint8Array) => new Bun.CryptoHasher("sha256").update(b).digest("hex");
const ENTRY = "ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe";
const dirs: string[] = [];
afterEach(() => {
  for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

function setup(over: Partial<DownloaderDeps> = {}) {
  const dir = mkdtempSync(join(tmpdir(), "sr-dl-test-"));
  dirs.push(dir);
  const target = join(dir, "out", "ffmpeg.exe");
  const fetched: string[] = [];
  let doneCalls = 0;
  /** 模擬 tar（列出 / 解壓）與執行 ffmpeg -version */
  const run = async (cmd: string[]): Promise<RunResult> => {
    const ok = (stdout = ""): RunResult => ({ code: 0, stdout, stderr: "", timedOut: false });
    if (cmd[1] === "-tf") return ok(`ffmpeg-9.0.2-essentials_build/\n${ENTRY}\n`);
    if (cmd[1] === "-xf") {
      const out = join(cmd[4]!, ...ENTRY.split("/"));
      mkdirSync(dirname(out), { recursive: true });
      writeFileSync(out, "FAKE-FFMPEG-EXE");
      return ok();
    }
    return ok("ffmpeg version 9.0.2-test Copyright");
  };
  const dl = new FfmpegDownloader({
    urls: ["https://primary/ffmpeg.zip", "https://mirror/ffmpeg.zip"],
    sha256: sha(ZIP),
    tar: "tar.exe",
    target: () => target,
    run,
    fetch: async (url) => {
      fetched.push(url);
      return new Response(ZIP, { headers: { "content-length": String(ZIP.length) } });
    },
    onDone: async () => {
      doneCalls++;
    },
    ...over,
  });
  return { dl, target, fetched, done: () => doneCalls };
}

describe("FfmpegDownloader", () => {
  test("成功：比對 SHA-256、解出 ffmpeg.exe 放到定位並通知重新偵測", async () => {
    const { dl, target, fetched, done } = setup();
    dl.start();
    await dl.job;
    expect(dl.status().phase).toBe("done");
    expect(dl.status().version).toBe("9.0.2-test");
    expect(readFileSync(target, "utf8")).toBe("FAKE-FFMPEG-EXE");
    expect(existsSync(`${target}.download`)).toBe(false);
    expect(fetched).toEqual(["https://primary/ffmpeg.zip"]);
    expect(done()).toBe(1);
  });

  test("主來源內容不符時改用備用來源", async () => {
    const fetched: string[] = [];
    const { dl, target } = setup({
      fetch: async (url) => {
        fetched.push(url);
        return new Response(url.includes("primary") ? new TextEncoder().encode("tampered") : ZIP);
      },
    });
    dl.start();
    await dl.job;
    expect(dl.status().phase).toBe("done");
    expect(existsSync(target)).toBe(true);
    expect(fetched).toEqual(["https://primary/ffmpeg.zip", "https://mirror/ffmpeg.zip"]);
  });

  test("所有來源的 SHA-256 都不符：失敗且不留下任何檔案", async () => {
    const { dl, target } = setup({ fetch: async () => new Response(new TextEncoder().encode("tampered")) });
    dl.start();
    await dl.job;
    expect(dl.status().phase).toBe("error");
    expect(dl.status().message).toContain("SHA-256");
    expect(existsSync(target)).toBe(false);
  });

  test("下載中取消：狀態為已取消且不留下檔案", async () => {
    const { dl, target } = setup({
      fetch: async (_url, { signal }) => {
        const body = new ReadableStream<Uint8Array>({
          async pull(c) {
            await Bun.sleep(20);
            if (signal.aborted) return c.error(new DOMException("aborted", "AbortError"));
            c.enqueue(new Uint8Array(1024));
          },
        });
        return new Response(body);
      },
    });
    dl.start();
    await Bun.sleep(100);
    expect(dl.status().phase).toBe("downloading");
    dl.cancel();
    await dl.job;
    expect(dl.status().phase).toBe("canceled");
    expect(existsSync(target)).toBe(false);
  });

  test("安裝成功但重新偵測失敗：仍視為已安裝", async () => {
    const { dl, target } = setup({ onDone: async () => Promise.reject(new Error("refresh boom")) });
    dl.start();
    await dl.job;
    expect(dl.status().phase).toBe("done");
    expect(existsSync(target)).toBe(true);
  });

  test("壓縮檔裡沒有 ffmpeg.exe：回報錯誤", async () => {
    const { dl, target } = setup({ run: async () => ({ code: 0, stdout: "readme.txt\n", stderr: "", timedOut: false }) });
    dl.start();
    await dl.job;
    expect(dl.status().phase).toBe("error");
    expect(dl.status().message).toContain("bin/ffmpeg.exe");
    expect(existsSync(target)).toBe(false);
  });
});

// ───────────── 錄影開頭失敗時的退回策略（用 FFmpeg 實際印出的錯誤訊息） ─────────────

const NVENC_FAIL = `[h264_nvenc @ 0000023ef7e6d940] Cannot load nvEncodeAPI64.dll
[h264_nvenc @ 0000023ef7e6d940] The minimum required Nvidia driver for nvenc is 610.00 or newer
[vost#0:0/h264_nvenc @ 0000023ef7e6c6c0] [enc:h264_nvenc @ 0000023ef7a04e00] Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.
[vost#0:0/h264_nvenc @ 0000023ef7e6c6c0] [enc:h264_nvenc @ 0000023ef7a04e00] Could not open encoder before EOF`;

const DDAGRAB_FAIL = `[Parsed_ddagrab_0 @ 000002c19f1d4d80] Failed to enumerate DXGI output 7
[Parsed_ddagrab_0 @ 000002c19f1d4d80] Failed to configure output pad on Parsed_ddagrab_0
[fc#0 @ 000002c19f130980] Error configuring filter graph: Generic error in an external library
[vost#0:0/h264_qsv @ 000002c19f1cba80] [enc:h264_qsv @ 000002c19f1822c0] Could not open encoder before EOF`;

describe("startupFallback", () => {
  const base = { gpuEncoderInUse: true, encoderAuto: true, hasCpuEncoder: true, ddagrabInUse: true, methodAuto: true };

  test("分辨編碼器錯誤與擷取錯誤（擷取失敗連帶的 Could not open encoder 不算）", () => {
    expect(isEncoderFault(NVENC_FAIL)).toBe(true);
    expect(isEncoderFault(DDAGRAB_FAIL)).toBe(false);
  });

  test("GPU 編碼器失敗：退回 CPU 編碼，擷取方式不變", () => {
    expect(startupFallback({ ...base, stderr: NVENC_FAIL })).toBe("cpu-encoder");
  });

  test("ddagrab 失敗：只退回 gdigrab，不連帶降級編碼器", () => {
    expect(startupFallback({ ...base, stderr: DDAGRAB_FAIL })).toBe("gdigrab");
  });

  test("判斷不出原因：先退擷取方式；已無擷取可退時再退編碼器", () => {
    expect(startupFallback({ ...base, stderr: "something odd" })).toBe("gdigrab");
    expect(startupFallback({ ...base, ddagrabInUse: false, stderr: "something odd" })).toBe("cpu-encoder");
  });

  test("使用者指定的編碼器 / 擷取方式不自動更換", () => {
    expect(startupFallback({ ...base, encoderAuto: false, methodAuto: false, stderr: NVENC_FAIL })).toBe("fatal");
    expect(startupFallback({ ...base, gpuEncoderInUse: false, ddagrabInUse: false, stderr: NVENC_FAIL })).toBe("fatal");
  });
});
