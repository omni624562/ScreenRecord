/**
 * 從 CHANGELOG.md 取出指定版本的段落，當作 GitHub Release 的說明。
 * 用法：bun run scripts/release-notes.ts [版本] [--check-tag v1.2.0]
 *   版本省略時用 package.json 的版本；--check-tag 會確認 tag 與 package.json 的版本一致。
 */
import pkg from "../package.json";
import changelog from "../CHANGELOG.md" with { type: "text" };

const args = process.argv.slice(2);
const tagIdx = args.indexOf("--check-tag");
const tag = tagIdx >= 0 ? args[tagIdx + 1] : undefined;
const positional = args.filter((a, i) => !a.startsWith("--") && (tagIdx < 0 || i !== tagIdx + 1));
const version = positional[0] ?? pkg.version;

if (tag && tag.replace(/^v/i, "") !== pkg.version) {
  console.error(`tag ${tag} 與 package.json 的版本 ${pkg.version} 不一致，請先更新 package.json`);
  process.exit(1);
}

export function sectionFor(md: string, v: string): string | undefined {
  const lines = md.split(/\r?\n/);
  const start = lines.findIndex((l) => new RegExp(`^## ${v.replace(/\./g, "\\.")}(\\b|（|\\s|$)`).test(l));
  if (start < 0) return undefined;
  let end = lines.findIndex((l, i) => i > start && /^## /.test(l));
  if (end < 0) end = lines.length;
  return lines.slice(start + 1, end).join("\n").trim();
}

const body = sectionFor(changelog, version);
if (!body) {
  console.error(`CHANGELOG.md 找不到 ${version} 的段落`);
  process.exit(1);
}
console.log(`${body}

### 安裝
下載 \`ScreenRecorder.exe\`，放到任意資料夾後直接執行。找不到 FFmpeg 時，介面上可一鍵自動下載。`);
