/** 尋找 ffmpeg.exe 並偵測功能（ddagrab、gdigrab、H.264 編碼器）。 */
import { existsSync } from "node:fs";
import { join } from "node:path";
import { logDir } from "./log.ts";
import { appDir } from "./paths.ts";
import { ENCODERS, encoderSpec, HARDWARE_ENCODERS, type EncoderSpec } from "./args.ts";
import type { FfmpegInfo, MonitorInfo } from "./shared/types.ts";

export interface RunResult {
  code: number;
  stdout: string;
  stderr: string;
  timedOut: boolean;
}

/** 執行指令並收集輸出（逾時會強制結束）。 */
export async function run(cmd: string[], timeoutMs = 20_000): Promise<RunResult> {
  const p = Bun.spawn(cmd, { stdin: "ignore", stdout: "pipe", stderr: "pipe", windowsHide: true });
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    p.kill();
  }, timeoutMs);
  const [stdout, stderr, code] = await Promise.all([
    new Response(p.stdout).text(),
    new Response(p.stderr).text(),
    p.exited,
  ]);
  clearTimeout(timer);
  return { code, stdout, stderr, timedOut };
}

/** 依序尋找：程式資料夾 → 程式資料夾\ffmpeg\bin → 程式資料夾\bin → %LOCALAPPDATA%\ScreenRecorder（自動下載的位置）→ PATH */
export function locateFfmpeg(): { path?: string; searched: string[] } {
  const local = [
    join(appDir, "ffmpeg.exe"),
    join(appDir, "ffmpeg", "bin", "ffmpeg.exe"),
    join(appDir, "bin", "ffmpeg.exe"),
    join(logDir, "ffmpeg.exe"),
  ];
  for (const p of local) if (existsSync(p)) return { path: p, searched: local };
  const inPath = Bun.which("ffmpeg");
  return { path: inPath ?? undefined, searched: [...local, "PATH"] };
}

export function lastLines(text: string, n = 3): string {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean)
    .slice(-n)
    .join(" / ");
}

async function encoderWorks(ffmpeg: string, enc: EncoderSpec): Promise<boolean> {
  const r = await run([
    ffmpeg, "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=0.5",
    "-vf", `format=${enc.pixFmt}`, ...enc.live(), "-f", "null", "-",
  ], 15_000);
  return r.code === 0;
}

/**
 * 實測硬體編碼器：FFmpeg 列得出來不代表能用（例如 MX150 沒有 NVENC、沒有 Intel 內顯就不能用 QSV），
 * 實際試編一小段才算數。依 HARDWARE_ENCODERS 的順序回傳可用的名稱。
 */
export async function testHwEncoders(ffmpeg: string, listed: string[]): Promise<string[]> {
  const ok: string[] = [];
  for (const name of HARDWARE_ENCODERS) {
    const spec = encoderSpec(name);
    if (spec && listed.includes(name) && (await encoderWorks(ffmpeg, spec))) ok.push(name);
  }
  return ok;
}

export async function probeFfmpeg(): Promise<FfmpegInfo & { encoderSpec?: EncoderSpec; hwListed?: string[] }> {
  const { path, searched } = locateFfmpeg();
  const base: FfmpegInfo = { found: false, hasDdagrab: false, hasGdigrab: false, searched };
  if (!path) return base;

  try {
    const [ver, filters, devices, encoders] = await Promise.all([
      run([path, "-hide_banner", "-version"]),
      run([path, "-hide_banner", "-filters"]),
      run([path, "-hide_banner", "-devices"]),
      run([path, "-hide_banner", "-encoders"]),
    ]);
    if (ver.code !== 0) return { ...base, path, error: `無法執行 ffmpeg：${lastLines(ver.stderr)}` };

    const version = /ffmpeg version (\S+)/.exec(ver.stdout)?.[1] ?? "unknown";
    const hasDdagrab = /^\s*\S+\s+ddagrab\s/m.test(filters.stdout);
    const hasGdigrab = /^\s*D\S*\s+gdigrab\s/m.test(devices.stdout);

    // libx264 品質最好且不吃顯示卡；沒有時才實測硬體 / MediaFoundation 編碼器
    const listed = ENCODERS.filter((e) => new RegExp(`^\\s*V\\S*\\s+${e.name}\\s`, "m").test(encoders.stdout));
    let encoderSpec: EncoderSpec | undefined = listed.find((e) => e.name === "libx264");
    if (!encoderSpec) {
      for (const e of listed) {
        if (await encoderWorks(path, e)) {
          encoderSpec = e;
          break;
        }
      }
    }

    return {
      found: true,
      path,
      version,
      hasDdagrab,
      hasGdigrab,
      encoder: encoderSpec?.name,
      encoderSpec,
      hwListed: listed.filter((e) => HARDWARE_ENCODERS.includes(e.name)).map((e) => e.name),
      searched,
      error: encoderSpec ? undefined : "這個 FFmpeg 沒有可用的 H.264 編碼器（建議改用 gyan.dev 的 full / essentials 版本）",
    };
  } catch (e) {
    return { ...base, path, error: `無法執行 ffmpeg：${(e as Error).message}` };
  }
}

/** 實際用 ddagrab 抓一張畫面，確認 Desktop Duplication 在這台電腦可用。 */
export async function testDdagrab(ffmpeg: string, monitor?: MonitorInfo): Promise<{ ok: boolean; error?: string }> {
  const dev = monitor ? ["-init_hw_device", `d3d11va=dda:${monitor.adapter}`, "-filter_hw_device", "dda"] : [];
  const r = await run([
    ffmpeg, "-hide_banner", "-loglevel", "error", ...dev,
    "-filter_complex", `ddagrab=output_idx=${monitor?.output ?? 0}:framerate=5,hwdownload,format=bgra`,
    "-frames:v", "1", "-f", "null", "-",
  ], 15_000);
  if (r.code === 0 && !r.timedOut) return { ok: true };
  return { ok: false, error: r.timedOut ? "測試逾時" : lastLines(r.stderr) || `exit ${r.code}` };
}

