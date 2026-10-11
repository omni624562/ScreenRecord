#!/usr/bin/env bash
# 從 CHANGELOG.md 取出指定版本的段落，當作 GitHub Release 的說明（輸出到 stdout）。
# 用法：release-notes.sh <tag，例如 v3.0.0> [exe 路徑]
#   會確認 tag 與 Cargo.toml（workspace.package.version）的版本一致；給 exe 路徑時在說明裡寫上檔案大小。
set -euo pipefail
cd "$(dirname "$0")/../.."

tag="${1:?需要 tag，例如 v3.0.0}"
exe="${2:-}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ "${tag#v}" != "$version" ]]; then
  echo "tag $tag 與 Cargo.toml 的版本 $version 不一致，請先更新 Cargo.toml" >&2
  exit 1
fi

# 「## 3.0.0（2026-10-09）」到下一個「## 」之前
body="$(awk -v v="$version" '
  index($0, "## " v) == 1 { rest = substr($0, length("## " v) + 1); if (rest == "" || rest ~ /^[（ (]/) { on = 1; next } }
  on && /^## / { exit }
  on { print }
' CHANGELOG.md | sed -e '/./,$!d')"
if [[ -z "$body" ]]; then
  echo "CHANGELOG.md 找不到 $version 的段落" >&2
  exit 1
fi

size=""
if [[ -n "$exe" && -f "$exe" ]]; then
  size="（約 $(awk -v b="$(wc -c < "$exe")" 'BEGIN { printf "%.1f", b / 1048576 }') MB，直接執行）"
fi

printf '%s\n\n### 安裝\n下載 `ScreenRecorder.zip`（解壓縮後執行）或 `ScreenRecorder.exe`%s，放到任意資料夾即可。找不到 FFmpeg 時，介面上按「自動下載」即可。\n' "$body" "$size"
