//! 錄完自動處理：錄影存好後依設定依序做——
//! 1. 剪掉沒動靜的片段（畫面不動而且沒有聲音，見 idle.rs）：另存剪輯版，並存剪輯設定，之後在剪輯視窗可以再改
//! 2. 產生字幕：在影片（有剪輯版時用剪輯版）旁邊存一份 SRT（需要時先下載語音辨識用的 FFmpeg 與語音模型）
//! 3. 壓縮到指定大小以內：另存一支（原速）
//!
//! 設定存在操作視窗的設定（ui）裡（設定組合也會一起存）；系統匣、快速鍵、排程的錄影一樣會處理。
//! 一次處理一支：等手動的轉檔、新的錄影結束才開始下一步（錄影時做會搶 CPU、掉格）。

use crate::app::App;
use crate::edit::EditSpec;
use crate::format::human_duration;
use crate::recorder::BoxFut;
use crate::types::ExportState;
use crate::{info, tr, trf, warn};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 錄完要做什麼（操作視窗設定的 afterIdleCut、afterSubs、afterSubsModel、afterSubsLang、afterCompressMb）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterRecord {
    pub idle_cut: bool,
    pub subtitles: bool,
    /// subtitles::MODELS、LANGUAGES 的第幾個
    pub subs_model: usize,
    pub subs_lang: usize,
    /// 壓縮到這個大小以內（MB）；0 = 不壓縮
    pub compress_mb: u32,
    /// 依內容自動命名（只改預設的 Rec_時間 名稱）
    pub auto_name: bool,
    /// 產生字幕時也翻成英文（另存 同名.en.srt）
    pub subs_en: bool,
    /// 另存加速版（幾倍；0 = 不做）。只有一次處理多支時用，錄完自動處理沒有這一項
    pub speed: f64,
    /// 加速版保留聲音
    pub keep_audio: bool,
    /// 處理完複製到這個資料夾（雲端硬碟的同步資料夾、NAS）；空的 = 不複製
    pub copy_to: String,
}

/// ui 設定裡「錄完複製到資料夾」的欄位名稱（是位置，不放進設定組合）
pub const COPY_KEY: &str = "afterCopyTo";

/// ui 設定裡的欄位名稱（設定組合也存這些）
/// 錄影的命名規則（介面設定裡的 key；空的 = 不用）
pub const NAME_RULE_KEY: &str = "nameRule";
/// 截圖的命名規則
pub const SHOT_NAME_RULE_KEY: &str = "shotNameRule";

pub const UI_KEYS: [&str; 7] = ["afterIdleCut", "afterSubs", "afterSubsModel", "afterSubsLang", "afterCompressMb", "afterAutoName", "afterSubsEn"];

impl AfterRecord {
    pub fn from_ui(ui: Option<&Value>) -> AfterRecord {
        let Some(o) = ui.and_then(Value::as_object) else { return AfterRecord::default() };
        let b = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
        let n = |k: &str| o.get(k).and_then(Value::as_u64).unwrap_or(0);
        AfterRecord {
            idle_cut: b(UI_KEYS[0]),
            subtitles: b(UI_KEYS[1]),
            subs_model: (n(UI_KEYS[2]) as usize).min(crate::subtitles::MODELS.len() - 1),
            subs_lang: (n(UI_KEYS[3]) as usize).min(crate::subtitles::LANGUAGES.len() - 1),
            compress_mb: n(UI_KEYS[4]).min(10_000) as u32,
            auto_name: b(UI_KEYS[5]),
            subs_en: b(UI_KEYS[6]),
            copy_to: o.get(COPY_KEY).and_then(Value::as_str).unwrap_or("").trim().to_string(),
            ..Default::default()
        }
    }

    pub fn any(&self) -> bool {
        self.idle_cut || self.subtitles || self.compress_mb > 0 || self.speed > 1.0 || !self.copy_to.is_empty()
    }

    /// 要做幾步
    fn steps(&self) -> usize {
        [self.idle_cut, self.subtitles, self.speed > 1.0, self.compress_mb > 0, !self.copy_to.is_empty()].iter().filter(|b| **b).count()
    }
}

/// 只錄聲音存成 M4A 的設定（操作視窗設定的 audioM4a；沒有設定時是要）
pub const AUDIO_M4A_KEY: &str = "audioM4a";

pub fn audio_m4a(ui: Option<&Value>) -> bool {
    ui.and_then(|u| u.get(AUDIO_M4A_KEY)).and_then(Value::as_bool).unwrap_or(true)
}

/// 只錄聲音的錄影（錄影器用「錄音_」開頭的檔名）
pub fn is_audio_recording(path: &str) -> bool {
    path.rsplit(['\\', '/']).next().is_some_and(|n| n.starts_with("錄音_"))
}

/// 只錄聲音時另存的 M4A（和錄影同一個資料夾、同名）
pub fn m4a_path(video: &str) -> std::path::PathBuf {
    Path::new(video).with_extension("m4a")
}

/// 處理中的狀態（介面顯示、可以取消）
#[derive(Debug, Clone)]
pub struct AfterStatus {
    /// 錄影的檔名
    pub name: String,
    /// 正在做哪一步
    pub step: String,
    /// 第幾步、總共幾步（剪掉沒動靜的片段、字幕、壓縮）
    pub step_no: usize,
    pub steps: usize,
    /// 這一步的進度（千分比；不知道時是 0）
    pub progress: Arc<AtomicU32>,
    /// 使用者按了取消
    pub cancel: Arc<AtomicBool>,
    /// 停止目前這一步（取消，或讓錄影先做）
    stop_step: Arc<AtomicBool>,
    /// 開始錄影時讓出來：這一步錄完再重做
    preempted: Arc<AtomicBool>,
    /// 正在轉檔的工作是錄完自動處理開始的（介面不另外顯示轉檔卡）
    pub exporting: bool,
    /// 正在辨識字幕（很吃 CPU，開始錄影時也讓出來）
    transcribing: bool,
    /// 這次處理開始過的轉檔工作（完成後的轉檔卡也不另外顯示）
    pub export_ids: Vec<u64>,
    /// 等錄影或其他轉檔結束
    pub waiting: bool,
    /// 處理完了（介面顯示結果）
    pub done: Option<AfterDone>,
    /// 一次處理多支：第幾支、總共幾支
    pub batch: Option<(usize, usize)>,
}

/// 處理完的結果（介面顯示「已自動處理：…」）
#[derive(Debug, Clone, PartialEq)]
pub struct AfterDone {
    /// 第幾次（介面記住關掉了哪一個）
    pub seq: u64,
    pub ok: bool,
    /// 做了什麼（或失敗的原因）
    pub text: String,
    /// 處理好的影片（剪輯版或壓縮版）；沒有另存影片時是 None
    pub output: Option<String>,
    /// 原本的錄影（一次處理多支時是最後一支）
    pub source: String,
    /// 一次處理多支的結果
    pub batch: bool,
}

static DONE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 處理一支的結果
enum Processed {
    Done(AfterDone),
    /// 使用者取消
    Canceled,
    /// 沒有需要處理的
    Nothing,
}

impl App {
    /// 錄完自動處理中（介面顯示用；處理完就是 None）
    pub fn after_status(&self) -> Option<AfterStatus> {
        self.after.lock().unwrap().clone().filter(|s| s.done.is_none())
    }

    /// 最近一次錄完自動處理的結果（取消時沒有）
    pub fn after_done(&self) -> Option<AfterDone> {
        self.after.lock().unwrap().as_ref().and_then(|s| s.done.clone())
    }

    /// 這個轉檔工作是錄完自動處理開始的（介面不另外顯示它的轉檔卡、完成提示）
    pub fn after_owns_export(&self, id: u64) -> bool {
        self.after.lock().unwrap().as_ref().is_some_and(|s| s.export_ids.contains(&id))
    }

    /// 取消錄完自動處理（正在轉檔時一併取消）
    pub fn cancel_after(&self) {
        let Some(s) = self.after_status() else { return };
        s.cancel.store(true, Ordering::Relaxed);
        s.stop_step.store(true, Ordering::Relaxed);
        if s.exporting && self.exporter.running() {
            let _ = self.exporter.cancel();
        }
    }

    /// 要開始錄影了：錄完自動處理正在轉檔或辨識字幕時先停下來，錄完再重做這一步（錄影時做會搶 CPU、掉格）。
    /// 回傳有沒有停下來
    pub(crate) fn after_yield(&self) -> bool {
        let Some(s) = self.after_status() else { return false };
        if !s.exporting && !s.transcribing {
            return false;
        }
        s.preempted.store(true, Ordering::Relaxed);
        s.stop_step.store(true, Ordering::Relaxed);
        if s.exporting && self.exporter.running() {
            let _ = self.exporter.cancel();
        }
        info!("[錄完自動處理] 開始錄影，先暫停「{}」，錄完再繼續", s.step);
        true
    }

    fn after_set(&self, f: impl FnOnce(&mut AfterStatus)) {
        if let Some(s) = self.after.lock().unwrap().as_mut() {
            f(s);
        }
    }

    fn after_step(&self, step: String) {
        self.after_set(|s| {
            s.step = step;
            s.progress.store(0, Ordering::Relaxed);
        });
    }

    /// 開始做一次可以被錄影打斷的事（轉檔、辨識字幕）：清掉之前的記號
    fn after_arm(&self) {
        if let Some(s) = self.after_status() {
            s.preempted.store(false, Ordering::Relaxed);
            if !s.cancel.load(Ordering::Relaxed) {
                s.stop_step.store(false, Ordering::Relaxed);
            }
        }
    }

    /// 這一步被錄影打斷了（清掉記號，等錄完再做一次）
    fn after_take_preempted(&self) -> bool {
        let Some(s) = self.after_status() else { return false };
        let p = s.preempted.swap(false, Ordering::Relaxed);
        if p && !s.cancel.load(Ordering::Relaxed) {
            s.stop_step.store(false, Ordering::Relaxed);
        }
        p && !s.cancel.load(Ordering::Relaxed)
    }

    /// 等到可以轉檔：沒有錄影、沒有其他轉檔工作；取消時回傳 false
    async fn after_wait_idle(&self, cancel: &AtomicBool) -> bool {
        let mut waiting = false;
        let ok = loop {
            if cancel.load(Ordering::Relaxed) || self.is_quitting() {
                break false;
            }
            if !self.recorder.active() && !self.recorder.starting() && !self.exporter.running() {
                break true;
            }
            if !waiting {
                waiting = true;
                self.after_set(|s| s.waiting = true);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        self.after_set(|s| s.waiting = false);
        ok
    }

    /// 錄影存好之後（由錄影器呼叫）：依設定處理，完成時用系統匣通知
    pub async fn after_record(self: Arc<Self>, video: String) {
        let ui = self.settings.load().ui;
        // 命名規則：先改名，之後做出來的檔案（剪輯版、壓縮版、字幕）都接在新的名稱後面
        let rule = ui.as_ref().and_then(|u| u.get(NAME_RULE_KEY)).and_then(Value::as_str).unwrap_or("").to_string();
        let renamed = match crate::actions::apply_name_rule(&self, &video, &rule).await {
            Some(p) => {
                info!("[命名規則] {video} → {p}");
                Some(p)
            }
            None => None,
        };
        let video = renamed.clone().unwrap_or(video);
        // 只錄聲音、設定「存成 M4A」（預設）：另存一份只有聲音的 M4A（不重新壓縮，一下子就好）
        if is_audio_recording(&video) && audio_m4a(ui.as_ref()) {
            match crate::actions::save_audio(&self, &video).await {
                Ok(out) => info!("[錄音] 存成 M4A：{out}"),
                Err(e) => {
                    warn!("[錄音] 無法存成 M4A：{}", e.message());
                    self.notify(tr!("錄音", "Audio recording"), e.message(), true);
                }
            }
        }
        let mut after = AfterRecord::from_ui(ui.as_ref());
        // 已經依命名規則取好名稱：不再自動命名
        if renamed.is_some() {
            after.auto_name = false;
        }
        if !after.any() {
            if after.auto_name {
                self.after_auto_name(&video, None).await;
            }
            return;
        }
        self.after_process(video, &after, None).await;
    }

    /// 一次處理多支錄影（全部錄影勾選後）：一支一支做，做完用系統匣通知一次；取消時剩下的也不做
    pub async fn batch_process(self: Arc<Self>, videos: Vec<String>, after: AfterRecord) {
        let after = AfterRecord { auto_name: false, ..after };
        let n = videos.len();
        info!("[一次處理多支] 開始：{n} 支");
        let (mut ok, mut failed, mut outputs) = (0, vec![], vec![]);
        let mut last = String::new();
        for (i, video) in videos.into_iter().enumerate() {
            last = video.clone();
            match self.after_process(video.clone(), &after, Some((i + 1, n))).await {
                Processed::Canceled => {
                    info!("[一次處理多支] 已取消（做完 {i} 支）");
                    if i == 0 {
                        *self.after.lock().unwrap() = None;
                        return;
                    }
                    break;
                }
                Processed::Done(d) if d.ok => {
                    ok += 1;
                    outputs.extend(d.output);
                }
                Processed::Done(d) => failed.push(format!("{}：{}", Path::new(&video).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), d.text)),
                Processed::Nothing => ok += 1,
            }
        }
        let mut text = trf!("處理好 {ok} 支錄影", "{ok} recording(s) processed");
        if !failed.is_empty() {
            text += &trf!("；{} 支沒有完成（{}）", "; {} didn't finish ({})", failed.len(), failed.join(tr!("、", ", ")));
        }
        info!("[一次處理多支] 完成：{text}");
        self.notify(tr!("一次處理多支錄影", "Batch processing"), &text, !failed.is_empty() && ok == 0);
        // 輸出只有一支時可以直接播放
        let output = if outputs.len() == 1 { outputs.pop() } else { None };
        let done = AfterDone { seq: DONE_SEQ.fetch_add(1, Ordering::Relaxed) + 1, ok: ok > 0, text, output, source: last, batch: true };
        // 留著這次的狀態（轉檔工作的編號：介面不另外顯示那些轉檔卡），只加上結果
        let mut st = self.after.lock().unwrap();
        let s = st.get_or_insert_with(|| Self::new_status(String::new(), 0, Default::default(), Default::default(), None));
        s.done = Some(done);
        s.exporting = false;
    }

    fn new_status(name: String, steps: usize, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>, batch: Option<(usize, usize)>) -> AfterStatus {
        AfterStatus {
            name,
            step: String::new(),
            step_no: 0,
            steps,
            progress,
            cancel,
            stop_step: Arc::new(AtomicBool::new(false)),
            preempted: Arc::new(AtomicBool::new(false)),
            exporting: false,
            transcribing: false,
            export_ids: vec![],
            waiting: false,
            done: None,
            batch,
        }
    }

    /// 處理一支錄影。batch = 一次處理多支的第幾支：結果不留給介面、不另外通知（全部做完再一起說）
    async fn after_process(self: &Arc<Self>, video: String, after: &AfterRecord, batch: Option<(usize, usize)>) -> Processed {
        // 前一支還在處理：等它做完（一次一支）。一次處理多支時，上一支的狀態會留到全部做完（新的錄影等全部做完再處理）
        while self.after_status().is_some_and(|s| batch.is_none() || s.batch.is_none()) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let name = Path::new(&video).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (progress, cancel) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        {
            let mut st = self.after.lock().unwrap();
            let mut s = Self::new_status(name.clone(), after.steps(), progress.clone(), cancel.clone(), batch);
            // 一次處理多支：沿用前幾支開始的轉檔工作編號
            if let Some(prev) = st.as_ref().filter(|p| batch.is_some() && p.batch.is_some()) {
                s.export_ids = prev.export_ids.clone();
            }
            *st = Some(s);
        }
        info!("[錄完自動處理] 開始：{name}");
        let mut output = None;
        let r = self.after_run(&video, after, progress, cancel.clone(), &mut output).await;
        let title = tr!("錄完自動處理", "Post-recording processing");
        let done = match r {
            _ if cancel.load(Ordering::Relaxed) => {
                info!("[錄完自動處理] 已取消：{name}");
                None
            }
            Ok(done) if done.is_empty() => {
                info!("[錄完自動處理] 沒有需要處理的：{name}");
                None
            }
            Ok(done) => {
                let text = done.join(tr!("；", "; "));
                info!("[錄完自動處理] 完成：{name}：{text}");
                if batch.is_none() {
                    self.notify(title, &text, false);
                }
                Some(AfterDone { seq: DONE_SEQ.fetch_add(1, Ordering::Relaxed) + 1, ok: true, text, output, source: video.clone(), batch: false })
            }
            Err(e) => {
                warn!("[錄完自動處理] {name}：{e}");
                if batch.is_none() {
                    self.notify(title, &e, true);
                }
                Some(AfterDone { seq: DONE_SEQ.fetch_add(1, Ordering::Relaxed) + 1, ok: false, text: e, output: None, source: video.clone(), batch: false })
            }
        };
        let canceled = cancel.load(Ordering::Relaxed);
        // 自動命名放最後：字幕做好了才能依說的話取名
        let done = match (after.auto_name, done) {
            (true, Some(mut d)) => {
                if let Some((v, o)) = self.after_auto_name(&video, d.output.clone().as_deref()).await {
                    d.source = v;
                    d.output = o.or(d.output);
                }
                Some(d)
            }
            (true, None) => {
                self.after_auto_name(&video, None).await;
                None
            }
            (false, d) => d,
        };
        // 複製到資料夾放最後：名稱、字幕、處理好的版本都好了才複製
        let done = if after.copy_to.is_empty() || canceled {
            done
        } else {
            let (src, out) = match &done {
                Some(d) => (d.source.clone(), d.output.clone()),
                None => (video.clone(), None),
            };
            self.after_begin(after.steps(), trf!("複製到「{}」", "Copying to “{}”", folder_name(&after.copy_to)));
            let dest = after.copy_to.clone();
            let r = tokio::task::spawn_blocking(move || copy_results(&src, out.as_deref(), Path::new(&dest))).await.unwrap_or_else(|e| Err(e.to_string()));
            let (ok, note) = match r {
                Ok(n) => {
                    info!("[錄完自動處理] 複製 {n} 個檔案到 {}", after.copy_to);
                    (true, trf!("複製 {n} 個檔案到「{}」", "copied {n} files to “{}”", folder_name(&after.copy_to)))
                }
                Err(e) => {
                    warn!("[錄完自動處理] 複製到 {} 失敗：{e}", after.copy_to);
                    (false, trf!("沒有複製到「{}」：{e}", "couldn't copy to “{}”: {e}", folder_name(&after.copy_to)))
                }
            };
            let sep = tr!("；", "; ");
            match done {
                Some(mut d) => {
                    d.text = format!("{}{sep}{note}", d.text);
                    d.ok &= ok;
                    Some(d)
                }
                None => {
                    if batch.is_none() {
                        self.notify(tr!("錄完自動處理", "Post-recording processing"), &note, !ok);
                    }
                    Some(AfterDone { seq: DONE_SEQ.fetch_add(1, Ordering::Relaxed) + 1, ok, text: note, output: None, source: video.clone(), batch: false })
                }
            }
        };
        let mut st = self.after.lock().unwrap();
        match done {
            // 一次處理多支：狀態留著（下一支接著用），全部做完再一起顯示
            _ if batch.is_some() => {
                if let Some(s) = st.as_mut() {
                    s.exporting = false;
                }
            }
            // 留著結果給介面顯示（處理中的狀態不算）
            Some(ref d) => {
                if let Some(s) = st.as_mut() {
                    s.done = Some(d.clone());
                    s.exporting = false;
                }
            }
            None => *st = None,
        }
        match done {
            _ if canceled => Processed::Canceled,
            Some(d) => Processed::Done(d),
            None => Processed::Nothing,
        }
    }

    /// 自動命名：錄影（與錄完處理做出來、名稱接在後面的檔案，例如 _cut）改成依內容想的名稱；回傳 (新的錄影路徑, 新的輸出路徑)
    async fn after_auto_name(&self, video: &str, output: Option<&str>) -> Option<(String, Option<String>)> {
        let old_stem = Path::new(video).file_stem()?.to_string_lossy().into_owned();
        let new_video = crate::actions::auto_rename(self, video).await?;
        let new_stem = Path::new(&new_video).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut new_out = None;
        if let Some(o) = output {
            let o_stem = Path::new(o).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(rest) = o_stem.strip_prefix(&old_stem) {
                match crate::actions::rename(self, o, &format!("{new_stem}{rest}")).await {
                    Ok(p) => new_out = Some(p),
                    Err(e) => warn!("[自動命名] {o}：{}", e.message()),
                }
            }
        }
        info!("[自動命名] {old_stem} → {new_stem}");
        self.notify(tr!("自動命名", "Auto-named"), &trf!("錄影已命名為「{}」", "Recording named “{}”", new_stem), false);
        Some((new_video, new_out))
    }

    /// 開始一步：第幾步、做什麼
    fn after_begin(&self, step_no: usize, step: String) {
        self.after_set(|s| s.step_no = step_no);
        self.after_step(step);
    }

    /// 錄完自動處理的一個轉檔步驟（剪掉沒動靜的片段、壓縮）：開始錄影時讓出來，錄完再做一次；回傳輸出檔。
    /// cancel 時回傳 Ok(None)
    async fn after_export<'f>(&self, cancel: &AtomicBool, start: impl Fn() -> BoxFut<'f, crate::Result<()>>) -> Result<Option<String>, String> {
        loop {
            if !self.after_wait_idle(cancel).await {
                return Ok(None);
            }
            self.after_arm();
            self.after_set(|s| s.exporting = true);
            if let Err(e) = start().await {
                self.after_set(|s| s.exporting = false);
                // 剛好開始錄影：錄完再做
                if self.recorder.active() || self.recorder.starting() {
                    continue;
                }
                return Err(e.message().to_string());
            }
            if let Some(id) = self.exporter.status().map(|s| s.id) {
                self.after_set(|s| s.export_ids.push(id));
            }
            let r = self.after_export_result().await;
            self.after_set(|s| s.exporting = false);
            if self.after_take_preempted() {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            return r.map(Some);
        }
    }

    async fn after_run(&self, video: &str, after: &AfterRecord, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>, output: &mut Option<String>) -> Result<Vec<String>, String> {
        let ffmpeg = self.ffmpeg_path().ok_or_else(|| tr!("找不到 FFmpeg", "FFmpeg not found").to_string())?;
        let stop_step = self.after.lock().unwrap().as_ref().map(|s| s.stop_step.clone()).unwrap_or_default();
        let mut video = video.to_string();
        let mut done = vec![];
        let mut step_no = 0;
        // 1. 剪掉沒動靜的片段
        if after.idle_cut {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            self.after_begin(step_no, tr!("找出沒動靜的片段", "Finding idle parts").into());
            let info = self.cache.probe(&ffmpeg, &video).await.map_err(|e| e.message().to_string())?;
            let dur = info.duration_sec.unwrap_or(0.0);
            let ranges = crate::idle::find(&ffmpeg, &video, dur, info.has_audio == Some(true)).await.map_err(|e| e.message().to_string())?;
            let cut: f64 = ranges.iter().map(|(a, b)| b - a).sum();
            if cut >= 1.0 {
                self.after_step(tr!("剪掉沒動靜的片段", "Cutting idle parts").into());
                let spec = EditSpec { start: 0.0, end: dur, removed: ranges.clone(), ..Default::default() };
                // 剪輯設定（與剪輯視窗相同的格式）：之後在剪輯視窗打開剪輯版可以再改
                let project = crate::annotate::ProjectData {
                    v: 1,
                    duration: dur,
                    spec: crate::annotate::ProjectSpec { start: 0.0, end: dur, removed: ranges, crop: None },
                    crop_on: false,
                    anns: vec![],
                    audio: Default::default(),
                    fast: vec![],
                    zoom: 0.0,
                    frame: None,
                    cursor: 0.0,
                    vertical: 0,
                };
                let data = serde_json::to_value(project).ok();
                let (me, src, spec, data) = (self, video.as_str(), &spec, &data);
                let Some(out) = self.after_export(&cancel, move || Box::pin(crate::actions::cut_start(me, src, spec, None, data.clone()))).await? else { return Ok(done) };
                done.push(trf!("剪掉 {} 沒動靜的片段", "cut {} of idle time", human_duration(cut)));
                video = out.clone();
                *output = Some(out);
            } else {
                done.push(tr!("沒有沒動靜的片段", "no idle parts").into());
            }
        }
        // 2. 字幕
        if after.subtitles {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            // 語音辨識需要的檔案在「設定 → 外部元件」下載；還沒下載時跳過字幕（不在背景自己下載幾百 MB）
            let speech = crate::subtitles::find_speech_ffmpeg(&ffmpeg).await;
            let m = crate::subtitles::MODELS[after.subs_model];
            let mut missing = vec![];
            if speech.is_none() {
                missing.push(crate::components::Kind::SpeechFfmpeg);
            }
            if !crate::subtitles::model_ready(&m) {
                missing.push(crate::components::Kind::Model(after.subs_model));
            }
            if let (true, Some(speech)) = (missing.is_empty(), speech) {
                let dur = self.cache.probe(&ffmpeg, &video).await.ok().and_then(|i| i.duration_sec).unwrap_or(0.0);
                let lang = crate::subtitles::LANGUAGES[after.subs_lang].0;
                // 辨識很吃 CPU：開始錄影時停下來，錄完再重新辨識
                let cues = loop {
                    if !self.after_wait_idle(&cancel).await {
                        return Ok(done);
                    }
                    self.after_begin(step_no, tr!("產生字幕", "Generating subtitles").into());
                    self.after_arm();
                    self.after_set(|s| s.transcribing = true);
                    let r = crate::subtitles::transcribe(&speech, &video, dur, &m, lang, progress.clone(), stop_step.clone()).await;
                    self.after_set(|s| s.transcribing = false);
                    if self.after_take_preempted() {
                        continue;
                    }
                    break r?;
                };
                if cancel.load(Ordering::Relaxed) {
                    return Ok(done);
                }
                if cues.is_empty() {
                    done.push(tr!("沒有辨識出說話的內容", "no speech recognized").into());
                } else {
                    let srt = crate::subtitles::save_next_to(&video, &cues)?;
                    done.push(trf!("字幕存成「{}」", "subtitles saved as “{}”", srt.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    // 英文字幕（說英文時原文就是英文，不用再做）
                    if after.subs_en && lang != "en" {
                        let en = loop {
                            if !self.after_wait_idle(&cancel).await {
                                return Ok(done);
                            }
                            self.after_begin(step_no, tr!("翻成英文字幕", "Translating subtitles to English").into());
                            self.after_arm();
                            self.after_set(|s| s.transcribing = true);
                            let r = crate::subtitles::translate(&speech, &video, dur, &m, lang, progress.clone(), stop_step.clone()).await;
                            self.after_set(|s| s.transcribing = false);
                            if self.after_take_preempted() {
                                continue;
                            }
                            break r;
                        };
                        if cancel.load(Ordering::Relaxed) {
                            return Ok(done);
                        }
                        match en {
                            Ok(en) if !en.is_empty() => {
                                let p = crate::subtitles::save_english_next_to(&video, &en)?;
                                done.push(trf!("英文字幕存成「{}」", "English subtitles saved as “{}”", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                            }
                            Ok(_) => {}
                            Err(e) => done.push(trf!("英文字幕沒有做成：{e}", "English subtitles failed: {e}")),
                        }
                    }
                }
            } else {
                crate::warn!("[錄完自動處理] 跳過字幕：還沒下載 {}", crate::components::describe(&missing));
                done.push(trf!("沒有產生字幕：請先到「設定 → 外部元件」下載{}", "no subtitles: download {} in Settings → Components first", crate::components::describe(&missing)));
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(done);
            }
        }
        // 3. 加速版（一次處理多支）
        if after.speed > 1.0 {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            self.after_begin(step_no, trf!("製作 {} 倍加速版", "Making a {}× sped-up version", crate::format::num(after.speed)));
            let (me, src, speed, keep) = (self, video.as_str(), after.speed, after.keep_audio);
            let Some(out) = self.after_export(&cancel, move || Box::pin(async move { me.exporter.start(&me.export_ctx(), src, speed, keep, 0.0, 0.0).await.map(|_| ()) })).await? else {
                return Ok(done);
            };
            done.push(trf!("{} 倍加速版", "{}× sped-up version", crate::format::num(after.speed)));
            *output = Some(out);
        }
        // 4. 壓縮
        if after.compress_mb > 0 {
            step_no += 1;
            if !self.after_wait_idle(&cancel).await {
                return Ok(done);
            }
            let bytes = std::fs::metadata(&video).map(|m| m.len()).unwrap_or(0);
            if bytes <= after.compress_mb as u64 * 1024 * 1024 {
                done.push(trf!("已經小於 {} MB，不用壓縮", "already under {} MB, not compressed", after.compress_mb));
            } else {
                self.after_begin(step_no, trf!("壓縮到 {} MB 以內", "Compressing to under {} MB", after.compress_mb));
                let (me, src, mb) = (self, video.as_str(), after.compress_mb as f64);
                let Some(out) = self.after_export(&cancel, move || Box::pin(async move { me.exporter.start(&me.export_ctx(), src, 1.0, true, 0.0, mb).await.map(|_| ()) })).await? else {
                    return Ok(done);
                };
                let size = std::fs::metadata(&out).map(|m| crate::format::format_bytes(m.len())).unwrap_or_default();
                done.push(trf!("壓縮版 {size}", "compressed copy {size}"));
                *output = Some(out);
            }
        }
        Ok(done)
    }

    /// 等轉檔工作結束：完成時回傳輸出檔，取消或失敗時回傳錯誤訊息
    async fn after_export_result(&self) -> Result<String, String> {
        self.exporter.wait().await;
        let s = self.exporter.status().ok_or_else(|| tr!("轉檔沒有結果", "The conversion didn't finish").to_string())?;
        match s.state {
            ExportState::Done => Ok(s.output),
            ExportState::Canceled => Err(tr!("已取消", "Canceled").into()),
            _ => Err(s.message.unwrap_or_else(|| tr!("轉檔失敗", "The conversion failed").into())),
        }
    }
}

/// 資料夾的名稱（訊息裡用）
fn folder_name(dir: &str) -> String {
    Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.to_string())
}

/// 錄完處理的結果要複製的檔案：錄影、處理好的版本，和它們旁邊的字幕（.srt、.en.srt）與 M4A
pub fn result_files(video: &str, output: Option<&str>) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for f in std::iter::once(video).chain(output) {
        let p = Path::new(f);
        if !p.is_file() || out.iter().any(|o: &std::path::PathBuf| o == p) {
            continue;
        }
        out.push(p.to_path_buf());
        for side in [p.with_extension("srt"), p.with_extension("en.srt"), p.with_extension("m4a")] {
            if side.is_file() && !out.contains(&side) {
                out.push(side);
            }
        }
    }
    out
}

/// 複製到資料夾（資料夾沒有就建立；已經有同名檔案時加上 _2、_3…，不覆蓋）；回傳複製了幾個
pub fn copy_results(video: &str, output: Option<&str>, dest: &Path) -> Result<usize, String> {
    if !dest.is_absolute() {
        return Err(tr!("資料夾位置不對，請重新選擇", "The folder location isn't valid. Choose it again").into());
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let files = result_files(video, output);
    for f in &files {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (stem, ext) = match name.split_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (name.clone(), String::new()),
        };
        let to = crate::paths::unique_path(dest, &stem, &ext);
        std::fs::copy(f, &to).map_err(|e| e.to_string())?;
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_the_results_with_subtitles() {
        let dir = tempfile::tempdir().unwrap();
        let v = dir.path().join("Rec_1.mp4");
        let cut = dir.path().join("Rec_1_cut.mp4");
        for f in [&v, &cut, &dir.path().join("Rec_1.srt"), &dir.path().join("Rec_1.en.srt"), &dir.path().join("Rec_1_cut.srt")] {
            std::fs::write(f, b"x").unwrap();
        }
        let (v, cut) = (v.display().to_string(), cut.display().to_string());
        assert_eq!(result_files(&v, Some(&cut)).len(), 5);
        let dest = dir.path().join("sync");
        assert_eq!(copy_results(&v, Some(&cut), &dest), Ok(5));
        assert!(dest.join("Rec_1.en.srt").is_file());
        // 再複製一次：不覆蓋，加上 _2
        assert_eq!(copy_results(&v, None, &dest), Ok(3));
        assert!(dest.join("Rec_1_2.mp4").is_file() && dest.join("Rec_1_2.en.srt").is_file());
        assert!(copy_results(&v, None, Path::new("relative")).is_err());
        let a = AfterRecord::from_ui(Some(&serde_json::json!({ "afterCopyTo": " D:\\Sync " })));
        assert_eq!(a.copy_to, "D:\\Sync");
        assert!(a.any());
    }
    use serde_json::json;

    #[test]
    fn audio_recordings_are_also_saved_as_m4a() {
        assert!(is_audio_recording("C:\\v\\錄音_2026-10-13_10-00-00.mp4") && !is_audio_recording("C:\\v\\Rec_2026-10-13_10-00-00.mp4"));
        assert!(audio_m4a(None) && audio_m4a(Some(&json!({}))) && !audio_m4a(Some(&json!({ "audioM4a": false }))));
        assert_eq!(m4a_path("/v/錄音_1.mp4"), std::path::PathBuf::from("/v/錄音_1.m4a"));
    }

    #[test]
    fn reads_settings() {
        assert!(!AfterRecord::from_ui(None).any());
        let a = AfterRecord::from_ui(Some(&json!({ "afterIdleCut": true, "afterSubs": true, "afterSubsModel": 9, "afterSubsLang": 1, "afterCompressMb": 25, "afterAutoName": true })));
        assert_eq!(a, AfterRecord { idle_cut: true, subtitles: true, subs_model: crate::subtitles::MODELS.len() - 1, subs_lang: 1, compress_mb: 25, auto_name: true, ..Default::default() });
        assert!(a.any());
        assert!(AfterRecord::from_ui(Some(&json!({ "afterCompressMb": 10 }))).any());
    }
}
