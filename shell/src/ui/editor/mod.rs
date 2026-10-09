//! 剪輯視窗：剪頭尾、刪除中間片段、裁切畫面、加上標註，另存為新檔。
//! 影片由 FFmpeg 解碼（core::player），標註與馬賽克 / 模糊的預覽和匯出用同一套繪製（core::annotate、core::effects）。
//!
//! 之前用這支影片做過剪輯（或開啟的就是剪輯版）時，從原始影片重新載入當時的剪輯與標註，修改後取代那個剪輯版。

mod side;
mod stage;
mod timeline;

use super::dialogs::file_name;
use super::theme::{self, Btn};
use super::UiApp;
use eframe::egui::{self, vec2, Align, Color32, Id, Key, Layout, Modifiers, RichText, TextureHandle, TextureOptions};
use screenrecorder_core::actions::{self, EditProject, ProjectMatch};
use screenrecorder_core::annotate::{self, Ann, AnnKind, ProjectData, ProjectSpec, Shape, COLORS, EMOJIS};
use screenrecorder_core::edit::{cut_file_name, keep_ranges, normalize_crop, normalize_ranges, total_length, CropInput, EditSpec, Range};
use screenrecorder_core::format::video_clock;
use screenrecorder_core::player::{Frame, MediaSpec, Player};
use screenrecorder_core::types::LibraryEntry;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// 解碼的最大尺寸（顯示用，匯出不受影響）
const DECODE_MAX: (u32, u32) = (1280, 720);
/// 開啟的序號：查詢專案回來時已經換了影片就不套用
static OPEN_SEQ: AtomicU64 = AtomicU64::new(0);
/// 右側顯示的分頁（下次開啟時沿用）
static LAST_TAB: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Time,
    Crop,
    Ann,
}

/// 標註工具：表情是加上表情符號的文字
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ann(AnnKind),
    Emoji,
}

impl Tool {
    fn kind(self) -> AnnKind {
        match self {
            Tool::Ann(k) => k,
            Tool::Emoji => AnnKind::Text,
        }
    }
    /// 放好一個後繼續放下一個（編號 1、2、3…、表情）
    fn sticky(self) -> bool {
        matches!(self, Tool::Emoji | Tool::Ann(AnnKind::Step))
    }
}

/// 開啟時查到的剪輯專案（說明列用）
enum BannerInfo {
    None,
    /// 找不到剪輯版的原始影片
    MissingSource(String),
    /// 原片有上次的剪輯可以載入
    HasProject {
        output: String,
        data: Value,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TlKind {
    Start,
    End,
    Playhead,
    Range,
    Select,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Start,
    End,
    Body,
}

/// 進行中的拖曳
enum Drag {
    None,
    Timeline {
        kind: TlKind,
        x0: f32,
        t0: f64,
        moved: bool,
        orig: (f64, f64),
    },
    AnnBar {
        id: u64,
        edge: Edge,
        x0: f32,
        t0: f64,
        moved: bool,
        orig: (f64, f64),
    },
    Scroll {
        x0: f32,
        a0: f64,
    },
    Crop {
        from: (f64, f64),
    },
    Create {
        id: u64,
        from: (f64, f64),
    },
    Move {
        id: u64,
        from: (f64, f64),
        orig: Ann,
    },
    Resize {
        id: u64,
        handle: usize,
        orig: Ann,
    },
    /// 拖曳旋轉把手
    Rotate {
        id: u64,
    },
}

pub struct Editor {
    entry: LibraryEntry,
    ffmpeg: PathBuf,
    duration: f64,
    fps: f64,
    vw: f64,
    vh: f64,
    spec: EditSpec,
    /// 在時間軸上拖曳選取、準備刪除的範圍
    sel: Option<Range>,
    /// 預覽結果：只播放保留的部分
    previewing: bool,
    crop_on: bool,
    /// 時間軸顯示的範圍（放大時只顯示一部分）
    view: (f64, f64),
    tab: Tab,

    anns: Vec<Ann>,
    ann_sel: Option<u64>,
    next_id: u64,
    tool: Option<Tool>,
    emoji: usize,
    /// 上次選的馬賽克 / 模糊形狀與範圍（新的沿用）：[馬賽克, 模糊]
    last_style: [(Shape, bool); 2],
    /// 拖曳標註軌時固定每個標註所在的列，避免拖曳中跳列
    lane_freeze: Option<HashMap<u64, usize>>,
    /// 儲存時取代這個剪輯版（修改之前的剪輯）
    replace_target: Option<String>,
    /// 開啟的那個剪輯版（從原片重新載入時記住，可以改回直接剪輯它）
    opened_cut: Option<LibraryEntry>,
    banner: BannerInfo,

    player: Player,
    /// 最新解出的畫面（套用馬賽克 / 模糊前）
    frame: Option<Frame>,
    video_tex: Option<TextureHandle>,
    video_key: u64,
    overlay: stage::Overlay,
    strip: timeline::Strip,
    emoji_tex: Vec<TextureHandle>,

    drag: Drag,
    wheel_at: Instant,
    /// 新加的文字標註：把焦點移到文字欄並全選
    focus_text: bool,
    /// 出現 / 結束時間的輸入（編輯中不被覆蓋）
    time_text: [String; 2],
    saving: bool,
    close: bool,
    /// 上一格有輸入欄在輸入（Esc 只離開輸入欄）
    typing: bool,
    /// 開啟（或載入專案）時的剪輯設定：關閉時比對有沒有改過
    initial: String,
}

pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        return app.toast("找不到 FFmpeg，無法剪輯", true);
    };
    let seq = OPEN_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let (core, path) = (app.core.clone(), entry.media.path.clone());
    app.spawn(async move { actions::edit_project(&core, &path).await }, move |app, info| {
        if OPEN_SEQ.load(Ordering::Relaxed) != seq || app.editor.is_some() {
            return;
        }
        app.editor = Some(Editor::new(app, entry, ffmpeg, info));
    });
}

impl Editor {
    fn new(app: &UiApp, e: LibraryEntry, ffmpeg: PathBuf, info: Option<EditProject>) -> Editor {
        let tab = match LAST_TAB.load(Ordering::Relaxed) {
            1 => Tab::Crop,
            2 => Tab::Ann,
            _ => Tab::Time,
        };
        let player = make_player(&app.ctx, &ffmpeg, &e);
        let mut ed = Editor {
            entry: e.clone(),
            ffmpeg,
            duration: 0.0,
            fps: 30.0,
            vw: 0.0,
            vh: 0.0,
            spec: EditSpec { start: 0.0, end: 0.0, removed: vec![], crop: None, overlays: vec![] },
            sel: None,
            previewing: false,
            crop_on: false,
            view: (0.0, 0.0),
            tab,
            anns: vec![],
            ann_sel: None,
            next_id: 1,
            tool: None,
            emoji: 0,
            last_style: [(Shape::Rect, false); 2],
            lane_freeze: None,
            replace_target: None,
            opened_cut: None,
            banner: BannerInfo::None,
            player,
            frame: None,
            video_tex: None,
            video_key: 0,
            overlay: stage::Overlay::default(),
            strip: timeline::Strip::default(),
            emoji_tex: emoji_textures(&app.ctx),
            drag: Drag::None,
            wheel_at: Instant::now(),
            focus_text: false,
            time_text: Default::default(),
            saving: false,
            close: false,
            typing: false,
            initial: String::new(),
        };
        match info {
            Some(EditProject { project, matched: ProjectMatch::Output, source: Some(src) }) => {
                // 開啟的是剪輯版：改用原始影片，套用上次的設定
                ed.load(&app.ctx, src);
                ed.opened_cut = Some(e);
                ed.apply_project(&project.data);
                ed.replace_target = Some(ed_path(&ed.opened_cut));
            }
            Some(EditProject { project, matched: ProjectMatch::Output, source: None }) => {
                ed.load(&app.ctx, e);
                ed.banner = BannerInfo::MissingSource(file_name(&project.source));
            }
            Some(EditProject { project, matched: ProjectMatch::Source, .. }) => {
                ed.load(&app.ctx, e);
                ed.banner = BannerInfo::HasProject { output: project.output, data: project.data };
            }
            None => ed.load(&app.ctx, e),
        }
        ed.mark_saved();
        ed
    }

    /// 記下目前的設定（之後關閉時比對有沒有改過）
    fn mark_saved(&mut self) {
        self.initial = serde_json::to_string(&self.project_data()).unwrap_or_default();
    }

    /// 有還沒儲存的修改
    fn dirty(&self) -> bool {
        serde_json::to_string(&self.project_data()).unwrap_or_default() != self.initial
    }

    /// 載入影片並清除所有剪輯設定
    fn load(&mut self, ctx: &egui::Context, e: LibraryEntry) {
        if e.media.path != self.entry.media.path {
            self.player = make_player(ctx, &self.ffmpeg, &e);
        }
        self.duration = e.media.duration_sec.unwrap_or(0.0);
        self.fps = e.media.fps.filter(|f| *f > 0.0).unwrap_or(30.0);
        self.vw = e.media.width.unwrap_or(0) as f64;
        self.vh = e.media.height.unwrap_or(0) as f64;
        self.spec = EditSpec { start: 0.0, end: self.duration, removed: vec![], crop: None, overlays: vec![] };
        self.sel = None;
        self.previewing = false;
        self.crop_on = false;
        self.view = (0.0, self.duration);
        self.anns.clear();
        self.ann_sel = None;
        self.tool = None;
        self.frame = None;
        self.video_key = 0;
        self.strip.reset();
        self.entry = e;
        self.player.seek(0.0);
    }

    /// 套用存起來的剪輯設定與標註（2.1 版存的也可以）
    fn apply_project(&mut self, data: &Value) {
        if data.get("v").and_then(Value::as_u64) != Some(1) {
            return;
        }
        let Some(spec) = data.get("spec").and_then(|s| serde_json::from_value::<ProjectSpec>(s.clone()).ok()) else {
            return;
        };
        let saved_dur = data.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
        let d = self.duration;
        let full = spec.end >= saved_dur - 0.05;
        let start = if spec.start.is_finite() { spec.start.clamp(0.0, d) } else { 0.0 };
        let end = if full || !spec.end.is_finite() { d } else { spec.end.clamp(0.0, d) };
        self.spec = EditSpec { start, end, removed: spec.removed.iter().copied().filter(|(a, b)| a.is_finite() && b.is_finite()).collect(), crop: spec.crop, overlays: vec![] };
        if self.spec.end - self.spec.start < 0.1 {
            self.spec = EditSpec { start: 0.0, end: d, removed: vec![], crop: None, overlays: vec![] };
        }
        self.crop_on = data.get("cropOn").and_then(Value::as_bool).unwrap_or(false) && self.spec.crop.is_some();
        // 一個一個讀：格式不對的標註略過，不影響其他的
        let list = data.get("anns").and_then(Value::as_array).cloned().unwrap_or_default();
        self.anns = list.into_iter().filter_map(|v| serde_json::from_value::<Ann>(v).ok()).filter(|a| [a.x, a.y, a.w, a.h, a.start, a.end, a.size].iter().all(|v| v.is_finite())).collect();
        for a in &mut self.anns {
            a.id = self.next_id;
            self.next_id += 1;
            annotate::measure(a);
        }
    }

    /// 目前的剪輯設定與標註（存起來供之後修改）
    fn project_data(&self) -> ProjectData {
        ProjectData {
            v: 1,
            duration: self.duration,
            spec: ProjectSpec { start: self.spec.start, end: self.spec.end, removed: self.spec.removed.clone(), crop: self.spec.crop },
            crop_on: self.crop_on,
            anns: self.anns.clone(),
        }
    }

    // ───────────── 計算 ─────────────

    fn now(&self) -> f64 {
        self.player.time()
    }

    fn keep(&self) -> Vec<Range> {
        keep_ranges(self.duration, &self.spec)
    }

    fn selected(&self) -> Option<&Ann> {
        self.ann_sel.and_then(|id| self.anns.iter().find(|a| a.id == id))
    }

    fn selected_mut(&mut self) -> Option<&mut Ann> {
        let id = self.ann_sel?;
        self.anns.iter_mut().find(|a| a.id == id)
    }

    fn ann_mut(&mut self, id: u64) -> Option<&mut Ann> {
        self.anns.iter_mut().find(|a| a.id == id)
    }

    /// 馬賽克 / 模糊在前，其他標註在後（預覽與匯出的順序一致）
    fn ordered(&self) -> Vec<&Ann> {
        self.anns.iter().filter(|a| a.kind.is_effect()).chain(self.anns.iter().filter(|a| !a.kind.is_effect())).collect()
    }

    /// 步驟編號依出現時間自動排成 1、2、3…
    fn renumber_steps(&mut self) {
        let mut steps: Vec<(f64, u64)> = self.anns.iter().filter(|a| a.kind == AnnKind::Step).map(|a| (a.start, a.id)).collect();
        steps.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.cmp(&q.1)));
        for (i, (_, id)) in steps.into_iter().enumerate() {
            if let Some(a) = self.ann_mut(id) {
                if a.n != Some(i as u32 + 1) {
                    a.n = Some(i as u32 + 1);
                    annotate::measure(a);
                }
            }
        }
    }

    /// 依開始時間排列，每個標註放進第一個不重疊的列
    fn assign_lanes(&self) -> HashMap<u64, usize> {
        let mut sorted: Vec<&Ann> = self.anns.iter().collect();
        sorted.sort_by(|p, q| p.start.total_cmp(&q.start).then(p.id.cmp(&q.id)));
        let mut ends: Vec<f64> = vec![];
        let mut map = HashMap::new();
        for a in sorted {
            let lane = ends.iter().position(|&e| e <= a.start + 1e-6).unwrap_or(ends.len());
            if lane == ends.len() {
                ends.push(a.end);
            } else {
                ends[lane] = a.end;
            }
            map.insert(a.id, lane);
        }
        map
    }

    /// 新標註的出現時間：從目前位置起 3 秒，但不超出目前所在的保留片段；
    /// 太靠近片段結尾時往前延伸（一定包含目前這一格，放好就看得到）。
    fn default_window(&self, t: f64) -> (f64, f64) {
        let len = 3f64.min(self.duration);
        let seg = self.keep().into_iter().find(|&(a, b)| t >= a - 1e-3 && t < b).unwrap_or((0.0, self.duration));
        let end = seg.1.min(t.max(0.0) + len);
        let start = seg.0.max(t.min(end - len));
        (start, end.max(start + 0.1))
    }

    fn new_ann(&mut self, kind: AnnKind, x: f64, y: f64) -> Ann {
        let vh = if self.vh > 0.0 { self.vh } else { 1080.0 };
        let (start, end) = self.default_window(self.now());
        let emoji = self.tool == Some(Tool::Emoji);
        let mut a = Ann {
            id: self.next_id,
            kind,
            x,
            y,
            w: 0.0,
            h: 0.0,
            start,
            end,
            color: match kind {
                AnnKind::Highlight => "#f5b301".into(),
                AnnKind::Text => "#ffffff".into(),
                _ => COLORS[0].into(),
            },
            size: annotate::default_size(kind, vh),
            text: None,
            bg: false,
            n: None,
            shape: None,
            invert: false,
            rot: 0.0,
        };
        self.next_id += 1;
        if kind == AnnKind::Text {
            a.text = Some(if emoji { EMOJIS[self.emoji].to_string() } else { "說明文字".into() });
            a.bg = !emoji;
            if emoji {
                a.size = (annotate::default_size(AnnKind::Text, vh) * 1.6).round();
            }
        }
        if kind == AnnKind::Step {
            a.n = Some(self.anns.iter().filter(|o| o.kind == AnnKind::Step).count() as u32 + 1);
        }
        if kind.is_effect() {
            let (shape, invert) = self.last_style[(kind == AnnKind::Blur) as usize];
            a.shape = Some(shape);
            a.invert = invert;
        }
        annotate::measure(&mut a);
        if matches!(kind, AnnKind::Text | AnnKind::Step) {
            // 以點的位置為中心
            a.x = (x - a.w / 2.0).clamp(0.0, (self.vw - a.w).max(0.0));
            a.y = (y - a.h / 2.0).clamp(0.0, (self.vh - a.h).max(0.0));
        }
        a
    }

    fn select_ann(&mut self, id: Option<u64>) {
        self.ann_sel = id;
        if id.is_some() {
            self.tab = Tab::Ann;
        }
    }

    fn delete_ann(&mut self, id: u64) {
        self.anns.retain(|a| a.id != id);
        if self.ann_sel == Some(id) {
            self.ann_sel = None;
        }
    }

    /// 改了選取的標註（文字、字級改變時重新計算寬高）
    fn touch_selected(&mut self) {
        let d = self.duration;
        if let Some(a) = self.selected_mut() {
            annotate::measure(a);
            if a.end - a.start < 0.1 {
                a.end = d.min(a.start + 0.1);
            }
        }
    }

    // ───────────── 播放與時間軸 ─────────────

    fn seek(&mut self, t: f64) {
        self.player.seek(t.clamp(0.0, (self.duration - 0.001).max(0.0)));
    }

    fn toggle_play(&mut self) {
        if self.previewing {
            return self.stop_preview();
        }
        if self.player.is_playing() {
            self.player.pause();
        } else {
            if self.now() >= self.duration - 0.05 {
                self.seek(0.0);
            }
            self.player.play();
        }
    }

    /// 前後移動：frame = 一張，否則秒數
    fn step(&mut self, frames: Option<i32>, secs: f64) {
        self.stop_preview();
        self.player.pause();
        let d = match frames {
            Some(n) => n as f64 / self.fps,
            None => secs,
        };
        self.seek(self.now() + d);
    }

    fn set_start(&mut self, t: f64) {
        self.spec.start = t.clamp(0.0, (self.spec.end - 0.1).max(0.0));
    }

    fn set_end(&mut self, t: f64) {
        self.spec.end = t.clamp(self.spec.start + 0.1, self.duration.max(self.spec.start + 0.1));
    }

    fn set_view(&mut self, a: f64, b: f64) {
        let d = self.duration;
        let span = (b - a).clamp(d.min((20.0 / self.fps).max(0.5)), d);
        let start = a.clamp(0.0, (d - span).max(0.0));
        self.view = (start, start + span);
    }

    /// 以時間 t 為中心縮放（factor < 1 放大）
    fn zoom(&mut self, factor: f64, t: f64) {
        let span = self.view.1 - self.view.0;
        let ratio = (t - self.view.0) / span.max(1e-9);
        let next = span * factor;
        self.set_view(t - ratio * next, t - ratio * next + next);
    }

    fn zoomed(&self) -> bool {
        self.view.1 - self.view.0 < self.duration - 0.001
    }

    /// 刪除選取的範圍
    fn delete_selection(&mut self, app_toast: &mut Option<(String, bool)>) {
        let Some(sel) = self.sel.take() else { return };
        if sel.1 - sel.0 < 0.05 {
            *app_toast = Some(("選取的片段太短".into(), true));
        } else {
            let mut list = self.spec.removed.clone();
            list.push(sel);
            self.spec.removed = normalize_ranges(&list, self.duration);
        }
    }

    fn restore_removed(&mut self, i: usize) {
        let mut list = normalize_ranges(&self.spec.removed, self.duration);
        if i < list.len() {
            list.remove(i);
        }
        self.spec.removed = list;
    }

    /// 預覽結果：從保留的第一段開始播放，跳過刪除的片段，播到結尾就停
    fn start_preview(&mut self) {
        let keep = self.keep();
        let Some(first) = keep.first() else { return };
        self.sel = None;
        self.previewing = true;
        let t = self.now();
        // 播放頭已在保留範圍內（且不在最後）就從那裡開始，否則從頭
        let inside = keep.iter().any(|&(a, b)| t >= a && t < b - 0.1);
        let from = if inside { t } else { first.0 };
        self.player.pause();
        self.seek(from);
        self.player.play();
    }

    fn stop_preview(&mut self) {
        if !self.previewing {
            return;
        }
        self.previewing = false;
        self.player.pause();
    }

    /// 預覽中：播到刪除的片段就跳到下一段保留的開頭，播完最後一段就停
    fn preview_tick(&mut self) {
        if !self.previewing {
            return;
        }
        let t = self.now();
        let keep = self.keep();
        if keep.iter().any(|&(a, b)| t >= a - 0.02 && t < b) {
            return;
        }
        if let Some(next) = keep.iter().find(|&&(a, _)| a > t) {
            let a = next.0;
            self.seek(a);
        } else {
            self.stop_preview();
            if let Some(&(_, b)) = keep.last() {
                self.seek(b - 0.001);
            }
        }
    }

    fn set_crop(&mut self, c: CropInput) {
        let r = normalize_crop(Some(c), self.vw as i32, self.vh as i32);
        self.spec.crop = Some(match r {
            Some(r) => CropInput { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 },
            None => CropInput { x: 0.0, y: 0.0, width: self.vw, height: self.vh },
        });
    }

    fn crop_rect(&self) -> CropInput {
        self.spec.crop.unwrap_or(CropInput { x: 0.0, y: 0.0, width: self.vw, height: self.vh })
    }

    /// 標註在輸出影片中看得到嗎（與保留的部分重疊）
    fn in_output(a: &Ann, keep: &[Range]) -> bool {
        keep.iter().map(|&(x, y)| (y.min(a.end) - x.max(a.start)).max(0.0)).sum::<f64>() >= 0.05
    }

    fn gone_count(&self, keep: &[Range]) -> usize {
        self.anns.iter().filter(|a| !Self::in_output(a, keep)).count()
    }

    /// 全部重設
    fn reset(&mut self) {
        self.spec = EditSpec { start: 0.0, end: self.duration, removed: vec![], crop: None, overlays: vec![] };
        self.sel = None;
        self.stop_preview();
        self.crop_on = false;
        self.anns.clear();
        self.ann_sel = None;
        self.tool = None;
    }

    /// 開發用：自動操作（截圖檢查）
    #[cfg(debug_assertions)]
    pub fn dev(&mut self, cmd: &str) {
        let Some((k, v)) = cmd.split_once('=') else {
            return;
        };
        match k {
            "tab" => {
                self.tab = if v == "ann" {
                    Tab::Ann
                } else if v == "crop" {
                    Tab::Crop
                } else {
                    Tab::Time
                }
            }
            "sel" => self.select_ann(v.parse().ok()),
            "seek" => self.seek(v.parse().unwrap_or(0.0)),
            "tool" => self.tool = Some(if v == "emoji" { Tool::Emoji } else { Tool::Ann(serde_json::from_value(Value::String(v.into())).unwrap_or(AnnKind::Text)) }),
            "crop" => {
                self.crop_on = true;
                self.set_crop(CropInput { x: 200.0, y: 100.0, width: 1280.0, height: 720.0 });
            }
            "zoom" => self.set_view(2.0, 2.0 + v.parse::<f64>().unwrap_or(4.0)),
            "selrange" => self.sel = Some((7.0, 8.5)),
            _ => {}
        }
    }

    /// 暫停播放（視窗隱藏時）
    pub fn pause(&mut self) {
        self.previewing = false;
        self.player.pause();
    }

    /// 取得新的畫面，播放到結尾時結束預覽
    fn poll_player(&mut self) {
        if let Some(f) = self.player.take_frame() {
            self.frame = Some(f);
        }
        if self.player.take_ended() {
            self.previewing = false;
        }
        self.preview_tick();
        // 播放時播放頭跑出放大的範圍：跟著捲動
        let t = self.now();
        if self.player.is_playing() && self.zoomed() && (t > self.view.1 || t < self.view.0) {
            let span = self.view.1 - self.view.0;
            self.set_view(t - span * 0.1, t + span * 0.9);
        }
    }

    /// 影片畫面（套用目前看得到的馬賽克 / 模糊）
    fn update_video(&mut self, ctx: &egui::Context) {
        let Some(f) = &self.frame else { return };
        let effects: Vec<&Ann> = self.anns.iter().filter(|a| a.kind.is_effect() && f.time >= a.start && f.time <= a.end).collect();
        let key = hash_of(&(f.time.to_bits(), f.width, serde_json::to_string(&effects).unwrap_or_default()));
        if key == self.video_key && self.video_tex.is_some() {
            return;
        }
        self.video_key = key;
        let mut rgba = f.rgba.clone();
        if self.vw > 0.0 {
            for a in &effects {
                screenrecorder_core::effects::apply(&mut rgba, f.width as usize, f.height as usize, a, self.vw, self.vh);
            }
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &rgba);
        match &mut self.video_tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.video_tex = Some(ctx.load_texture("editor-video", img, TextureOptions::LINEAR)),
        }
    }
}

fn ed_path(e: &Option<LibraryEntry>) -> String {
    e.as_ref().map(|e| e.media.path.clone()).unwrap_or_default()
}

fn make_player(ctx: &egui::Context, ffmpeg: &std::path::Path, e: &LibraryEntry) -> Player {
    let spec =
        MediaSpec { path: e.media.path.clone(), duration: e.media.duration_sec.unwrap_or(0.0), fps: e.media.fps.filter(|f| *f > 0.0).unwrap_or(30.0), has_audio: e.media.has_audio == Some(true) };
    let c = ctx.clone();
    let (w, h) = (e.media.width.unwrap_or(1280), e.media.height.unwrap_or(720));
    Player::new(ffmpeg.to_path_buf(), spec, w, h, DECODE_MAX.0, DECODE_MAX.1, Arc::new(move || c.request_repaint()))
}

/// 這一格的滾輪量（點）：與 DOM 的方向相反，往下捲為負
pub fn wheel_delta(i: &egui::InputState) -> egui::Vec2 {
    i.raw
        .events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { unit, delta, modifiers, .. } if !modifiers.ctrl && !modifiers.command => Some(match unit {
                egui::MouseWheelUnit::Point => *delta,
                egui::MouseWheelUnit::Line => *delta * 50.0,
                egui::MouseWheelUnit::Page => *delta * 400.0,
            }),
            _ => None,
        })
        .fold(egui::Vec2::ZERO, |a, b| a + b)
}

pub fn hash_of<T: std::hash::Hash>(v: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// 表情選單用的彩色表情圖（用與影片相同的繪製，Windows 上是彩色的）
fn emoji_textures(ctx: &egui::Context) -> Vec<TextureHandle> {
    EMOJIS
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let px = 40.0;
            let mut a = Ann {
                id: 0,
                kind: AnnKind::Text,
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                start: 0.0,
                end: 1.0,
                color: "#ffffff".into(),
                size: px,
                text: Some(e.to_string()),
                bg: true,
                n: None,
                shape: None,
                invert: false,
                rot: 0.0,
            };
            annotate::measure(&mut a);
            // 只要表情本身：用深色底的版面量大小，但不畫底色
            a.bg = false;
            let (w, h) = (a.w.ceil() as u32, a.h.ceil() as u32);
            let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
            a.x = 0.0;
            a.y = 0.0;
            annotate::measure(&mut a);
            annotate::draw(&mut pm, &a, tiny_skia::Transform::identity());
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            Some(ctx.load_texture(format!("emoji-{i}"), img, TextureOptions::LINEAR))
        })
        .collect()
}

// ───────────── 視窗 ─────────────

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut ed) = app.editor.take() else {
        return;
    };
    let mut toast: Option<(String, bool)> = None;
    ed.poll_player();
    ed.renumber_steps();
    if app.ask.is_none() {
        keyboard(&mut ed, ctx, &mut toast);
    }
    ed.update_video(ctx);
    if ed.player.is_playing() {
        ctx.request_repaint();
    }

    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 32.0).max(600.0), (screen.height() - 32.0).max(400.0));
    let mut save = false;
    egui::Modal::new(Id::new("editor")).frame(theme::modal_frame(ctx).inner_margin(0)).show(ctx, |ui| {
        ui.set_width(size.x);
        ui.set_height(size.y);
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        let p = theme::pal(ui);

        // 標題
        egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new("剪輯影片").font(theme::font_bold(16.0)));
                    ui.label(RichText::new(&ed.entry.media.name).font(theme::mono(12.0)).color(p.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::new("關閉").ghost().small().show(ui).clicked() {
                        ed.close = true;
                    }
                });
            });
        });
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top() - 4.0, egui::Stroke::new(1.0, p.border));
        banner(&mut ed, ui, ctx);

        // 下方的摘要列固定高度，其他給影片、時間軸與右側面板
        let foot_h = 64.0;
        let body_h = (ui.available_height() - foot_h).max(200.0);
        let full_w = ui.available_width();
        ui.allocate_ui_with_layout(vec2(full_w, body_h), Layout::left_to_right(Align::Min), |ui| {
            ui.add_space(18.0);
            let side_w = 300.0;
            let main_w = (full_w - side_w - 18.0 * 2.0 - 18.0).max(300.0);
            ui.allocate_ui_with_layout(vec2(main_w, body_h), Layout::top_down(Align::Min), |ui| {
                ui.set_width(main_w);
                ui.set_height(body_h);
                main_column(&mut ed, ui, ctx, &mut toast);
            });
            ui.add_space(8.0);
            let r = ui.cursor();
            ui.painter().vline(r.left(), r.top()..=r.top() + body_h - 8.0, egui::Stroke::new(1.0, p.border));
            ui.add_space(12.0);
            ui.allocate_ui_with_layout(vec2(side_w, body_h), Layout::top_down(Align::Min), |ui| {
                ui.set_width(side_w);
                ui.set_height(body_h - 8.0);
                side::show(&mut ed, ui, ctx, &mut toast);
            });
        });

        // 摘要與儲存
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, p.border));
        egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 10)).show(ui, |ui| {
            ui.horizontal(|ui| {
                save = footer(&mut ed, ui);
            });
        });
    });

    if save {
        start_save(app, &mut ed);
    }
    if let Some((t, err)) = toast {
        app.toast(t, err);
    }
    LAST_TAB.store(ed.tab as u8, Ordering::Relaxed);
    ed.typing = ctx.egui_wants_keyboard_input();
    if std::mem::take(&mut ed.close) {
        if !ed.dirty() || ed.saving {
            // 關閉時釋放影片檔（Player 結束時停止 FFmpeg）
            ed.strip.cancel();
            return;
        }
        ed.pause();
        let mut ask = super::dialogs::Ask::confirm("放棄這次的剪輯？", "剪輯、裁切或標註還沒有儲存，關閉後就不見了。", "放棄並關閉", |app, _| {
            app.ask = None;
            if let Some(e) = app.editor.take() {
                e.strip.cancel();
            }
        });
        ask.danger = true;
        app.ask = Some(ask);
    }
    app.editor = Some(ed);
}

/// 說明列：正在修改哪個剪輯版、找不到原片、或原片有上次的剪輯可以載入
fn banner(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let p = theme::pal(ui);
    let (text, warn): (String, bool) = if let Some(target) = &ed.replace_target {
        (format!("正在修改剪輯版「{}」：已從原始影片「{}」載入上次的剪輯與標註，可以直接修改。儲存時會取代這個剪輯版。", file_name(target), ed.entry.media.name), false)
    } else {
        match &ed.banner {
            BannerInfo::None => return,
            BannerInfo::MissingSource(src) => (format!("找不到這個剪輯版的原始影片「{src}」，之前的標註已燒進影片、無法修改；只能在這個檔案上繼續剪輯。"), true),
            BannerInfo::HasProject { output, .. } => (format!("這支影片之前剪輯成「{}」。", file_name(output)), false),
        }
    };
    egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 0)).show(ui, |ui| {
        egui::Frame::new().fill(if warn { p.warn_soft } else { p.accent_soft }).corner_radius(theme::RADIUS_SM).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(text).color(if warn { p.warn } else { p.text }));
                if ed.replace_target.is_some() {
                    if Btn::new("改為另存新的剪輯版").small().show(ui).clicked() {
                        ed.replace_target = None;
                        ed.banner = BannerInfo::None;
                    }
                    if let Some(cut) = ed.opened_cut.clone() {
                        if Btn::new("改成直接剪輯這個檔案").small().show(ui).clicked() {
                            ed.opened_cut = None;
                            ed.replace_target = None;
                            ed.banner = BannerInfo::None;
                            ed.load(ctx, cut);
                            ed.mark_saved();
                        }
                    }
                } else if let BannerInfo::HasProject { output, data } = &ed.banner {
                    if Btn::new("載入上次的剪輯來修改").small().show(ui).clicked() {
                        let (output, data) = (output.clone(), data.clone());
                        ed.apply_project(&data);
                        ed.replace_target = Some(output);
                        ed.banner = BannerInfo::None;
                        ed.mark_saved();
                    }
                }
            });
        });
    });
}

/// 左邊：影片、時間軸、標註軌、播放控制
fn main_column(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    let below = timeline::TL_H + 4.0 + timeline::AT_H + 4.0 + timeline::SCROLL_H + 8.0 + 20.0 + 8.0 + 34.0 + 8.0;
    let stage_h = (ui.available_height() - below - 8.0).max(140.0);
    stage::show(ed, ui, ctx, stage_h);
    timeline::show(ed, ui, ctx, toast);
    transport(ed, ui);
}

fn transport(ed: &mut Editor, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let playing = ed.player.is_playing() && !ed.previewing;
        if Btn::new(if playing { "暫停" } else { "播放" }).icon(if playing { theme::Icon::Pause } else { theme::Icon::Play }).min_width(84.0).show(ui).clicked() {
            ed.toggle_play();
        }
        if Btn::new(if ed.previewing { "停止預覽" } else { "預覽結果" }).ghost().tooltip("只播放保留的部分（跳過刪除的片段），確認剪輯結果").show(ui).clicked() {
            if ed.previewing {
                ed.stop_preview();
            } else {
                ed.start_preview();
            }
        }
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.x = 2.0;
        if Btn::new("−1 秒").ghost().small().tooltip("上一秒（Shift+←）").show(ui).clicked() {
            ed.step(None, -1.0);
        }
        if Btn::new("−1 張").ghost().small().tooltip("上一張（←）").show(ui).clicked() {
            ed.step(Some(-1), 0.0);
        }
        if Btn::new("+1 張").ghost().small().tooltip("下一張（→）").show(ui).clicked() {
            ed.step(Some(1), 0.0);
        }
        if Btn::new("+1 秒").ghost().small().tooltip("下一秒（Shift+→）").show(ui).clicked() {
            ed.step(None, 1.0);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format!("{} / {}", video_clock(ed.now()), video_clock(ed.duration))).font(theme::mono(14.0)));
        });
    });
}

/// 摘要與按鈕；回傳是否按了儲存
fn footer(ed: &mut Editor, ui: &mut egui::Ui) -> bool {
    let p = theme::pal(ui);
    let keep = ed.keep();
    let length = total_length(&keep);
    let crop = if ed.crop_on { normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32) } else { None };
    let size = match &crop {
        Some(c) => format!("{}×{}", c.width, c.height),
        None if ed.vw > 0.0 => format!("{}×{}", ed.vw, ed.vh),
        None => String::new(),
    };
    let gone = ed.gone_count(&keep);
    let unchanged = keep.len() == 1 && keep[0].0 == 0.0 && keep[0].1 >= ed.duration - 0.05 && crop.is_none() && ed.anns.is_empty();
    let mut save = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label("輸出長度 ");
            ui.label(RichText::new(video_clock(length)).font(theme::font_bold(13.5)));
            ui.label(format!("（原 {}）・保留 {} 段", video_clock(ed.duration), keep.len()));
            if !size.is_empty() {
                ui.label("・畫面 ");
                ui.label(RichText::new(size).font(theme::font_bold(13.5)));
            }
            if !ed.anns.is_empty() {
                ui.label(format!("・標註 {} 個", ed.anns.len()));
            }
            if gone > 0 {
                ui.label(RichText::new(format!("（{gone} 個在刪除的片段中，不會出現）")).color(p.warn));
            }
        });
        let line = match &ed.replace_target {
            Some(t) => format!("儲存後取代 {}", file_name(t)),
            None => format!("另存為 {}", cut_file_name(&ed.entry.media.name)),
        };
        ui.label(RichText::new(line).font(theme::font(12.0)).color(p.muted));
    });
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let label = if ed.replace_target.is_some() { "儲存修改" } else { "另存剪輯版" };
        let mut b = Btn::new(label).primary().enabled(!unchanged && length >= 0.1 && !ed.saving);
        if unchanged {
            b = b.tooltip("還沒有任何剪輯、裁切或標註");
        }
        if b.show(ui).clicked() {
            save = true;
        }
        if Btn::new("全部重設").ghost().show(ui).clicked() {
            ed.reset();
        }
    });
    save
}

fn start_save(app: &mut UiApp, ed: &mut Editor) {
    ed.saving = true;
    let overlays = ed.ordered().into_iter().filter_map(|a| annotate::to_overlay(a, ed.vw, ed.vh)).collect();
    let crop =
        if ed.crop_on { normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32).map(|r| CropInput { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 }) } else { None };
    let spec = EditSpec { start: ed.spec.start, end: ed.spec.end, removed: ed.spec.removed.clone(), crop, overlays };
    let project = serde_json::to_value(ed.project_data()).ok();
    let (core, source, replace) = (app.core.clone(), ed.entry.media.path.clone(), ed.replace_target.clone());
    let replacing = replace.is_some();
    app.spawn(async move { actions::cut_start(&core, &source, &spec, replace.as_deref(), project).await }, move |app, r| match r {
        Ok(()) => {
            if let Some(e) = app.editor.take() {
                e.strip.cancel();
            }
            app.dismissed_job = None;
            app.toast(if replacing { "已開始更新剪輯版，進度顯示在右側" } else { "已開始剪輯，進度顯示在右側" }, false);
        }
        Err(e) => {
            if let Some(ed) = &mut app.editor {
                ed.saving = false;
            }
            app.toast(e.message().to_string(), true);
        }
    });
}

/// 快捷鍵：空白 = 播放/暫停、←/→ = 一張（Shift = 一秒）、I/O = 開頭/結尾、Delete = 刪除選取的片段或標註、Esc = 取消或關閉
fn keyboard(ed: &mut Editor, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    if ctx.egui_wants_keyboard_input() || ed.typing {
        // 正在輸入文字（egui 在這一格開始時就因 Esc 離開輸入欄）：Esc 只離開輸入欄
        return;
    }
    let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    if key(Modifiers::NONE, Key::Space) {
        if ed.previewing {
            ed.stop_preview();
        } else {
            ed.toggle_play();
        }
    }
    if key(Modifiers::SHIFT, Key::ArrowLeft) {
        ed.step(None, -1.0);
    }
    if key(Modifiers::SHIFT, Key::ArrowRight) {
        ed.step(None, 1.0);
    }
    if key(Modifiers::NONE, Key::ArrowLeft) {
        ed.step(Some(-1), 0.0);
    }
    if key(Modifiers::NONE, Key::ArrowRight) {
        ed.step(Some(1), 0.0);
    }
    if key(Modifiers::NONE, Key::I) {
        let t = ed.now();
        ed.set_start(t);
    }
    if key(Modifiers::NONE, Key::O) {
        let t = ed.now();
        ed.set_end(t);
    }
    let del = key(Modifiers::NONE, Key::Delete) || key(Modifiers::NONE, Key::Backspace);
    let d = key(Modifiers::NONE, Key::D);
    if (del || d) && ed.sel.is_some() {
        ed.delete_selection(toast);
    } else if del {
        if let Some(id) = ed.ann_sel {
            ed.delete_ann(id);
        }
    }
    if key(Modifiers::NONE, Key::Escape) {
        if ed.sel.is_some() || ed.tool.is_some() || ed.ann_sel.is_some() {
            // 有選取或正在放置時 Esc 只取消，不關閉
            ed.sel = None;
            ed.tool = None;
            ed.ann_sel = None;
        } else {
            ed.close = true;
        }
    }
}

/// 「1:05.3」「65.3」「1:02:03」→ 秒（可為 0）
fn parse_time(text: &str) -> Option<f64> {
    let t = text.trim();
    let re = regex::Regex::new(r"^\d+(?::\d{1,2}){0,2}(?:\.\d+)?$").ok()?;
    if !re.is_match(t) {
        return None;
    }
    Some(t.split(':').fold(0.0, |acc, p| acc * 60.0 + p.parse::<f64>().unwrap_or(0.0)))
}

/// 標註的顏色（馬賽克 / 模糊用灰色）
fn ann_color(a: &Ann) -> Color32 {
    if a.kind.is_effect() {
        return Color32::from_rgb(0x7a, 0x7f, 0x87);
    }
    let (r, g, b) = annotate::parse_color(&a.color);
    Color32::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::parse_time;

    #[test]
    fn times() {
        assert_eq!(parse_time("1:05.5"), Some(65.5));
        assert_eq!(parse_time("65.3"), Some(65.3));
        assert_eq!(parse_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_time("0"), Some(0.0));
        assert_eq!(parse_time("abc"), None);
        assert_eq!(parse_time("1:2:3:4"), None);
    }
}
