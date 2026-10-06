/** 前後端共用的型別。 */

export interface MonitorInfo {
  /** `${adapter}:${output}` */
  id: string;
  adapter: number;
  output: number;
  adapterName: string;
  /** 例如 \\.\DISPLAY1 */
  deviceName: string;
  displayNumber: number;
  x: number;
  y: number;
  width: number;
  height: number;
  primary: boolean;
  /** DXGI_MODE_ROTATION：1 = 不旋轉 */
  rotation: number;
}

export interface AudioDevice {
  id: string;
  name: string;
  isDefault: boolean;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type CaptureMethod = "ddagrab" | "gdigrab";
export type MethodPreference = "auto" | CaptureMethod;
export const SCALE_OPTIONS = [100, 75, 50, 25] as const;
export type ScalePercent = (typeof SCALE_OPTIONS)[number];

export type SourceConfig =
  | { type: "monitor"; monitorId: string }
  /** 所有螢幕（整個延伸桌面） */
  | { type: "all" }
  | ({ type: "region" } & Rect);

export interface AudioConfig {
  /** 電腦播放的聲音（WASAPI loopback） */
  system: boolean;
  mic: boolean;
  /** 空字串 = 預設麥克風 */
  micId: string;
}

export interface RecordConfig {
  source: SourceConfig;
  fps: number;
  scale: ScalePercent;
  drawMouse: boolean;
  /** 0 = 不限 */
  maxMinutes: number;
  method: MethodPreference;
  outputDir: string;
  audio: AudioConfig;
  /** 編碼器：auto = 平常 CPU、畫面量大或 CPU 跟不上時改 GPU；未指定視為 auto */
  encoder?: "auto" | "cpu" | "gpu";
}

export type RecorderState = "idle" | "recording" | "paused" | "stopping";

export interface RecordingResult {
  ok: boolean;
  path?: string;
  frames: number;
  videoSec: number;
  bytes?: number;
  message: string;
  /** 合併失敗時保留的分段資料夾 */
  partsDir?: string;
}

export interface LogEntry {
  t: number;
  level: "info" | "warn" | "error";
  text: string;
}

export interface RecorderStatus {
  state: RecorderState;
  /** 過渡動作的說明（啟動中、收尾中、合併中…） */
  busy?: string;
  method?: CaptureMethod;
  /** 合成的螢幕數（ddagrab 跨螢幕時 > 1） */
  tiles?: number;
  encoder?: string;
  /** 錄音來源說明；undefined = 不錄聲音 */
  audio?: string;
  segments: number;
  frames: number;
  videoSec: number;
  bytes: number;
  recordedMs: number;
  maxMs: number;
  fps: number;
  outWidth: number;
  outHeight: number;
  /** 近幾秒實際寫入的不重複畫面數 / 秒（低於設定 fps 代表電腦跟不上） */
  actualFps?: number;
  slow: boolean;
  retrying?: string;
  startedAt?: number;
  result?: RecordingResult;
  log: LogEntry[];
}

export interface MediaInfo {
  path: string;
  name: string;
  bytes: number;
  mtime: number;
  durationSec?: number;
  width?: number;
  height?: number;
  fps?: number;
  hasAudio?: boolean;
}

export interface LibraryEntry extends MediaInfo {
  /** 由這支錄影匯出的加速版 */
  exports: (MediaInfo & { speed: number })[];
}

export interface LibraryQuery {
  q?: string;
  /** all = 全部、original = 原始錄影（不含剪輯版）、cut = 剪輯版、speed = 有加速版、audio = 有聲音 */
  filter?: "all" | "original" | "cut" | "speed" | "audio";
  sort?: "new" | "old" | "size" | "duration";
  page?: number;
  pageSize?: number;
}

export interface LibraryPage {
  dir: string;
  total: number;
  page: number;
  pages: number;
  pageSize: number;
  items: LibraryEntry[];
}

export type ExportState = "running" | "done" | "error" | "canceled";

export interface ExportStatus {
  id: number;
  /** speed = 加速匯出、cut = 剪輯 */
  kind: "speed" | "cut";
  state: ExportState;
  source: string;
  output: string;
  speed: number;
  /** 0～1 */
  progress: number;
  expectedSec: number;
  elapsedMs: number;
  etaSec?: number;
  bytes?: number;
  message?: string;
}

export interface DownloadStatus {
  phase: "idle" | "downloading" | "verifying" | "extracting" | "done" | "error" | "canceled";
  received: number;
  total?: number;
  /** bytes / 秒 */
  speed?: number;
  target?: string;
  version?: string;
  message?: string;
}

export interface FfmpegInfo {
  found: boolean;
  path?: string;
  version?: string;
  hasDdagrab: boolean;
  hasGdigrab: boolean;
  encoder?: string;
  /** 實測可用的硬體編碼器；undefined = 測試中 */
  hwEncoders?: string[];
  /** 「自動」模式曾偵測到 CPU 編碼跟不上，之後的錄影改用 GPU */
  preferGpu?: boolean;
  /** ddagrab 實測：undefined = 尚未測試 */
  ddagrabWorks?: boolean;
  ddagrabError?: string;
  searched: string[];
  error?: string;
}

export interface EnvInfo {
  appDir: string;
  defaultOutputDir: string;
  ffmpeg: FfmpegInfo;
  monitors: MonitorInfo[];
  monitorError?: string;
  desktop: Rect;
  audio: { render?: string; captures: AudioDevice[]; error?: string };
}
