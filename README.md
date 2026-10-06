# 螢幕錄影（ScreenRecorder）

Windows 11 螢幕錄影工具：**原速錄影並完整保留，停止後再選倍率匯出加速版**（可對同一支錄影重複匯出不同倍率）。

- TypeScript + Bun；`Bun.serve` 提供本機網頁介面，啟動後自動以預設瀏覽器開啟
- 擷取與編碼交給 FFmpeg：優先 `ddagrab`（Desktop Duplication，GPU 擷取），不支援或失敗時自動退回 `gdigrab`
- 輸出 H.264 MP4（yuv420p、BT.709），`bun build --compile` 編成單一 exe

目前版本：**1.1.0**　各版本的變更見 [CHANGELOG.md](CHANGELOG.md)（版本號顯示在操作視窗的標題列；系統匣選單「更新說明」可在程式裡查看）。

## 功能

| 項目 | 說明 |
| --- | --- |
| 擷取範圍 | 單一螢幕（DXGI 列舉）、所有螢幕（整個延伸桌面拼成一個畫面），或手動輸入 / 在預覽圖上拖曳框選範圍（可跨螢幕） |
| 聲音 | 可選擇錄「系統聲音」（電腦播放的聲音）與「麥克風」（可選裝置），兩者可同時錄並混音；AAC 48 kHz 立體聲 |
| 錄影設定 | FPS（15/24/30/60 或自訂 1–60）、解析度縮放 100/75/50/25%、是否錄進游標、最長錄影時間（0 = 不限） |
| 錄影控制 | 開始 / 暫停 / 繼續 / 停止；暫停採分段錄製，停止後以 concat（stream copy）合併成單一檔案 |
| 正常收尾 | 停止、暫停、結束程式時都對 FFmpeg 送出 `q`，逾時（10 秒）才強制終止 |
| 即時資訊 | 已錄時間、影片長度、檔案大小、實際 fps；電腦跟不上即時錄影時會提示 |
| 加速匯出 | 倍率 1.5/2/4/8/16× 或自訂（1.1–1000×），顯示「原片 → 加速後」長度與匯出進度，可取消；可保留聲音（變速不變調） |
| 剪輯影片 | 剪掉頭尾、刪除中間多段、裁切畫面範圍（在影片上拖曳框選），另存為 `*_cut.mp4`，原檔不變；剪輯版可再拿去加速匯出 |
| 錄影清單 | 主畫面下方「最近錄影」左右翻頁；「全部錄影」視窗可搜尋、篩選、排序、分頁，勾選多筆移到資源回收筒（可還原） |
| 介面 | 一頁式、不出現捲軸：以 1280×800 設計，較小的視窗等比縮小（最小 0.8 倍），較大的視窗撐滿、預覽區變大（最寬 2400px） |

預設儲存位置：`%USERPROFILE%\Videos\Timelapse`（介面上可改，會記住）  
檔名：`Rec_2026-10-05_14-30-00.mp4`，加速版：`…_4x.mp4`，剪輯版：`…_cut.mp4`

剪輯視窗快捷鍵：空白鍵 播放 / 暫停、← / → 前後一張（加 Shift 為一秒）、I 設為開頭、O 設為結尾、D 標記刪除起點 / 終點。

## 使用

### 系統匣常駐

程式啟動後常駐在工作列右下角的系統匣（Windows 11 新程式預設在 `^` 收合區，可拖到工作列上，或在「設定 → 個人化 → 工作列 → 其他系統匣圖示」開啟）。

- **左鍵點圖示**：開啟操作視窗（Chrome 的 app 模式獨立視窗；沒有 Chrome 用 Edge，都沒有才用預設瀏覽器）
- **右鍵選單**：開始錄影（沿用上次設定）、錄製指定螢幕 / 所有螢幕、暫停 / 繼續、停止並儲存、切換系統聲音 / 麥克風、開啟儲存資料夾、播放最近的錄影、開機時自動啟動、結束
- 圖示顏色代表狀態：深色 = 待命、紅色 = 錄影中、琥珀色 = 已暫停；滑鼠停在圖示上會顯示已錄時間
- 錄影儲存後會跳出通知，點通知開啟操作視窗
- 關閉操作視窗不會結束程式（仍常駐在系統匣）；要結束請用系統匣選單的「結束」
- 設定存在 `%LOCALAPPDATA%\ScreenRecorder\settings.json`，記錄檔 `ScreenRecorder.log` 也在同一處

### 執行編譯好的版本

```
ScreenRecorder.exe
ffmpeg.exe          ← 放在同一個資料夾
```

找不到 `ffmpeg.exe` 時，介面上方會出現提示，按「自動下載」即可：從 [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) 下載固定版本 FFmpeg 9.0.2 essentials（約 110 MB，失敗時改用 GitHub 上的同一檔案），比對程式內建的 SHA-256（不是從下載網站取得，檔案被替換也會被擋下）後，用 Windows 內建的 `System32\tar.exe` 解出 `ffmpeg.exe`，放到 exe 旁邊（沒有寫入權限時放 `%LOCALAPPDATA%\ScreenRecorder`），完成後自動偵測，不用重開程式。下載可取消，不會留下未完成的檔案。也可以按「手動下載」自行下載後放好，再按「重新偵測」。  
搜尋順序：`exe 所在資料夾\ffmpeg.exe` → `ffmpeg\bin\ffmpeg.exe` → `bin\ffmpeg.exe` → `%LOCALAPPDATA%\ScreenRecorder\ffmpeg.exe` → `PATH`。

從系統匣選單「結束」會先把錄影正常收尾並合併再結束。編譯版不顯示主控台視窗；需要看即時訊息時用 `bun run build:console` 另外編一個有主控台的版本。

參數：`--port <n>`（預設 47391，被占用時往後找）、`--no-open`（不自動開操作視窗）、`--tray`（只常駐系統匣，開機自動啟動時使用）。同時只會有一個實例，重複啟動會直接開啟現有的操作視窗。

### 開發

```bash
bun install
```

```bash
bun run start
```

```bash
bun test
```

```bash
bun run typecheck
```

### 建置

```bash
bun run build
```

產生 `dist\ScreenRecorder.exe`。若要一併把 PATH 上的 `ffmpeg.exe` 複製進 `dist\` 組成可發佈的資料夾：

```bash
bun run dist
```

### 發佈新版本

1. 修改 `package.json` 的 `version`（新功能增加次版號，只修正問題增加修訂號）
2. 在 `CHANGELOG.md` 最上方加上這一版的「新增／變更／修正」
3. `bun run build`：版本號會自動寫進 exe 的檔案內容與程式畫面

## 專案結構

```
src/
  main.ts        進入點：單一實例檢查、啟動伺服器、開瀏覽器、Ctrl+C 正常收尾
  app.ts         全域狀態（FFmpeg 偵測、螢幕清單、錄影器、匯出器）
  server.ts      Bun.serve 路由與 API（只綁 127.0.0.1，檢查 Host / Origin）
  recorder.ts    錄影狀態機：分段、暫停 / 繼續、q 收尾、意外中斷自動續錄、合併
  exporter.ts    轉檔工作：加速匯出、剪輯（同時只跑一個，可取消）
  library.ts     掃描儲存資料夾、讀取影片資訊
  args.ts        所有 FFmpeg 參數組裝（純函式，有單元測試）
  ffmpeg.ts      尋找 ffmpeg.exe、偵測 ddagrab / gdigrab / 編碼器、ddagrab 實測
  monitors.ts    以 bun:ffi 呼叫 DXGI 列舉螢幕（取得 ddagrab 需要的 adapter / output 索引）
  audio.ts       以 bun:ffi 呼叫 WASAPI：系統聲音（loopback）與麥克風擷取、裝置列舉
  audiopipe.ts   聲音對齊畫面時間、混音，經本機 TCP 送進 FFmpeg
  com.ts         COM vtable 呼叫、GUID、QPC 時鐘等共用工具
  tray.ts        系統匣控制（狀態、選單指令、通知）；tray-worker.ts 在獨立執行緒建立圖示與選單
  icon.ts        以程式繪製的圖示（系統匣三種狀態、exe 圖示）
  settings.ts    設定存檔；desktop.ts 開啟操作視窗、開機自動啟動；log.ts 記錄檔
  downloader.ts  自動下載 FFmpeg（SHA-256 校驗、解壓縮、放置）
  shared/        前後端共用的型別、格式化與剪輯計算（edit.ts）
  ui/editor.ts   剪輯對話框（時間軸、刪除片段、裁切框選）
  ui/            網頁介面（index.html + app.ts + style.css，由 Bun 打包進 exe）
scripts/copy-ffmpeg.ts
```

## 設計重點

- **ddagrab 優先、gdigrab 備援**：啟動時用 ddagrab 實際抓一張畫面測試；錄影時若 ddagrab 在第一張畫面前就失敗（自動模式），立即改用 gdigrab。
- **延伸螢幕**：ddagrab 一次只能擷取一個輸出，所以「所有螢幕」與跨螢幕的範圍會對每個螢幕各開一個 ddagrab，再用 `xstack` 依 Windows 顯示設定中的位置拼接（大小不同的空白處補黑）。ddagrab 的 `output_idx` 是「某張顯示卡上的第幾個輸出」，因此用 DXGI 列舉並以 `-init_hw_device d3d11va=dda:<adapter>` 指定顯示卡；涵蓋的螢幕接在不同顯示卡上時無法合成，改用 gdigrab 擷取整個範圍。預覽圖也走同一條路徑，確保看到的就是錄到的。
- **聲音**：FFmpeg 在 Windows 只能用 dshow 錄麥克風、錄不到系統聲音，所以兩者都以 WASAPI 自行擷取（統一轉成 48 kHz / 立體聲 / float32），經本機 TCP 送進 FFmpeg。對齊方式：FFmpeg 的 `showinfo` 回報每張畫面的時間，推算畫面時間零點；WASAPI 封包帶有 QPC 時間戳（與 ddagrab 同一個時鐘），依此補靜音或裁切，長時間錄影也不漂移。沒有播放聲音時 WASAPI 不送資料，會依時鐘補靜音；音訊裝置被拔除或切換時以靜音代替並自動重新連接。實測（avsynctest 閃光 + 嗶聲）影音差距約 +12～19 ms，在 1 張畫面以內。
- **分段與防損壞**：分段 MP4 以 fragmented MP4 寫入（每秒一個 fragment），即使 FFmpeg 被強制結束或當機，最多只損失最後約 1 秒；停止後以 `-c copy` 合併為一般 MP4（`+faststart`）並驗證成品。
- **自動續錄**：錄到一半 FFmpeg 意外結束（例如鎖定畫面、UAC 安全桌面讓 Desktop Duplication 中斷），會保留已錄分段並以退避重試開新分段；超過 15 秒沒有新畫面也會重啟 FFmpeg。
- **編碼器（CPU / GPU）**：預設「自動」——平常用 libx264（畫質最穩、相容性最好），畫面量超過 1080p60（例如 4K、雙螢幕拼接）時改用 GPU 編碼；錄影中偵測到電腦處理不及，會記住並在之後的錄影自動改用 GPU（「更多 → 編碼器」可重設）（同一段錄影不中途切換，否則分段無法無損合併）。GPU 編碼器在啟動時實際試編一小段，只列出真的能用的（例如 MX150 沒有 NVENC 會被排除）；GPU 編碼一開始就失敗時自動退回 CPU。加速匯出與剪輯屬於離線轉檔，固定用 libx264 以畫質為優先。
- **效能監看**：以近 5 秒實際寫入張數與 `dup_frames` 計算實際 fps（FFmpeg 的 `speed=` 會把啟動時間算進去，開頭會嚴重偏低，不適合用來判斷）。
- **加速匯出**：`setpts=PTS/倍率,fps=原fps`，直接捨棄多餘的幀而不做混合，螢幕文字才不會有殘影；聲音用串接的 `atempo`（每段 ≤ 2 倍）變速不變調。
- **剪輯**：`select` / `aselect` 依保留區段逐張挑選並重新接時間戳，可剪在任意一張畫面上（不受關鍵影格限制），再 `crop` 裁切畫面；因此需要重新編碼。預覽播放由 `/api/media` 直接提供影片檔（支援 Range，可拖曳進度）。
- **系統匣**：以 `bun:ffi` 呼叫 `Shell_NotifyIcon` / `TrackPopupMenu`。選單開著時會卡住所在執行緒，所以放在 Bun Worker 裡，錄音讀取與網頁伺服器不受影響；選單跟隨 Windows 深色 / 淺色模式。
- **本機 API 防護**：只綁 `127.0.0.1`；檢查 `Host`（防 DNS rebinding）、非 GET 需同源且為 JSON（防其他網站跨站呼叫）；「開啟檔案」只接受 `.mp4`。
- **子行程**：FFmpeg 由 libuv 放進 kill-on-close 的 Job Object，本程式被強制結束時 FFmpeg 也會一起結束；瀏覽器與播放器則透過 `explorer.exe` 交給 Shell 啟動，不會被一併關閉。

## 已知限制

- 多個 ddagrab 合成延伸桌面的流程只在單螢幕電腦上以模擬方式驗證過拼接參數；在真正的多螢幕電腦上若合成失敗，自動模式會立即改用 gdigrab。
- 系統聲音會受 Windows 音量影響（音量調得很小時錄到的也很小聲）。
- 錄聲音時，每個分段結尾的畫面會比聲音多約 0.3～0.5 秒（FFmpeg 收到 `q` 後先停止讀取輸入，畫面來源還會多送幾張），因此停止處與暫停點會有一小段沒有聲音；影音同步不受影響。
- gdigrab 在 1080p30 以上較吃 CPU，可能無法維持設定的 fps（會提示）；ddagrab 沒有這個問題。
- 混合 DPI 的多螢幕環境下，gdigrab 的座標換算以 FFmpeg 的行為為準；旋轉的螢幕未實測。
