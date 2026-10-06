/** 組 FFmpeg 參數（純函式，方便測試）。 */
import { LIMITS, outputSize } from "./shared/format.ts";
import { SCALE_OPTIONS, type CaptureMethod, type MonitorInfo, type Rect, type RecordConfig } from "./shared/types.ts";

export interface EncoderSpec {
  name: string;
  /** 送進編碼器的像素格式；全部都是 4:2:0 8-bit */
  pixFmt: string;
  /** 即時錄影用（速度優先） */
  live: () => string[];
  /** 匯出加速版用（離線，畫質 / 壓縮率優先） */
  offline: () => string[];
}

export const ENCODERS: EncoderSpec[] = [
  {
    name: "libx264",
    pixFmt: "yuv420p",
    live: () => ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"],
    offline: () => ["-c:v", "libx264", "-preset", "fast", "-crf", "20"],
  },
  {
    name: "h264_nvenc",
    pixFmt: "yuv420p",
    live: () => ["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "24", "-b:v", "0"],
    offline: () => ["-c:v", "h264_nvenc", "-preset", "p6", "-rc", "vbr", "-cq", "22", "-b:v", "0"],
  },
  {
    name: "h264_qsv",
    pixFmt: "nv12",
    live: () => ["-c:v", "h264_qsv", "-preset", "veryfast", "-global_quality", "24"],
    offline: () => ["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "22"],
  },
  {
    name: "h264_amf",
    pixFmt: "nv12",
    live: () => ["-c:v", "h264_amf", "-usage", "lowlatency", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24"],
    offline: () => ["-c:v", "h264_amf", "-quality", "quality", "-rc", "cqp", "-qp_i", "20", "-qp_p", "22"],
  },
  {
    name: "h264_mf",
    pixFmt: "nv12",
    live: () => ["-c:v", "h264_mf", "-rate_control", "quality", "-quality", "75"],
    offline: () => ["-c:v", "h264_mf", "-rate_control", "quality", "-quality", "80"],
  },
];

/** 硬體編碼器（實測可用才會列入選項）；h264_mf 只在沒有 libx264 時當軟體備援 */
export const HARDWARE_ENCODERS = ["h264_nvenc", "h264_qsv", "h264_amf"];
export const encoderSpec = (name: string) => ENCODERS.find((e) => e.name === name);

/** 「自動」模式：每秒處理的像素超過 1080p60 就改用 GPU 編碼 */
export const AUTO_GPU_PIXELS_PER_SEC = 1920 * 1080 * 60;

export type EncoderPreference = "auto" | "cpu" | "gpu";

/** 擷取端（ddagrab / gdigrab / 濾鏡圖）出錯的訊息 */
const CAPTURE_ERROR = /ddagrab|gdigrab|Desktop duplication|Error configuring filter graph|Failed to capture/i;
/**
 * 編碼器本身出錯的訊息。注意擷取端失敗時 FFmpeg 也會連帶印出「Could not open encoder before EOF」，
 * 那是沒有收到畫面的結果，不代表編碼器有問題，所以不列入。
 */
const ENCODER_ERROR = /Error while opening encoder|Error initializing output stream|enc:(h264_\w+|libx264)[^\n]*(Error|fail|not (supported|available))|No (NVENC|capable) devices?|Cannot load nvEncodeAPI|MFX|\bAMF\b|Cannot load nvcuda|CUDA_ERROR/i;

/** FFmpeg 錯誤訊息是編碼器而非擷取端造成的 */
export function isEncoderFault(stderr: string): boolean {
  const lines = stderr.split(/\r?\n/).filter((l) => !/Could not open encoder before EOF/i.test(l)).join("\n");
  return ENCODER_ERROR.test(lines) && !CAPTURE_ERROR.test(lines);
}
export type StartupFallback = "cpu-encoder" | "gdigrab" | "fatal";

/**
 * 第一張畫面之前就失敗時該退回哪一項：依錯誤訊息判斷是編碼器還是擷取（ddagrab）出問題，
 * 不要因為擷取失敗（例如鎖定畫面）就連編碼器也一起降級。
 * 判斷不出來時，先退擷取方式（較常見），之後若仍失敗再退編碼器。
 */
export function startupFallback(opts: {
  stderr: string;
  gpuEncoderInUse: boolean;
  encoderAuto: boolean;
  hasCpuEncoder: boolean;
  ddagrabInUse: boolean;
  methodAuto: boolean;
}): StartupFallback {
  const canCpu = opts.gpuEncoderInUse && opts.encoderAuto && opts.hasCpuEncoder;
  const canGdi = opts.ddagrabInUse && opts.methodAuto;
  const encoderFault = isEncoderFault(opts.stderr);
  if (encoderFault && canCpu) return "cpu-encoder";
  if (canGdi) return "gdigrab";
  if (canCpu) return "cpu-encoder";
  return "fatal";
}

/**
 * 決定這次錄影的編碼器。
 * @param cpu       軟體編碼器（通常是 libx264）
 * @param gpu       實測可用的硬體編碼器（依優先順序）
 * @param learned   之前在「自動」模式偵測到 CPU 跟不上，之後直接用 GPU
 */
export function chooseEncoder(
  pref: EncoderPreference,
  outWidth: number,
  outHeight: number,
  fps: number,
  cpu: EncoderSpec | undefined,
  gpu: EncoderSpec[],
  learned = false,
): { spec: EncoderSpec; reason: string } {
  if (pref === "gpu") {
    if (!gpu[0]) throw new ConfigError("這台電腦沒有可用的 GPU 編碼器，請改用 CPU 或自動");
    return { spec: gpu[0], reason: "指定使用 GPU 編碼" };
  }
  if (pref === "auto" && gpu[0]) {
    if (outWidth * outHeight * fps > AUTO_GPU_PIXELS_PER_SEC) return { spec: gpu[0], reason: "畫面量超過 1080p60，自動改用 GPU 編碼" };
    if (learned) return { spec: gpu[0], reason: "先前偵測到 CPU 編碼跟不上，自動改用 GPU 編碼" };
  }
  const spec = cpu ?? gpu[0];
  if (!spec) throw new ConfigError("FFmpeg 沒有可用的 H.264 編碼器");
  return { spec, reason: cpu ? "CPU 編碼" : "沒有 CPU 編碼器，改用 GPU 編碼" };
}

const COLOR_TAGS =["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"];
const AUDIO_ENCODE = ["-c:a", "aac", "-b:a", "160k"];

/** ddagrab 擷取的一塊：某個螢幕與擷取範圍的交集 */
export interface Tile {
  output: number;
  /** 相對於該螢幕左上角 */
  offsetX: number;
  offsetY: number;
  width: number;
  height: number;
  /** 是否為整個螢幕（不需指定 offset / video_size） */
  full: boolean;
  /** 在擷取範圍（輸出畫面）內的位置 */
  x: number;
  y: number;
}

export interface CapturePlan {
  /** 擷取範圍（虛擬桌面絕對座標，gdigrab 用） */
  rect: Rect;
  /**
   * ddagrab：範圍內每個螢幕各擷取一塊，多塊時用 xstack 拼回原本的位置。
   * 範圍跨越不同顯示卡上的螢幕時無法使用（undefined），改用 gdigrab。
   */
  dda?: { adapter: number; tiles: Tile[] };
  /** 範圍涵蓋到的螢幕 */
  monitors: MonitorInfo[];
  outWidth: number;
  outHeight: number;
}

export class ConfigError extends Error {}

const contains = (m: MonitorInfo, r: Rect) =>
  r.x >= m.x && r.y >= m.y && r.x + r.width <= m.x + m.width && r.y + r.height <= m.y + m.height;

const intersects = (m: MonitorInfo, r: Rect) =>
  r.x < m.x + m.width && r.x + r.width > m.x && r.y < m.y + m.height && r.y + r.height > m.y;

/** 所有螢幕的聯集（= Windows 虛擬桌面範圍） */
export function desktopRect(monitors: MonitorInfo[]): Rect {
  if (monitors.length === 0) return { x: 0, y: 0, width: 0, height: 0 };
  const x = Math.min(...monitors.map((m) => m.x));
  const y = Math.min(...monitors.map((m) => m.y));
  const r = Math.max(...monitors.map((m) => m.x + m.width));
  const b = Math.max(...monitors.map((m) => m.y + m.height));
  return { x, y, width: r - x, height: b - y };
}

/** 依擷取範圍切出每個螢幕的 ddagrab 區塊 */
export function planTiles(rect: Rect, monitors: MonitorInfo[]): { involved: MonitorInfo[]; dda?: CapturePlan["dda"] } {
  const involved = monitors.filter((m) => intersects(m, rect));
  const tiles: Tile[] = involved.map((m) => {
    const ix = Math.max(rect.x, m.x);
    const iy = Math.max(rect.y, m.y);
    const w = Math.min(rect.x + rect.width, m.x + m.width) - ix;
    const h = Math.min(rect.y + rect.height, m.y + m.height) - iy;
    return {
      output: m.output,
      offsetX: ix - m.x,
      offsetY: iy - m.y,
      width: w,
      height: h,
      full: ix === m.x && iy === m.y && w === m.width && h === m.height,
      x: ix - rect.x,
      y: iy - rect.y,
    };
  });
  const adapters = new Set(involved.map((m) => m.adapter));
  const dda = involved.length > 0 && adapters.size === 1
    ? { adapter: involved[0]!.adapter, tiles: tiles.filter((t) => t.width >= 2 && t.height >= 2) }
    : undefined;
  return { involved, dda };
}

/** 驗證設定並算出擷取範圍。錯誤訊息直接顯示在介面上。 */
export function resolvePlan(config: RecordConfig, monitors: MonitorInfo[]): CapturePlan {
  const { fps, scale, maxMinutes } = config;
  if (!Number.isInteger(fps) || fps < LIMITS.fpsMin || fps > LIMITS.fpsMax)
    throw new ConfigError(`錄影 FPS 需為 ${LIMITS.fpsMin}～${LIMITS.fpsMax} 的整數`);
  if (!(SCALE_OPTIONS as readonly number[]).includes(scale)) throw new ConfigError("解析度縮放只能是 100 / 75 / 50 / 25%");
  if (!Number.isFinite(maxMinutes) || maxMinutes < 0 || maxMinutes > LIMITS.maxMinutesMax)
    throw new ConfigError("最長錄影時間設定不正確");
  if (!config.outputDir?.trim()) throw new ConfigError("請指定儲存位置");

  let rect: Rect;
  if (config.source.type === "monitor") {
    const id = config.source.monitorId;
    const m = monitors.find((x) => x.id === id);
    if (!m) throw new ConfigError("找不到選擇的螢幕，請重新整理螢幕清單");
    rect = { x: m.x, y: m.y, width: m.width, height: m.height };
  } else if (config.source.type === "all") {
    if (monitors.length === 0) throw new ConfigError("找不到任何螢幕");
    rect = desktopRect(monitors);
  } else {
    const { x, y, width, height } = config.source;
    if (![x, y, width, height].every(Number.isInteger)) throw new ConfigError("範圍座標必須是整數");
    if (width < 16 || height < 16) throw new ConfigError("範圍寬高至少 16 像素");
    rect = { x, y, width, height };
  }

  const { involved, dda } = planTiles(rect, monitors);
  if (monitors.length > 0 && involved.length === 0) throw new ConfigError("範圍不在任何螢幕上");
  const out = outputSize(rect.width, rect.height, scale);
  return { rect, dda, monitors: involved, outWidth: out.width, outHeight: out.height };
}

/**
 * ddagrab 來源（GPU 擷取後下載回系統記憶體）。多個螢幕時以 xstack 依實際位置拼接，
 * 螢幕之間沒有畫面的區域（例如高度不同的螢幕）補黑色。
 */
function ddagrabChain(plan: CapturePlan, fps: number, drawMouse: boolean): string {
  const { tiles } = plan.dda!;
  const src = (t: Tile) => {
    const p = [`ddagrab=output_idx=${t.output}`, `framerate=${fps}`, `draw_mouse=${drawMouse ? 1 : 0}`];
    if (!t.full) p.push(`offset_x=${t.offsetX}`, `offset_y=${t.offsetY}`, `video_size=${t.width}x${t.height}`);
    return `${p.join(":")},hwdownload,format=bgra`;
  };
  if (tiles.length === 1) return src(tiles[0]!);

  const { width: W, height: H } = plan.rect;
  const right = Math.max(...tiles.map((t) => t.x + t.width));
  const bottom = Math.max(...tiles.map((t) => t.y + t.height));
  const labels = tiles.map((_, i) => `[t${i}]`);
  const parts = tiles.map((t, i) => `${src(t)}${labels[i]}`);
  let stack = `${labels.join("")}xstack=inputs=${tiles.length}:layout=${tiles.map((t) => `${t.x}_${t.y}`).join("|")}:fill=black`;
  if (right < W || bottom < H) stack += `,pad=${W}:${H}:0:0:color=black`;
  return `${parts.join(";")};${stack}`;
}

/** 裁成偶數 → 縮放 + 色彩轉換（BT.709 limited） */
function convertFilters(plan: CapturePlan, config: RecordConfig, enc: EncoderSpec): string[] {
  const f: string[] = [];
  const { width, height } = plan.rect;
  if (config.scale === 100 && (plan.outWidth !== width || plan.outHeight !== height)) {
    f.push(`crop=${plan.outWidth}:${plan.outHeight}:0:0`); // 奇數寬高時裁掉 1px，避免 100% 也重新取樣
  }
  f.push(`scale=${plan.outWidth}:${plan.outHeight}:flags=bicubic:out_color_matrix=bt709:out_range=tv`);
  f.push(`format=${enc.pixFmt}`);
  return f;
}

export interface CaptureSpec {
  /** 放在所有輸入之前的參數（硬體裝置） */
  pre: string[];
  /** 視訊輸入（gdigrab）；ddagrab 是濾鏡來源，沒有輸入檔 */
  inputs: string[];
  /** 視訊輸入檔數量（之後的音訊輸入編號從這裡開始） */
  inputCount: number;
  /** -filter_complex，輸出標籤為 [vout] */
  graph: string;
}

/**
 * @param probe 在擷取後插入 showinfo，每張畫面印一行 pts，供聲音對齊畫面時間零點
 */
export function captureSpec(plan: CapturePlan, config: RecordConfig, method: CaptureMethod, enc: EncoderSpec, probe = false): CaptureSpec {
  const tail = [...(probe ? ["showinfo=checksum=0"] : []), ...convertFilters(plan, config, enc)].join(",");

  if (method === "ddagrab") {
    if (!plan.dda) throw new ConfigError("此範圍涵蓋不同顯示卡上的螢幕，無法使用 ddagrab");
    return {
      pre: ["-init_hw_device", `d3d11va=dda:${plan.dda.adapter}`, "-filter_hw_device", "dda"],
      inputs: [],
      inputCount: 0,
      graph: `${ddagrabChain(plan, config.fps, config.drawMouse)},${tail}[vout]`,
    };
  }

  const { x, y, width, height } = plan.rect;
  return {
    pre: [],
    inputs: [
      "-f", "gdigrab", "-framerate", String(config.fps), "-draw_mouse", String(config.drawMouse ? 1 : 0),
      "-offset_x", String(x), "-offset_y", String(y), "-video_size", `${width}x${height}`,
      "-i", "desktop",
    ],
    inputCount: 1,
    graph: `[0:v]${tail}[vout]`,
  };
}

/**
 * 單一分段的完整參數。stdin 送 q 收尾；-progress 從 stdout 回報張數與大小。
 * @param audioInput 錄聲音時 AudioPipe 提供的輸入參數（TCP raw PCM）
 */
export function segmentArgs(
  plan: CapturePlan,
  config: RecordConfig,
  method: CaptureMethod,
  enc: EncoderSpec,
  outFile: string,
  audioInput?: string[],
): string[] {
  const spec = captureSpec(plan, config, method, enc, !!audioInput);
  return [
    // 錄聲音時需要 showinfo（info 等級）的輸出；level 前綴用來分辨錯誤訊息
    "-hide_banner", "-nostats", "-loglevel", audioInput ? "level+info" : "error",
    ...spec.pre,
    ...spec.inputs,
    ...(audioInput ?? []),
    "-filter_complex", spec.graph,
    "-map", "[vout]",
    ...(audioInput ? ["-map", `${spec.inputCount}:a`, ...AUDIO_ENCODE, "-max_muxing_queue_size", "4096"] : []),
    ...enc.live(),
    "-r", String(config.fps), "-fps_mode", "cfr",
    "-g", String(config.fps * 2),
    ...COLOR_TAGS,
    // 分段寫成每秒一個 fragment：即使 FFmpeg 被強制結束或當機，最多只損失最後 1 秒
    "-movflags", "+empty_moov+default_base_moof", "-frag_duration", "1000000",
    "-flush_packets", "1",
    "-progress", "pipe:1", "-stats_period", "0.5",
    "-y", outFile,
  ];
}

/** concat demuxer 清單；單引號需跳脫為 '\'' */
export function concatList(files: string[]): string {
  return files.map((f) => `file '${f.replaceAll("\\", "/").replaceAll("'", "'\\''")}'`).join("\n") + "\n";
}

export function concatArgs(listFile: string, outFile: string): string[] {
  return [
    "-hide_banner", "-nostats", "-loglevel", "error",
    "-f", "concat", "-safe", "0", "-i", listFile,
    "-map", "0", "-c", "copy", "-movflags", "+faststart",
    "-y", outFile,
  ];
}

/** atempo 串接：每段不超過 2 倍，變速不變調且音質較好 */
export function atempoChain(speed: number): string {
  const f: string[] = [];
  let s = speed;
  while (s > 2) {
    f.push("atempo=2");
    s /= 2;
  }
  if (s > 1.0001) f.push(`atempo=${Math.round(s * 1e6) / 1e6}`);
  return f.join(",");
}

/**
 * 加速匯出：setpts 壓縮時間軸，fps 維持原本的幀率（多出來的幀直接捨棄，
 * 不做混合，螢幕上的文字才不會出現殘影）；聲音以 atempo 變速不變調。
 */
export function exportArgs(source: string, outFile: string, speed: number, fps: number, enc: EncoderSpec, withAudio = false): string[] {
  if (!(speed >= LIMITS.speedMin && speed <= LIMITS.speedMax)) throw new ConfigError(`倍率需介於 ${LIMITS.speedMin}～${LIMITS.speedMax}`);
  return [
    "-hide_banner", "-nostats", "-loglevel", "error",
    "-i", source,
    "-map", "0:v:0", ...(withAudio ? ["-map", "0:a:0"] : ["-an"]), "-sn", "-dn",
    "-vf", `setpts=PTS/${speed},fps=${fps},format=${enc.pixFmt}`,
    ...(withAudio ? ["-af", atempoChain(speed), ...AUDIO_ENCODE] : []),
    ...enc.offline(),
    "-g", String(fps * 2),
    ...COLOR_TAGS,
    "-movflags", "+faststart",
    "-progress", "pipe:1", "-stats_period", "0.5",
    "-y", outFile,
  ];
}

/**
 * 剪輯：只保留 keep 區段（select / aselect 精確到每張畫面，並把時間戳重新接起來），
 * 可再裁切畫面範圍。因為要精準剪在任意時間點並裁切，必須重新編碼。
 */
export function cutArgs(
  source: string,
  outFile: string,
  keep: [number, number][],
  crop: Rect | undefined,
  fps: number,
  enc: EncoderSpec,
  withAudio: boolean,
): string[] {
  if (keep.length === 0) throw new ConfigError("剪輯後沒有留下任何片段");
  // 單引號內的逗號不會被當成濾鏡分隔
  const expr = keep.map(([a, b]) => `gte(t,${a})*lt(t,${b})`).join("+");
  const vf = [`select='${expr}'`, `setpts=N/(${fps}*TB)`];
  if (crop) vf.push(`crop=${crop.width}:${crop.height}:${crop.x}:${crop.y}`);
  vf.push(`format=${enc.pixFmt}`);
  return [
    "-hide_banner", "-nostats", "-loglevel", "error",
    "-i", source,
    "-map", "0:v:0", ...(withAudio ? ["-map", "0:a:0"] : ["-an"]), "-sn", "-dn",
    "-vf", vf.join(","),
    ...(withAudio ? ["-af", `aselect='${expr}',asetpts=N/SR/TB`, ...AUDIO_ENCODE] : []),
    ...enc.offline(),
    "-r", String(fps), "-fps_mode", "cfr",
    "-g", String(fps * 2),
    ...COLOR_TAGS,
    "-movflags", "+faststart",
    "-progress", "pipe:1", "-stats_period", "0.5",
    "-y", outFile,
  ];
}

/** 解析 `ffmpeg -i file` 的 stderr：長度、解析度、fps、有無聲音 */
export function parseMediaInfo(stderr: string): { durationSec?: number; width?: number; height?: number; fps?: number; hasAudio: boolean } {
  const d = /Duration:\s*(\d+):(\d{2}):(\d{2}(?:\.\d+)?)/.exec(stderr);
  const video = /Stream #\S+.*?Video:.*$/m.exec(stderr)?.[0] ?? "";
  const size = /,\s*(\d{2,5})x(\d{2,5})[\s,\[]/.exec(video);
  const fps = /,\s*(\d+(?:\.\d+)?)\s*fps/.exec(video);
  return {
    durationSec: d ? Number(d[1]) * 3600 + Number(d[2]) * 60 + Number(d[3]) : undefined,
    width: size ? Number(size[1]) : undefined,
    height: size ? Number(size[2]) : undefined,
    fps: fps ? Number(fps[1]) : undefined,
    hasAudio: /Stream #\S+.*?Audio:/.test(stderr),
  };
}

/**
 * 預覽截圖（JPEG 到 stdout）：傳入的螢幕清單決定範圍（整個桌面，或只有選到的那一台）。
 * 能用 ddagrab 時與錄影走同一條路徑，混合 DPI 的多螢幕環境下預覽與實際錄到的畫面才會一致；否則用 gdigrab。
 */
export function previewArgs(monitors: MonitorInfo[], useDdagrab: boolean, maxWidth = 1600, liveFps?: number): string[] {
  // 單張：JPEG 到 stdout；即時：以 mpjpeg（multipart/x-mixed-replace）持續輸出，<img> 可直接顯示
  const out = liveFps
    ? ["-c:v", "mjpeg", "-q:v", "6", "-f", "mpjpeg", "-flush_packets", "1", "-"]
    : ["-frames:v", "1", "-c:v", "mjpeg", "-q:v", "4", "-f", "image2pipe", "-"];
  const fps = liveFps ?? 10;
  const fit = `scale='min(${maxWidth},iw)':-2:flags=bilinear,format=yuvj420p`;
  const rect = desktopRect(monitors);
  const { dda } = planTiles(rect, monitors);
  if (useDdagrab && dda && dda.tiles.length) {
    const plan: CapturePlan = { rect, dda, monitors, outWidth: rect.width, outHeight: rect.height };
    return [
      "-hide_banner", "-loglevel", "error",
      "-init_hw_device", `d3d11va=dda:${dda.adapter}`, "-filter_hw_device", "dda",
      "-filter_complex", `${ddagrabChain(plan, fps, true)},${fit}[vout]`, "-map", "[vout]",
      ...out,
    ];
  }
  return [
    "-hide_banner", "-loglevel", "error",
    "-f", "gdigrab", "-framerate", String(fps), "-draw_mouse", "1",
    ...(monitors.length === 1 ? ["-offset_x", String(rect.x), "-offset_y", String(rect.y), "-video_size", `${rect.width}x${rect.height}`] : []),
    "-i", "desktop",
    "-vf", fit, ...out,
  ];
}
