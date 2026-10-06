/**
 * 把 WASAPI 擷取的聲音對齊畫面時間後，以 raw float32 經本機 TCP 送進 FFmpeg。
 *
 * 時間對齊：
 * - 畫面：FFmpeg 的 showinfo 每張畫面印一行 pts；畫面時間零點 ≈ min(收到該行的 QPC 時間 − pts)，
 *   取開頭約 1 秒的最小值，排除管線延遲。
 * - 聲音：每個 WASAPI 封包都有 QPC 時間戳，依「封包時間 − 畫面零點」決定它在時間軸上的位置；
 *   落後就補靜音、超前就裁掉，長時間錄影也不會因音效卡時鐘誤差而漂移。
 * - 系統聲音沒有播放時 WASAPI 不送資料，依 QPC 時鐘補靜音維持連續。
 *
 * 效能：錄影期間不配置新的陣列（Windows 上的 Bun 在計時器中配置 TypedArray 每次約 300µs）。
 * 每個來源用預先配置的環狀緩衝，混音與送出也重複使用同一塊緩衝。
 */
import type { Socket, TCPSocketListener } from "bun";
import { CHANNELS, SAMPLE_RATE, WasapiCapture } from "./audio.ts";
import { qpcNow100ns } from "./com.ts";

/** 每 25ms 取一次聲音：WASAPI 緩衝有 2 秒，間隔拉長不會掉資料，呼叫次數少六成 */
const TICK_MS = 25;
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
/** 每個來源的環狀緩衝（取樣數 / 每聲道）：一般只會累積一兩個 tick 的量，不夠時自動加大 */
const RING_FRAMES = SAMPLE_RATE * 4;
const frames100ns = (d: number) => Math.round((d * SAMPLE_RATE) / 1e7);

export interface AudioSourceSpec {
  loopback: boolean;
  /** 麥克風裝置 ID；空字串 = 預設麥克風 */
  micId?: string;
}

/** 畫面零點確定前收到的封包（只在開頭約 1 秒，可以配置） */
interface PrePacket {
  qpc?: number;
  /** null = 靜音 */
  data: Float32Array | null;
  frames: number;
}

/** 固定容量的 float32 環狀緩衝（交錯立體聲，以「每聲道取樣數」計） */
export class Ring {
  private buf: Float32Array;
  private head = 0; // 讀取位置（float 索引）
  size = 0; // 目前取樣數（每聲道）

  constructor(frames: number) {
    this.buf = new Float32Array(frames * CHANNELS);
  }

  private ensure(extraFrames: number) {
    const need = (this.size + extraFrames) * CHANNELS;
    if (need <= this.buf.length) return;
    // 罕見（程式停住很久）：加大並整理成從 0 開始
    const next = new Float32Array(Math.max(need, this.buf.length * 2));
    this.copyOut(next, this.size, false);
    this.buf = next;
    this.head = 0;
  }

  /** 寫入資料（null = 靜音） */
  push(data: Float32Array | null, frames: number) {
    if (frames <= 0) return;
    this.ensure(frames);
    const cap = this.buf.length;
    let w = (this.head + this.size * CHANNELS) % cap;
    let left = frames * CHANNELS;
    let src = 0;
    while (left > 0) {
      const n = Math.min(left, cap - w);
      if (data) this.buf.set(src === 0 && n === data.length ? data : data.subarray(src, src + n), w);
      else this.buf.fill(0, w, w + n);
      w = (w + n) % cap;
      src += n;
      left -= n;
    }
    this.size += frames;
  }

  /** 取出 frames 個取樣到 out 的開頭：add = 相加（混音），否則覆寫 */
  take(out: Float32Array, frames: number, add: boolean) {
    this.copyOut(out, frames, add);
    this.head = (this.head + frames * CHANNELS) % this.buf.length;
    this.size -= frames;
  }

  private copyOut(out: Float32Array, frames: number, add: boolean) {
    const cap = this.buf.length;
    let r = this.head;
    let left = frames * CHANNELS;
    let dst = 0;
    while (left > 0) {
      const n = Math.min(left, cap - r);
      if (add) for (let i = 0; i < n; i++) out[dst + i]! += this.buf[r + i]!;
      else out.set(this.buf.subarray(r, r + n), dst);
      r = (r + n) % cap;
      dst += n;
      left -= n;
    }
  }
}

interface Source {
  spec: AudioSourceSpec;
  label: string;
  cap?: WasapiCapture;
  reopenAt?: number;
  pre: PrePacket[];
  /** 已排進時間軸的取樣數（每聲道） */
  written: number;
  /** 等待混音的資料 */
  ring: Ring;
}

type Log = (level: "info" | "warn", text: string) => void;

export class AudioPipe {
  private server: TCPSocketListener<undefined>;
  private sock?: Socket<undefined>;
  /** 還沒送出的資料（FFmpeg 還沒連上、或寫不完時才有；是自己持有的複本） */
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
  /** 混音輸出（重複使用） */
  private mixBuf = new Float32Array(SAMPLE_RATE * CHANNELS);

  constructor(specs: AudioSourceSpec[], private log: Log) {
    this.sources = specs.map((spec) => ({
      spec,
      label: spec.loopback ? "系統聲音" : "麥克風",
      pre: [],
      written: 0,
      ring: new Ring(RING_FRAMES),
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

  /** 畫面時間零點已確定：之後不再需要 showinfo 的輸出 */
  get synced() {
    return this.t0 !== undefined;
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
    let maxWritten = 0;
    for (const s of this.sources) maxWritten = Math.max(maxWritten, s.written);
    const behind = target - maxWritten;
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
      for (const p of pre) this.place(s, p.data, p.frames, p.qpc);
    }
  }

  /** 讀取所有來源的新封包；裝置失效時關閉並定期嘗試重新開啟 */
  private pull(now: number) {
    for (const s of this.sources) {
      if (!s.cap) {
        if (s.reopenAt !== undefined && now >= s.reopenAt) this.open(s);
        if (!s.cap) continue;
      }
      try {
        if (this.t0 === undefined) {
          // 零點確定前先保留（只有開頭約 1 秒，這裡配置複本沒關係）
          s.cap.read((data, frames, qpc) => s.pre.push({ qpc, frames, data: data ? data.slice() : null }));
          while (s.pre.length && s.pre[0]!.qpc !== undefined && now - s.pre[0]!.qpc! > PRE_BUFFER_100NS) s.pre.shift();
        } else s.cap.read((data, frames, qpc) => this.place(s, data, frames, qpc));
      } catch (e) {
        this.log("warn", `${s.label}中斷：${(e as Error).message}，先以靜音代替並嘗試重新連接`);
        s.cap.close();
        s.cap = undefined;
        s.reopenAt = now + 10_000_000;
      }
    }
  }

  /** 依封包時間戳放進時間軸（data = null 為靜音） */
  private place(s: Source, data: Float32Array | null, frames: number, qpc: number | undefined) {
    const pos = qpc === undefined ? s.written : frames100ns(qpc - this.t0!);
    if (pos > s.written + TOLERANCE) this.fillSilence(s, pos);
    let skip = 0;
    if (pos < s.written - TOLERANCE) skip = s.written - pos; // 與已寫入的資料重疊（通常是零點之前的聲音）
    if (skip >= frames) return;
    this.append(s, data && skip ? data.subarray(skip * CHANNELS) : data, frames - skip);
  }

  private fillSilence(s: Source, upTo: number) {
    if (upTo > s.written) this.append(s, null, upTo - s.written);
  }

  private append(s: Source, data: Float32Array | null, frames: number) {
    s.ring.push(data, frames);
    s.written += frames;
  }

  /** 所有來源都有資料的部分相加後送出 */
  private mix() {
    if (!this.sources.length) return;
    let n = Number.POSITIVE_INFINITY;
    for (const s of this.sources) n = Math.min(n, s.ring.size);
    if (!(n > 0)) return;
    if (this.mixBuf.length < n * CHANNELS) this.mixBuf = new Float32Array(n * CHANNELS);
    const out = this.mixBuf;
    for (let i = 0; i < this.sources.length; i++) this.sources[i]!.ring.take(out, n, i > 0);
    this.write(new Uint8Array(out.buffer, out.byteOffset, n * CHANNELS * 4));
  }

  /** 送出 bytes（可能是重複使用的緩衝：沒能立即送出的部分會複製一份保留） */
  private write(bytes: Uint8Array) {
    let off = 0;
    if (this.sock && this.pending.length === 0) {
      off = Math.max(0, this.sock.write(bytes));
      if (off >= bytes.length) return;
    }
    const rest = bytes.length - off;
    if (this.pendingBytes + rest > MAX_PENDING_BYTES) {
      if (!this.overflowWarned) this.log("warn", "FFmpeg 沒有在讀取聲音資料，部分聲音已捨棄");
      this.overflowWarned = true;
      return;
    }
    this.pending.push(bytes.slice(off));
    this.pendingBytes += rest;
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
