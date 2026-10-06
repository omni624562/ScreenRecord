/**
 * 程式圖示（以程式繪製，不需要圖檔）：
 * - idle：深色圓角方塊 + 紅點（與網頁圖示一致）
 * - recording：紅色方塊 + 白點，一眼看出正在錄影
 * - paused：琥珀色方塊 + 兩條白槓
 * - countdown：深色方塊 + 紅色圓環（即將開始）
 * 產生 Windows 圖示格式（BMP 型 ICO 資源），供系統匣與 exe 圖示使用。
 */
export type IconState = "idle" | "recording" | "paused" | "countdown";

type RGBA = [number, number, number, number];
const COLORS: Record<IconState, { bg: RGBA; fg: RGBA }> = {
  idle: { bg: [43, 43, 43, 255], fg: [229, 72, 77, 255] },
  recording: { bg: [229, 72, 77, 255], fg: [255, 255, 255, 255] },
  paused: { bg: [214, 140, 18, 255], fg: [255, 255, 255, 255] },
  countdown: { bg: [43, 43, 43, 255], fg: [229, 72, 77, 255] },
};

/** 每個像素 4×4 超取樣做反鋸齒；回傳由上而下的 RGBA */
export function renderIcon(size: number, state: IconState): Uint8ClampedArray {
  const { bg, fg } = COLORS[state];
  const px = new Uint8ClampedArray(size * size * 4);
  const pad = size * 0.06;
  const radius = size * 0.24;
  const c = size / 2;
  const inRoundRect = (x: number, y: number) => {
    const l = pad, t = pad, r = size - pad, b = size - pad;
    if (x < l || x > r || y < t || y > b) return false;
    const cx = Math.min(Math.max(x, l + radius), r - radius);
    const cy = Math.min(Math.max(y, t + radius), b - radius);
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2;
  };
  const inFg = (x: number, y: number) => {
    if (state === "paused") {
      const w = size * 0.13, h = size * 0.44, gap = size * 0.09;
      const top = c - h / 2;
      return y >= top && y <= top + h && ((x >= c - gap - w && x <= c - gap) || (x >= c + gap && x <= c + gap + w));
    }
    const d2 = (x - c) ** 2 + (y - c) ** 2;
    if (state === "countdown") return d2 <= (size * 0.26) ** 2 && d2 >= (size * 0.16) ** 2;
    return d2 <= (size * (state === "idle" ? 0.2 : 0.18)) ** 2;
  };
  const S = 4;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let nb = 0, nf = 0;
      for (let sy = 0; sy < S; sy++)
        for (let sx = 0; sx < S; sx++) {
          const fx = x + (sx + 0.5) / S, fy = y + (sy + 0.5) / S;
          if (inRoundRect(fx, fy)) inFg(fx, fy) ? nf++ : nb++;
        }
      const a = (nb + nf) / (S * S);
      const i = (y * size + x) * 4;
      if (a === 0) continue;
      const t = nf / (nb + nf);
      for (let k = 0; k < 3; k++) px[i + k] = bg[k]! * (1 - t) + fg[k]! * t;
      px[i + 3] = 255 * a;
    }
  }
  return px;
}

/** 單一尺寸的 BMP 型圖示資源（BITMAPINFOHEADER + BGRA 由下而上 + AND 遮罩） */
export function iconResource(size: number, state: IconState): Uint8Array {
  const rgba = renderIcon(size, state);
  const maskStride = Math.ceil(size / 32) * 4;
  const out = new Uint8Array(40 + size * size * 4 + maskStride * size);
  const v = new DataView(out.buffer);
  v.setUint32(0, 40, true);
  v.setInt32(4, size, true);
  v.setInt32(8, size * 2, true); // 高度含 AND 遮罩
  v.setUint16(12, 1, true);
  v.setUint16(14, 32, true);
  v.setUint32(20, size * size * 4 + maskStride * size, true);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const s = (y * size + x) * 4;
      const d = 40 + ((size - 1 - y) * size + x) * 4;
      out[d] = rgba[s + 2]!;
      out[d + 1] = rgba[s + 1]!;
      out[d + 2] = rgba[s]!;
      out[d + 3] = rgba[s + 3]!;
    }
  }
  return out; // AND 遮罩全為 0（透明度由 alpha 決定）
}

/** 多尺寸 .ico 檔（exe 圖示用） */
export function icoFile(state: IconState, sizes = [16, 20, 24, 32, 40, 48, 64, 256]): Uint8Array {
  const images = sizes.map((s) => iconResource(s, state));
  const total = 6 + 16 * sizes.length + images.reduce((n, b) => n + b.length, 0);
  const out = new Uint8Array(total);
  const v = new DataView(out.buffer);
  v.setUint16(2, 1, true);
  v.setUint16(4, sizes.length, true);
  let offset = 6 + 16 * sizes.length;
  sizes.forEach((s, i) => {
    const e = 6 + i * 16;
    out[e] = s >= 256 ? 0 : s;
    out[e + 1] = s >= 256 ? 0 : s;
    v.setUint16(e + 4, 1, true);
    v.setUint16(e + 6, 32, true);
    v.setUint32(e + 8, images[i]!.length, true);
    v.setUint32(e + 12, offset, true);
    out.set(images[i]!, offset);
    offset += images[i]!.length;
  });
  return out;
}
