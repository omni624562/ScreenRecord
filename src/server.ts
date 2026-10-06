/** 本機網頁介面與 API（只綁 127.0.0.1）。 */
import { existsSync, mkdirSync, statSync } from "node:fs";
import index from "./ui/index.html";
import { ConfigError } from "./args.ts";
import type { App } from "./app.ts";
import { listLibrary } from "./library.ts";
import { moveToRecycleBin } from "./recycle.ts";
import { loadSettings, saveSettings } from "./settings.ts";
import type { EditSpec } from "./shared/edit.ts";
import type { LibraryQuery, RecordConfig } from "./shared/types.ts";

export const APP_ID = "screen-recorder";

type Handler = (req: Request, url: URL) => Response | Promise<Response>;

const json = (data: unknown, status = 200) => Response.json(data, { status, headers: { "Cache-Control": "no-store" } });

export function startServer(app: App, port: number, development: boolean) {
  /**
   * 只接受來自本機介面的請求：
   * - Host 必須是 127.0.0.1 / localhost（擋 DNS rebinding）
   * - 非 GET 必須同源且為 JSON（擋其他網站對本機 API 發出的跨站請求）
   */
  function guard(req: Request): Response | undefined {
    const host = req.headers.get("host") ?? "";
    const okHost = host === `127.0.0.1:${server.port}` || host === `localhost:${server.port}`;
    if (!okHost) return new Response("Forbidden", { status: 403 });
    const site = req.headers.get("sec-fetch-site");
    if (site && site !== "same-origin" && site !== "none") return new Response("Forbidden", { status: 403 });
    if (req.method !== "GET") {
      const origin = req.headers.get("origin");
      if (origin && origin !== `http://${host}`) return new Response("Forbidden", { status: 403 });
      if (!req.headers.get("content-type")?.includes("application/json"))
        return new Response("Unsupported Media Type", { status: 415 });
    }
    return undefined;
  }

  const api = (h: Handler) => async (req: Request) => {
    const blocked = guard(req);
    if (blocked) return blocked;
    try {
      return await h(req, new URL(req.url));
    } catch (e) {
      const err = e as Error;
      if (!(err instanceof ConfigError)) console.error(err);
      return json({ ok: false, error: err.message }, err instanceof ConfigError ? 400 : 500);
    }
  };

  const body = async <T>(req: Request): Promise<T> => {
    try {
      return (await req.json()) as T;
    } catch {
      throw new ConfigError("請求格式錯誤");
    }
  };

  const status = () => ({
    recorder: app.recorder.status(),
    export: app.exporter.status(),
    download: app.downloader.status(),
    settingsRev: loadSettings().rev,
  });

  const server = Bun.serve({
    hostname: "127.0.0.1",
    port,
    development,
    idleTimeout: 60,
    routes: {
      "/": index,
      "/api/ping": api(() => json({ app: APP_ID, pid: process.pid })),
      "/api/env": api(() => json(app.env())),
      "/api/settings": {
        GET: api(() => json(loadSettings())),
        POST: api(async (req) => {
          const { ui, config } = await body<{ ui?: Record<string, unknown>; config?: RecordConfig }>(req);
          const saved = saveSettings({ ...(ui ? { ui } : {}), ...(config ? { config } : {}) });
          return json({ ok: true, rev: saved.rev });
        }),
      },
      "/api/ffmpeg/download": {
        POST: api(() => {
          if (app.ffmpegPath()) throw new ConfigError("已經有 FFmpeg 了");
          app.downloader.start();
          return json({ ok: true, ...status() });
        }),
      },
      "/api/encoder/reset-learned": { POST: api(() => (app.resetLearnedGpu(), json({ ok: true }))) },
      "/api/ffmpeg/cancel": { POST: api(() => (app.downloader.cancel(), json({ ok: true, ...status() }))) },
      "/api/env/refresh": { POST: api(async () => json(await app.refresh())) },
      "/api/status": api(() => {
        app.lastSeen = Date.now();
        return json(status());
      }),

      "/api/preview": api(async () => {
        if (!app.ffmpegPath()) return new Response("ffmpeg not found", { status: 503 });
        const img = await app.preview();
        if (!img) return new Response("preview failed", { status: 500 });
        return new Response(img, { headers: { "Content-Type": "image/jpeg", "Cache-Control": "no-store" } });
      }),
      "/api/preview/live": api((req, url) => {
        const fps = Math.min(10, Math.max(1, Number(url.searchParams.get("fps")) || 5));
        const stream = app.livePreview(fps, req.signal);
        if (!stream) return new Response("ffmpeg not found", { status: 503 });
        return new Response(stream, {
          headers: { "Content-Type": "multipart/x-mixed-replace;boundary=ffmpeg", "Cache-Control": "no-store" },
        });
      }),

      "/api/record/start": {
        POST: api(async (req) => {
          if (app.exporter.running) throw new ConfigError("匯出進行中，請等匯出完成再開始錄影");
          await app.recorder.start(await body<RecordConfig>(req));
          return json({ ok: true, ...status() });
        }),
      },
      "/api/record/pause": { POST: api(async () => (await app.recorder.pause(), json({ ok: true, ...status() }))) },
      "/api/record/resume": { POST: api(async () => (await app.recorder.resume(), json({ ok: true, ...status() }))) },
      "/api/record/stop": { POST: api(async () => (await app.recorder.stop(), json({ ok: true, ...status() }))) },

      "/api/library": api(async (_req, url) => {
        const p = url.searchParams;
        const dir = p.get("dir")?.trim() || app.defaultOutputDir;
        const query: LibraryQuery = {
          q: p.get("q") ?? undefined,
          filter: (p.get("filter") as LibraryQuery["filter"]) ?? "all",
          sort: (p.get("sort") as LibraryQuery["sort"]) ?? "new",
          page: Number(p.get("page") ?? 1),
          pageSize: Number(p.get("pageSize") ?? 30),
        };
        return json(await listLibrary(app.ffmpegPath(), dir, query));
      }),

      /** 刪除（移到資源回收筒，可還原）；只接受 .mp4，且不能刪正在轉檔的檔案 */
      "/api/delete": {
        POST: api(async (req) => {
          const { paths } = await body<{ paths: string[] }>(req);
          if (!Array.isArray(paths) || paths.length === 0) throw new ConfigError("沒有選取檔案");
          const job = app.exporter.status();
          const busy = job?.state === "running" ? [job.source, job.output].map((x) => x.toLowerCase()) : [];
          const list = paths.map((x) => String(x));
          for (const f of list) {
            if (!/^[a-zA-Z]:\\|^\\\\/.test(f) || !f.toLowerCase().endsWith(".mp4") || !existsSync(f) || !statSync(f).isFile())
              throw new ConfigError(`找不到檔案：${f}`);
            if (busy.includes(f.toLowerCase())) throw new ConfigError("檔案正在轉檔中，無法刪除");
          }
          moveToRecycleBin(list);
          return json({ ok: true, deleted: list.length });
        }),
      },
      "/api/export/start": {
        POST: api(async (req) => {
          if (app.recorder.active) throw new ConfigError("錄影中無法匯出，請先停止錄影");
          const { source, speed, keepAudio } = await body<{ source: string; speed: number; keepAudio?: boolean }>(req);
          await app.exporter.start(String(source ?? ""), Number(speed), keepAudio !== false);
          return json({ ok: true, ...status() });
        }),
      },
      "/api/cut/start": {
        POST: api(async (req) => {
          if (app.recorder.active) throw new ConfigError("錄影中無法剪輯，請先停止錄影");
          const { source, spec } = await body<{ source: string; spec: EditSpec }>(req);
          if (!spec || !Number.isFinite(spec.start) || !Number.isFinite(spec.end) || !Array.isArray(spec.removed))
            throw new ConfigError("剪輯設定格式錯誤");
          await app.exporter.startCut(String(source ?? ""), {
            start: Number(spec.start),
            end: Number(spec.end),
            removed: spec.removed.map((r) => [Number(r[0]), Number(r[1])] as [number, number]).filter((r) => r.every(Number.isFinite)),
            crop: spec.crop && [spec.crop.x, spec.crop.y, spec.crop.width, spec.crop.height].every(Number.isFinite) ? spec.crop : undefined,
          });
          return json({ ok: true, ...status() });
        }),
      },

      /** 剪輯預覽用：讓瀏覽器直接播放影片檔（Bun.file 自動支援 Range，可拖曳進度） */
      "/api/media": api((_req, url) => {
        const p = url.searchParams.get("path") ?? "";
        if (!/^[a-zA-Z]:\\|^\\\\/.test(p) || !p.toLowerCase().endsWith(".mp4") || !existsSync(p) || !statSync(p).isFile())
          return new Response("Not Found", { status: 404 });
        return new Response(Bun.file(p), { headers: { "Content-Type": "video/mp4", "Cache-Control": "no-store" } });
      }),

      "/api/export/cancel": { POST: api(() => (app.exporter.cancel(), json({ ok: true, ...status() }))) },

      "/api/open": {
        POST: api(async (req) => {
          const { action, path } = await body<{ action: "play" | "reveal" | "folder"; path: string }>(req);
          const p = String(path ?? "").trim();
          if (!/^[a-zA-Z]:\\|^\\\\/.test(p) || p.includes('"')) throw new ConfigError("路徑不正確");
          if (action === "folder") {
            mkdirSync(p, { recursive: true });
            openWithExplorer(p);
          } else {
            // 只允許開啟 .mp4，避免透過此 API 執行任意檔案
            if (!p.toLowerCase().endsWith(".mp4") || !existsSync(p) || !statSync(p).isFile())
              throw new ConfigError("找不到影片檔");
            openWithExplorer(p, action === "reveal");
          }
          return json({ ok: true });
        }),
      },

      "/api/quit": {
        POST: api(() => {
          setTimeout(() => void app.quit(), 50);
          return json({ ok: true });
        }),
      },
    },
    fetch() {
      return new Response("Not Found", { status: 404 });
    },
    error(err) {
      console.error(err);
      return new Response("Internal Server Error", { status: 500 });
    },
  });
  return server;
}

/**
 * 交給 explorer.exe 開啟（檔案 → 預設播放器、網址 → 預設瀏覽器）。
 * 由 Shell 啟動的程式不會落在本程式的 Job Object 裡，關閉本程式時不會被一併結束。
 */
export function openWithExplorer(target: string, select = false) {
  const arg = select ? `/select,"${target}"` : `"${target}"`;
  Bun.spawn(["explorer.exe", arg], { stdin: "ignore", stdout: "ignore", stderr: "ignore", windowsVerbatimArguments: true });
}
