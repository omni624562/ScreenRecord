/**
 * 把 WASAPI 擷取的聲音對齊畫面時間後，以 raw float32 經本機 TCP 送進 FFmpeg。
 *
 * 時間對齊：
 * - 畫面：FFmpeg 的 showinfo 每張畫面印一行 pts；畫面時間零點 ≈ min(收到該行的 QPC 時間 − pts)，
 *   取開頭約 1 秒的最小值，排除管線延遲。
 * - 聲音：每個 WASAPI 封包都有 QPC 時間戳，依「封包時間 − 畫面零點」決定它在時間軸上的位置；
 *   落後就補靜音、超前就裁掉，長時間錄影也不會因音效卡時鐘誤差而漂移。
 * - 系統聲音沒有播放時 WASAPI 不送資料，依 QPC 時鐘補靜音維持連續。
 */
import type { Socket, TCPSocketListener } from "bun";
import { CHANNELS, SAMPLE_RATE, WasapiCapture, type AudioPacket } from "./audio.ts";
import { qpcNow100ns } from "./com.ts";

const TICK_MS = 10;
/** 偏差超過 20ms 才修正（補靜音 / 裁切），避免頻繁微調 */
const TOLERANCE = Math.round(SAMPLE_RATE * 0.02);
/** 補靜音只補到「現在 − 150ms」，給尚未取出的封包留時間 */
const SILENCE_MARGIN_100NS = 1_500_000;
/** 收集畫面時間樣本的時間長度 */
const T0_WINDOW_100NS = 10_000_000;
/** 開始前最多保留的音訊 */
const PRE_BUFFER_100NS = 60_000_000;
const MAX_PENDING_BYTES = 64 * 1024 * 1024;
/** 一次要補的靜音超過這個長度，代表程式曾停住（電腦睡眠）：時間軸直接跳過，不配置大量靜音 */
const MAX_SILENCE_FRAMES = SAMPLE_RATE * 2;
const frames100ns = (d: number) => Math.round((d * SAMPLE_RATE) / 1e7);

export interface AudioSourceSpec {
  loopback: boolean;
  /** 麥克風裝置 ID；空字串 = 預設麥克風 */
  micId?: string;
}

interface Source {
  spec: AudioSourceSpec;
  label: string;
  cap?: WasapiCapture;
  reopenAt?: number;
  /** 畫面零點確定前收到的封包 */
  pre: AudioPacket[];
  /** 已排進時間軸的取樣數（每聲道） */
  written: number;
  /** 等待混音的資料 */
  queue: Float32Array[];
  queued: number;
}

type Log = (level: "info" | "warn", text: string) => void;

export class AudioPipe {
  private server: TCPSocketListener<undefined>;
  private sock?: Socket<undefined>;
  private pending: Uint8Array[] = [];
  private pendingBytes = 0;
  private ending = false;
  private closed?: () => void;
  private sources: Source[];
  private timer: Timer;
  private t0?: number;
  private firstVideoAt?: number;
  private t0Estimate = Number.POSITIVE_INFINITY;
  private overflowWarned = false;

  constructor(specs: AudioSourceSpec[], private log: Log) {
    this.sources = specs.map((spec) => ({
      spec,
      label: spec.loopback ? "系統聲音" : "麥克風",
      pre: [],
      written: 0,
      queue: [],
      queued: 0,
    }));
    for (const s of this.sources) this.open(s, true);

    this.server = Bun.listen<undefined>({
      hostname: "127.0.0.1",
      port: 0,
      socket: {
        open: (sock) => {
          if (this.sock) return void sock.end(); // 只接受 FFmpeg 的一條連線
          this.sock = sock;
          this.flush();
        },
        drain: () => this.flush(),
        data: () => {},
        close: () => {
          this.sock = undefined;
          this.closed?.();
        },
        error: () => {},
      },
    });
    this.timer = setInterval(() => this.tick(), TICK_MS);
  }

  get port() {
    return this.server.port;
  }

  /** FFmpeg 的音訊輸入參數 */
  inputArgs(): string[] {
    return [
      "-thread_queue_size", "4096",
      "-f", "f32le", "-ar", String(SAMPLE_RATE), "-ac", String(CHANNELS),
      "-i", `tcp://127.0.0.1:${this.port}`,
    ];
  }

  describe(): string {
    return this.sources.map((s) => (s.cap ? `${s.label}（${s.cap.name}）` : `${s.label}（無法使用，改錄靜音）`)).join("、");
  }

  /** showinfo 回報一張畫面：pts（秒）與收到的 QPC 時間 */
  onVideoFrame(ptsSec: number, arrival = qpcNow100ns()) {
    if (this.t0 !== undefined) return;
    this.firstVideoAt ??= arrival;
    this.t0Estimate = Math.min(this.t0Estimate, arrival - ptsSec * 1e7);
  }

  /** 停止：把聲音補到 stopAt，送完後關閉連線（FFmpeg 收到 EOF） */
  async finish(stopAt = qpcNow100ns()) {
    clearInterval(this.timer);
    if (this.t0 === undefined && this.firstVideoAt !== undefined) this.fixT0();
    if (this.t0 !== undefined) {
      this.pull(stopAt);
      for (const s of this.sources) this.fillSilence(s, frames100ns(stopAt - this.t0));
      this.mix();
    }
    for (const s of this.sources) s.cap?.close();
    this.ending = true;
    if (this.sock) {
      const closed = new Promise<void>((r) => (this.closed = r));
      this.flush();
      await Promise.race([closed, Bun.sleep(3000)]);
    }
    this.server.stop(true);
  }

  /** 立即釋放（FFmpeg 已結束時） */
  close() {
    clearInterval(this.timer);
    for (const s of this.sources) s.cap?.close();
    this.sock?.end();
    this.server.stop(true);
  }

  // ───────────── 內部 ─────────────

  private open(s: Source, first = false) {
    try {
      s.cap = new WasapiCapture(s.spec.loopback, s.spec.micId || undefined);
      s.reopenAt = undefined;
      if (!first) this.log("info", `${s.label}已恢復（${s.cap.name}）`);
    } catch (e) {
      s.cap = undefined;
      s.reopenAt = qpcNow100ns() + 30_000_000;
      if (first) this.log("warn", `${s.label}無法開啟：${(e as Error).message}，這段先以靜音代替`);
    }
  }

  private tick() {
    const now = qpcNow100ns();
    if (this.t0 === undefined && this.firstVideoAt !== undefined && now - this.firstVideoAt >= T0_WINDOW_100NS) this.fixT0();
    this.pull(now);
    if (this.t0 === undefined) return;
    const target = frames100ns(now - SILENCE_MARGIN_100NS - this.t0);
    const behind = target - Math.max(...this.sources.map((s) => s.written));
    if (behind > MAX_SILENCE_FRAMES) {
      // 睡眠一小時要補約 1.4 GB 的靜音；改為把零點往後移，錄影端也會在恢復後重開分段
      this.t0 += Math.round((behind * 1e7) / SAMPLE_RATE);
      this.log("warn", `聲音中斷約 ${Math.round(behind / SAMPLE_RATE)} 秒（電腦睡眠？），已跳過這段`);
    }
    const capped = frames100ns(now - SILENCE_MARGIN_100NS - this.t0);
    for (const s of this.sources) this.fillSilence(s, capped);
    this.mix();
  }

  private fixT0() {
    this.t0 = this.t0Estimate;
    for (const s of this.sources) {
      const pre = s.pre;
      s.pre = [];
      for (const p of pre) this.place(s, p);
    }
  }

  /** 讀取所有來源的新封包；裝置失效時關閉並定期嘗試重新開啟 */
  private pull(now: number) {
    for (const s of this.sources) {
      if (!s.cap) {
        if (s.reopenAt !== undefined && now >= s.reopenAt) this.open(s);
        if (!s.cap) continue;
      }
      let packets: AudioPacket[];
      try {
        packets = s.cap.read();
      } catch (e) {
        this.log("warn", `${s.label}中斷：${(e as Error).message}，先以靜音代替並嘗試重新連接`);
        s.cap.close();
        s.cap = undefined;
        s.reopenAt = now + 10_000_000;
        continue;
      }
      if (this.t0 === undefined) {
        s.pre.push(...packets);
        while (s.pre.length && s.pre[0]!.qpc !== undefined && now - s.pre[0]!.qpc! > PRE_BUFFER_100NS) s.pre.shift();
      } else for (const p of packets) this.place(s, p);
    }
  }

  /** 依封包時間戳放進時間軸 */
  private place(s: Source, p: AudioPacket) {
    const frames = p.data.length / CHANNELS;
    const pos = p.qpc === undefined ? s.written : frames100ns(p.qpc - this.t0!);
    if (pos > s.written + TOLERANCE) this.fillSilence(s, pos);
    let skip = 0;
    if (pos < s.written - TOLERANCE) skip = s.written - pos; // 與已寫入的資料重疊（通常是零點之前的聲音）
    if (skip >= frames) return;
    this.append(s, skip ? p.data.subarray(skip * CHANNELS) : p.data);
  }

  private fillSilence(s: Source, upTo: number) {
    if (upTo > s.written) this.append(s, new Float32Array((upTo - s.written) * CHANNELS));
  }

  private append(s: Source, data: Float32Array) {
    s.queue.push(data);
    const n = data.length / CHANNELS;
    s.queued += n;
    s.written += n;
  }

  /** 所有來源都有資料的部分相加後送出 */
  private mix() {
    const n = Math.min(...this.sources.map((s) => s.queued));
    if (!(n > 0)) return;
    let out: Float32Array | undefined;
    for (const s of this.sources) {
      const chunk = take(s, n);
      if (!out) out = chunk;
      else for (let i = 0; i < out.length; i++) out[i]! += chunk[i]!;
    }
    this.write(new Uint8Array(out!.buffer, out!.byteOffset, out!.byteLength));
  }

  private write(bytes: Uint8Array) {
    if (this.pendingBytes + bytes.length > MAX_PENDING_BYTES) {
      if (!this.overflowWarned) this.log("warn", "FFmpeg 沒有在讀取聲音資料，部分聲音已捨棄");
      this.overflowWarned = true;
      return;
    }
    this.pending.push(bytes);
    this.pendingBytes += bytes.length;
    this.flush();
  }

  private flush() {
    const sock = this.sock;
    if (!sock) return;
    while (this.pending.length) {
      const head = this.pending[0]!;
      const n = sock.write(head);
      if (n < head.length) {
        this.pending[0] = head.subarray(Math.max(0, n));
        this.pendingBytes -= Math.max(0, n);
        return; // 等 drain
      }
      this.pending.shift();
      this.pendingBytes -= n;
    }
    if (this.ending) sock.end();
  }
}

/** 從來源佇列取出 n 個取樣（每聲道） */
function take(s: Source, n: number): Float32Array {
  const out = new Float32Array(n * CHANNELS);
  let off = 0;
  while (off < out.length) {
    const head = s.queue[0]!;
    const need = out.length - off;
    if (head.length <= need) {
      out.set(head, off);
      off += head.length;
      s.queue.shift();
    } else {
      out.set(head.subarray(0, need), off);
      s.queue[0] = head.subarray(need);
      off += need;
    }
  }
  s.queued -= n;
  return out;
}
