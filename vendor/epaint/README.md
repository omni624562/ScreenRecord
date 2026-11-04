# epaint 0.36.2（ScreenRecorder 修改版）

egui 的文字排版套件 [epaint](https://github.com/emilk/egui/tree/main/crates/epaint)（MIT OR Apache-2.0），
從 crates.io 的 0.36.2 複製過來，透過根目錄 `Cargo.toml` 的 `[patch.crates-io]` 取代原版。

## 改了什麼

只改中文換行的規則，其他和原版相同（刪掉了用不到的 benches 與 dev-dependencies）：

- `src/text/font.rs` 的 `is_cjk_break_allowed`：不能放在行首的字加上全形標點
  「）］，．：；！？％’”…‥」（原版只有半形的和「。、」等少數幾個，
  所以中文說明換行時「）」「，」會跑到下一行的開頭）
- `src/text/font.rs` 新增 `is_cjk_opening`，`src/text/text_layout.rs` 的 `RowBreakCandidates::add`：
  「（「『【」等開頭括號、引號後面不換行（不會留在行尾）

改過的地方都有 `ScreenRecorder：` 開頭的註解。

## 升級 egui 時

1. 把新版的 epaint 原始碼複製到這裡（刪掉 `benches/`、`Cargo.toml` 裡的 `[[bench]]` 與 `[dev-dependencies.*]`）
2. 重新套用上面兩處修改（搜尋 `ScreenRecorder：`）
3. 如果新版已經處理了全形標點，就刪掉這個資料夾和根目錄 `Cargo.toml` 的 `[patch.crates-io]`
