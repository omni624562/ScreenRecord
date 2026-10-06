import { describe, expect, test } from "bun:test";
import { checkForUpdate, compareVersions } from "./updater.ts";

const release = (body: object, status = 200) => async () => new Response(JSON.stringify(body), { status });

describe("檢查新版本", () => {
  test("版本比較", () => {
    expect(compareVersions("1.2.0", "1.1.9")).toBeGreaterThan(0);
    expect(compareVersions("v1.10.0", "1.9.0")).toBeGreaterThan(0);
    expect(compareVersions("1.2", "1.2.0")).toBe(0);
    expect(compareVersions("1.1.1", "1.2.0")).toBeLessThan(0);
  });

  test("有較新的正式版才回傳", async () => {
    const url = "https://github.com/omni624562/ScreenRecord/releases/tag/v1.3.0";
    expect(await checkForUpdate("1.2.0", release({ tag_name: "v1.3.0", html_url: url }))).toEqual({ version: "1.3.0", url, publishedAt: undefined });
    expect(await checkForUpdate("1.3.0", release({ tag_name: "v1.3.0", html_url: url }))).toBeUndefined();
    expect(await checkForUpdate("1.2.0", release({ tag_name: "v1.3.0", prerelease: true }))).toBeUndefined();
    expect(await checkForUpdate("1.2.0", release({ tag_name: "v1.3.0", draft: true }))).toBeUndefined();
  });

  test("下載網址只接受本專案（避免開啟其他網站）", async () => {
    const u = await checkForUpdate("1.0.0", release({ tag_name: "v2.0.0", html_url: "https://evil.example/x" }));
    expect(u?.url).toBe("https://github.com/omni624562/ScreenRecord/releases/latest");
  });

  test("連線失敗或被限流時丟出錯誤（由呼叫端略過）", async () => {
    await expect(checkForUpdate("1.0.0", release({ message: "rate limited" }, 403))).rejects.toThrow("HTTP 403");
    await expect(checkForUpdate("1.0.0", release({ message: "Not Found" }, 404))).rejects.toThrow("私人");
  });
});
