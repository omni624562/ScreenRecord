//! 剪輯視窗：剪頭尾、刪除中間片段、裁切畫面、加上標註，另存成新檔。
//! 影片由 FFmpeg 解碼（core::player），標註與馬賽克 / 模糊的預覽和匯出用同一套繪製（core::annotate、core::effects）。
//!
//! 之前用這支影片做過剪輯（或開啟的就是剪輯版）時，從原始影片重新載入當時的剪輯與標註，修改後取代那個剪輯版。
//!
//! 也用來編輯截圖（shot.rs）：沒有時間軸，標註與裁切相同，另有輸出設定（外框、陰影、大小）。
//! 兩種都可以復原 / 重做（Ctrl+Z / Ctrl+Y）。

mod history;
pub mod shot;
mod side;
mod speed;
mod stage;
mod subs;
mod timeline;
mod voice;

use super::dialogs::file_name;
use super::theme::{self, Btn, Icon};
use super::tour::{self, Spot};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, Id, Key, Layout, Modifiers, RichText, TextureHandle, TextureOptions};
use screenrecorder_core::actions::{self, EditProject, ProjectMatch};
use screenrecorder_core::annotate::{self, Ann, AnnKind, ProjectData, ProjectSpec, Shape, COLORS, EMOJIS};
use screenrecorder_core::chapters::Marker;
use screenrecorder_core::edit::{cut_file_name, keep_parts, keep_ranges, normalize_crop, normalize_ranges, output_length, ClickZoom, CropInput, EditSpec, FastRange, Part, Range};
use screenrecorder_core::format::video_clock;
use screenrecorder_core::picture;
use screenrecorder_core::player::{Frame, MediaSpec, Player};
use screenrecorder_core::types::LibraryEntry;
use screenrecorder_core::{tr, trf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// 解碼的最大尺寸（顯示用，匯出不受影響）
const DECODE_MAX: (u32, u32) = (1280, 720);
/// 開啟的序號：查詢專案回來時已經換了影片就不套用
static OPEN_SEQ: AtomicU64 = AtomicU64::new(0);
/// 右側顯示的分頁（下次開啟時沿用）
static LAST_TAB: AtomicU8 = AtomicU8::new(0);
/// 右側面板開啟著（可以收起來讓影片變大；下次開啟時沿用）
static SIDE_OPEN: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 修剪：頭尾、刪除的片段、片段順序
    Time,
    /// 加速：整支加速、局部加速與放慢、定格
    Speed,
    /// 畫面：裁切、跟著點擊放大、游標、直式、背景與圓角（截圖：裁切與旋轉）
    Crop,
    Ann,
    /// 截圖：外框、陰影、大小
    Output,
    /// 影片：字幕（自動產生、匯入、逐段修改、匯出 SRT）
    Subs,
    /// 影片：聲音處理
    Audio,
}

/// 剪輯視窗自己用的資料：和 ProjectData 存在同一個 JSON 裡（剪輯專案、草稿、復原 / 重做）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Extras {
    /// 哪些標註是字幕（在 anns 裡的位置）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    subs: Vec<usize>,
    /// 分割點（原影片的秒數）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    splits: Vec<f64>,
    /// 跟著點擊放大時，這些點擊不放大（點擊的時間）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    zoom_skip: Vec<f64>,
    /// 標記（改過才存；None = 用錄影時加的）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    markers: Option<Vec<Marker>>,
    /// 有標記要放大的地方時，改成每次點擊都放大
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    zoom_all_clicks: bool,
    /// 片頭、片尾
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intro: Option<screenrecorder_core::cards::TitleCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outro: Option<screenrecorder_core::cards::TitleCard>,
    /// 刪掉片段的接縫淡入淡出
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    fade_cuts: bool,
    /// 定格畫面
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    freezes: Vec<screenrecorder_core::edit::Freeze>,
    /// 平滑游標停著不動時隱藏
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    cursor_hide: bool,
    /// 手動放大
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    zoom_regions: Vec<screenrecorder_core::zoom::Region>,
    /// 保留的片段的順序（各片段開頭的毫秒）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    order: Vec<u64>,
    /// 整支加速（0 = 沒有存，當成 1）與保留聲音
    #[serde(default, skip_serializing_if = "is_zero_or_one")]
    speed: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    speed_audio: bool,
    /// 字幕樣式
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sub_style: Option<screenrecorder_core::subtitles::SubStyle>,
    /// 旁白與點擊音效（點擊的時間不存，輸出時才放進去）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    narration: Option<screenrecorder_core::narration::Narration>,
    /// 調色
    #[serde(default, skip_serializing_if = "screenrecorder_core::look::Look::is_default")]
    look: screenrecorder_core::look::Look,
    /// 日期時間浮水印（開始的時間不存，輸出時依錄影的時間算）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stamp: Option<screenrecorder_core::look::Stamp>,
}

fn is_zero_or_one(v: &f64) -> bool {
    *v == 0.0 || *v == 1.0
}

/// 存起來的剪輯專案：ProjectData 加上 Extras（同一層）
#[derive(Serialize)]
struct Saved<'a> {
    #[serde(flatten)]
    data: ProjectData,
    #[serde(flatten)]
    extras: &'a Extras,
}

/// 需要 app 的動作（影片專用；截圖與共用的在 shot::Act）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VAct {
    /// 選擇儲存位置（另存新檔視窗）
    PickOutput,
    /// 字幕匯出成 SRT（剪輯後的時間）
    ExportSrt,
    /// 字幕整理成逐字稿（true = Word，false = TXT）
    ExportTranscript(bool),
    /// 標記重新命名（第幾個）
    RenameMarker(usize),
}

/// 影片播放器；編輯截圖時沒有（所有操作都不做事，時間固定是 0）
pub struct Pl(Option<Player>);

impl Pl {
    fn time(&self) -> f64 {
        self.0.as_ref().map(|p| p.time()).unwrap_or(0.0)
    }
    fn is_playing(&self) -> bool {
        self.0.as_ref().is_some_and(|p| p.is_playing())
    }
    fn take_ended(&self) -> bool {
        self.0.as_ref().is_some_and(|p| p.take_ended())
    }
    fn take_frame(&self) -> Option<Frame> {
        self.0.as_ref()?.take_frame()
    }
    fn recycle(&self, rgba: Vec<u8>) {
        if let Some(p) = &self.0 {
            p.recycle(rgba);
        }
    }
    fn pause(&self) {
        if let Some(p) = &self.0 {
            p.pause();
        }
    }
    fn seek(&self, t: f64) {
        if let Some(p) = &self.0 {
            p.seek(t);
        }
    }
    fn play(&self) {
        if let Some(p) = &self.0 {
            p.play();
        }
    }
    fn set_rate(&self, r: f64) {
        if let Some(p) = &self.0 {
            p.set_rate(r);
        }
    }
    fn set_volume(&self, v: f32) {
        if let Some(p) = &self.0 {
            p.set_volume(v);
        }
    }
}

/// 標註工具：表情符號是加上表情符號的文字
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
    /// 放好一個後繼續放下一個（編號 1、2、3…、表情符號）
    fn sticky(self) -> bool {
        matches!(self, Tool::Emoji | Tool::Ann(AnnKind::Step) | Tool::Ann(AnnKind::Pen))
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
    /// 選取範圍（在縮圖上拖曳）
    Select,
    /// 拖曳播放頭到其他時間（尺規）
    Scrub,
    /// 拖曳選取範圍的開頭 / 結尾
    SelA,
    SelB,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Start,
    End,
    Body,
}

/// 裁切的比例：名稱（中文、英文）、寬 / 高（None = 自由）
pub const CROP_RATIOS: [(&str, &str, Option<f64>); 4] = [("自由", "Free", None), ("16:9", "16:9", Some(16.0 / 9.0)), ("4:3", "4:3", Some(4.0 / 3.0)), ("1:1", "1:1", Some(1.0))];

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
    /// 框選裁切範圍：from 是固定的那個角（拉角調整大小時是對角）；放開時太小就還原成 prev
    Crop {
        from: (f64, f64),
        prev_on: bool,
        prev: Option<CropInput>,
    },
    /// 拖曳移動裁切範圍
    CropMove {
        from: (f64, f64),
        orig: CropInput,
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
    /// 拖曳旋轉把手：按下時游標的角度、標註原本的角度（轉的量 = 游標轉了多少，按下時不會跳）
    Rotate {
        id: u64,
        a0: f64,
        r0: f64,
    },
    /// 畫筆：畫過的點（影片像素）
    Pen {
        id: u64,
        pts: Vec<(f64, f64)>,
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
    /// 在時間軸上拖曳選取、準備刪除的範圍（多段時是最後選的那一段）
    sel: Option<Range>,
    /// 按住 Ctrl 多選的其他幾段（sel 是 None 時不算）
    sel_more: Vec<Range>,
    /// 按著 Ctrl 在縮圖上按下：拖曳是再加選一段，只按一下是加選 / 取消那一段
    sel_add: bool,
    /// 預覽結果：只播放保留的部分
    previewing: bool,
    crop_on: bool,
    /// 裁切的比例（CROP_RATIOS 的位置；0 = 自由）
    crop_ratio: usize,
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
    /// 說明列按了 ✕ 收起來
    banner_hidden: bool,

    player: Pl,
    /// 預覽靜音（M）：旁白試聽等設定的音量照樣記著，取消靜音時還原
    muted: bool,
    out_volume: f32,
    /// 編輯截圖（不是影片）
    shot: Option<shot::Shot>,
    /// 復原 / 重做：每一步是 snapshot() 的 JSON
    undo: Vec<String>,
    redo: Vec<String>,
    /// 每一步改了什麼（和 undo / redo 一一對應，復原清單用）
    undo_labels: Vec<String>,
    redo_labels: Vec<String>,
    /// 最後記下的一步；目前的內容和它不同時，停下來（放開滑鼠、停止打字）就記成新的一步
    committed: String,
    changed_at: Option<Instant>,
    /// 上次算快照的時間；目前的內容和 committed 不同（還沒記成一步）
    history_at: Option<Instant>,
    uncommitted: bool,
    /// 快速鍵要做、需要 app 的事（儲存、複製…）
    pending: Option<shot::Act>,
    /// 標記（原片的時間；可以在剪輯視窗新增、重新命名、刪除）與錄影時加的（比對有沒有改過）
    markers: Vec<Marker>,
    rec_markers: Vec<f64>,
    /// 字幕的標註（id）
    sub_ids: HashSet<u64>,
    /// 用文字剪：字幕清單裡選起來、要一起剪掉的幾句（Ctrl / Shift＋點時間）；最後點的那句
    sub_picked: HashSet<u64>,
    sub_anchor: Option<u64>,
    /// 找到的贅字與停頓（勾選的要剪）；None = 沒有在看
    pub(crate) fillers: Option<Vec<(screenrecorder_core::fillers::Filler, bool)>>,
    /// 分割點（原片的時間）：時間軸分成一段一段，按兩下選取一段
    splits: Vec<f64>,
    /// 時間軸按右鍵的選單：按的地方（原片的時間）與那裡有什麼
    tl_menu: Option<timeline::TlMenu>,
    /// 跟著點擊放大時不放大的點擊（點擊的時間）
    zoom_skip: Vec<f64>,
    /// 需要 app 的動作（影片）
    vpending: Option<VAct>,
    /// 另存新檔視窗選的儲存位置（None = 原影片旁邊的 *_cut.mp4）
    out_path: Option<String>,
    /// 原影片旁邊實際會用的檔名（已經有 _cut 時是 _cut_2…）：(影片, 路徑, 算的時間)，每隔幾秒重新確認
    out_auto: Option<(String, String, Instant)>,
    /// 按了 Ctrl+S（需要 app，在 show 裡儲存）
    save_now: bool,
    /// 「顯示刪掉的部分」：播放時不跳過刪掉的片段（預設跳過，看到的就是剪輯後的樣子）
    show_removed: bool,
    /// 剛按了「全部重設」：提示可以復原
    reset_toast: bool,
    /// 錄影時的滑鼠點擊（秒, x, y；原片的時間）
    clicks: Vec<[f64; 3]>,
    /// 錄影時按快速鍵標記要放大的地方（原片的時間, x, y）
    pub(crate) zooms: Vec<[f64; 3]>,
    /// 有標記要放大的地方時，改成每次點擊都放大（預設只放大標記的地方）
    zoom_all_clicks: bool,
    /// 跟著點擊放大的倍率（0 = 不放大）
    zoom: f64,
    /// 錄影時記下的游標位置（原片的時間）、平滑後的位置（預覽用）、錄影時沒有把游標錄進影片
    cursor: Vec<[f64; 3]>,
    cursor_smooth: Vec<[f64; 3]>,
    no_cursor: bool,
    /// 錄影時麥克風暫時靜音的時段（原片的時間；時間軸上標出來）
    pub(crate) mutes: Vec<[f64; 2]>,
    /// 平滑、放大的游標的大小（0 = 不加）
    cursor_size: f64,
    /// 標註的「我的樣式」（改了會存回設定）
    pub(crate) ann_styles: Vec<screenrecorder_core::annotate::AnnStyle>,
    /// 平滑游標停著不動時隱藏
    pub(crate) cursor_hide: bool,
    /// 「速度」分頁：做成指定長度的輸入、選取片段的自訂倍率
    pub(crate) speed_target: String,
    pub(crate) sel_speed: f64,
    /// 逐字稿：每段加上時間、去掉贅字
    pub(crate) transcript_time: bool,
    pub(crate) transcript_clean: bool,
    /// 正在影片上框選要手動放大的地方
    pub(crate) zoom_pick: bool,
    /// 框選手動放大時按下的位置（0～1）
    pub(crate) zoom_from: Option<(f64, f64)>,
    /// 字幕樣式（套用到全部字幕、新加的字幕）
    pub(crate) sub_style: screenrecorder_core::subtitles::SubStyle,
    /// 平滑游標要隱藏的時段（預覽用）
    pub(crate) cursor_idle: Vec<(f64, f64)>,
    /// 預覽用的游標圖
    cursor_tex: Option<CursorTex>,
    /// 直式影片：0 = 不用、1 = 放在中間、2 = 跟著游標；跟著游標時鏡頭的位置（預覽用，需要時才算）
    vertical: u8,
    vertical_pan: Vec<[f64; 2]>,
    /// 上次加上的圖片（Logo），選單裡可以直接再用
    last_picture: String,
    /// 自動產生字幕（進行中）、選的語言與品質
    subs: Option<subs::SubsJob>,
    subs_lang: usize,
    subs_model: usize,
    /// 產生的字幕：0 = 說的語言、1 = 翻成英文、2 = 中英對照
    subs_out: usize,
    /// 目前用的 FFmpeg 有 whisper（沒有時第一次產生字幕要另外下載語音辨識用的 FFmpeg；提示用）
    ffmpeg_whisper: bool,
    /// 正在用文字辨識找個資
    finding_pii: bool,
    /// 正在分析沒動靜的片段
    finding_idle: bool,
    /// 分析找到的片段（標在時間軸上，等使用者決定刪除或加速）
    pub(crate) idle_found: Option<screenrecorder_core::idle::Found>,
    /// 「清掉空白」要找哪幾種：沒動靜、沒說話、贅字和停頓（要有字幕）
    pub(crate) clean_kinds: [bool; 3],
    /// 預覽到加速的片段時，上次跳過去的位置
    fast_from: Option<f64>,
    /// J / K / L：正數快轉、負數倒轉（1、2、4、8 倍），0 = 沒有
    shuttle: i8,
    /// 時間軸上按下時播放頭的位置（拖曳時貼齊用）與這一格貼齊到的時間
    press_playhead: f64,
    snap_at: Option<f64>,
    /// 快轉、倒轉上次往前 / 往後跳的時候
    shuttle_at: Option<Instant>,
    /// 預覽到定格：停在這個時間，到這個時候再繼續播放
    hold_until: Option<(f64, std::time::Instant)>,
    /// 上一次檢查預覽時的播放位置（看有沒有經過定格的地方）
    last_tick: f64,
    /// 片段改了順序時，預覽正在播第幾段（依新的順序）
    seg_cur: Option<usize>,
    /// 旁白：錄音、試聽
    pub(crate) voice: voice::VoiceState,
    /// 旁白錄完的訊息（下一格顯示）
    voice_msg: Option<(String, bool)>,
    /// 最新解出的畫面（套用馬賽克 / 模糊前）
    frame: Option<Frame>,
    video_tex: Option<TextureHandle>,
    video_key: u64,
    /// 背景與圓角的預覽：(內容, 圖)
    frame_preview: Option<(u64, TextureHandle)>,
    /// 預覽結果時畫面上的背景與圓角：(內容與大小, 底圖, 遮罩)
    stage_frame: Option<(u64, TextureHandle, TextureHandle)>,
    overlay: stage::Overlay,
    strip: timeline::Strip,
    /// 時間軸上的聲音波形
    wave: timeline::Wave,
    /// 片頭、片尾卡片的預覽（內容的 hash, 圖）
    card_tex: [Option<(u64, TextureHandle)>; 2],
    emoji_tex: Vec<TextureHandle>,

    drag: Drag,
    wheel_at: Instant,
    /// 新加的文字標註：把焦點移到文字欄並全選
    focus_text: bool,
    /// 出現 / 結束時間打了看不懂的字：(標註, 哪一欄)，顯示格式提示
    time_bad: Option<(u64, usize)>,
    saving: bool,
    close: bool,
    /// 上一格有輸入欄在輸入（Esc 只離開輸入欄）
    typing: bool,
    /// 開啟（或載入專案）時的剪輯設定：關閉時比對有沒有改過
    initial: String,
    /// 開啟時找到的草稿（上次沒有完成的剪輯），說明列問要不要接著剪；選了之後清掉
    draft: Option<DraftInfo>,
    /// 這次存過草稿的內容（committed 快照）、上次檢查的時間；這次有存過草稿
    draft_saved: String,
    draft_at: Instant,
    draft_written: bool,
    /// 說明列按了「捨棄」：刪掉草稿（需要 app，在 show 最後做）
    discard_draft: bool,
}

/// 開啟時找到的草稿
struct DraftInfo {
    data: Value,
    /// 存的時間（Unix 毫秒）
    saved_at: u64,
}

/// 每隔多久存一次草稿（有改過、內容變了才存）
const DRAFT_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    open_entry(app, entry, false, false);
}

/// 開啟剪輯並選好「直式影片：放在中間」（全部錄影、檢視器的「做成直式影片（Shorts / Reels）…」）
pub fn open_vertical(app: &mut UiApp, entry: LibraryEntry) {
    open_entry(app, entry, false, true);
}

/// 開啟剪輯視窗；resume = 直接套用上次沒有完成的剪輯（草稿），不用再問；vertical = 選好直式影片
fn open_entry(app: &mut UiApp, entry: LibraryEntry, resume: bool, vertical: bool) {
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        return app.toast(tr!("找不到 FFmpeg，無法剪輯", "FFmpeg not found, so videos can't be edited"), true);
    };
    let seq = OPEN_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let (core, path) = (app.core.clone(), entry.media.path.clone());
    app.spawn(
        async move {
            let info = actions::edit_project(&core, &path).await;
            // 標記記在原片上：開啟剪輯版時改讀原片的；草稿也是記在剪輯中的影片（原片）上
            let src = match &info {
                Some(EditProject { matched: ProjectMatch::Output, source: Some(s), .. }) => s.media.path.clone(),
                _ => path,
            };
            let draft = actions::edit_draft(&core, &src).await;
            (info, actions::marks(&core, &src).await, draft)
        },
        move |app, (info, marks, draft)| {
            if OPEN_SEQ.load(Ordering::Relaxed) != seq || app.editor.is_some() {
                return;
            }
            let mut ed = Editor::new(app, entry, ffmpeg, info, marks);
            // 草稿是同一支影片的才用（開啟剪輯版但找不到原片時，剪輯中的就是剪輯版本身）
            if let Some(d) = draft.filter(|d| d.output.eq_ignore_ascii_case(&ed.entry.media.path)) {
                let info = DraftInfo { data: d.data, saved_at: d.saved_at };
                if resume {
                    ed.apply_draft(&info.data);
                } else {
                    ed.draft = Some(info);
                }
            }
            if vertical {
                // 直式影片的設定在「畫面」分頁
                ed.vertical = 1;
                ed.tab = Tab::Crop;
            }
            app.editor = Some(ed);
            if resume {
                app.toast(tr!("已回到上次的剪輯，修改後可以再儲存一次", "Your previous edits are back. Make changes and save again"), false);
            }
        },
    );
}

/// 回到剪輯：剪輯輸出失敗或取消後，重新開啟剪輯視窗並還原當時的剪輯、標註與字幕（存在草稿裡）。
/// video = 剪輯中的影片（工作狀態的 source）
pub fn resume(app: &mut UiApp, video: &str) {
    if app.editor.is_some() {
        return app.toast(tr!("剪輯視窗已經開著，請先關閉", "The editor is already open. Close it first"), true);
    }
    if app.locked() {
        return app.toast(tr!("錄影中無法剪輯，請先停止錄影", "Can't edit while recording. Stop the recording first"), true);
    }
    let core = app.core.clone();
    let p = video.to_string();
    app.spawn(
        async move {
            let ff = core.ffmpeg_path()?;
            core.cache.probe(&ff, &p).await.ok()
        },
        |app, m| match m {
            Some(m) => open_entry(app, LibraryEntry { media: m, exports: vec![], hits: vec![] }, true, false),
            None => app.toast(tr!("找不到剪輯的影片，可能已經移動或刪除", "Can't find the video being edited. It may have been moved or deleted"), true),
        },
    );
}

/// 剪輯的工作結束：失敗或取消時提示，並提供「回到剪輯」（剪輯、標註、字幕都還在）
pub fn cut_ended(app: &mut UiApp, e: &screenrecorder_core::types::ExportStatus) {
    use screenrecorder_core::types::{ExportKind, ExportState};
    if e.kind != ExportKind::Cut || !matches!(e.state, ExportState::Error | ExportState::Canceled) {
        return;
    }
    let error = e.state == ExportState::Error;
    let msg = if error {
        tr!("剪輯失敗，剪輯和標註都還留著", "The edit failed. Your cuts and annotations are kept")
    } else {
        tr!("已取消剪輯，剪輯和標註都還留著", "Editing canceled. Your cuts and annotations are kept")
    };
    let video = e.source.clone();
    app.toast_with(msg, error, vec![super::ToastAction::new(tr!("回到剪輯", "Back to editing"), move |app: &mut UiApp| resume(app, &video))]);
}

/// 預覽用的游標圖：(高, 圖, 尖端位置, 圖的大小)
type CursorTex = (u32, TextureHandle, (i32, i32), (u32, u32));

impl Editor {
    fn new(app: &UiApp, e: LibraryEntry, ffmpeg: PathBuf, info: Option<EditProject>, marks: screenrecorder_core::recorder::Marks) -> Editor {
        let tab = match LAST_TAB.load(Ordering::Relaxed) {
            1 => Tab::Crop,
            2 => Tab::Ann,
            4 => Tab::Subs,
            5 => Tab::Audio,
            _ => Tab::Time,
        };
        let player = Pl(Some(make_player(&app.ctx, &ffmpeg, &e)));
        let mut ed = Editor::base(app, e.clone(), ffmpeg, player, tab);
        // 錄影時記下的標記、點擊、游標（載入專案前先設好：專案裡改過的標記會蓋過去）
        ed.markers = marks.markers.iter().map(|&t| Marker { t, name: String::new() }).collect();
        ed.rec_markers = marks.markers;
        ed.clicks = marks.clicks;
        ed.zooms = marks.zooms;
        ed.cursor_smooth = screenrecorder_core::cursor::smooth(&marks.cursor, 60.0);
        ed.cursor_idle = screenrecorder_core::cursor::idle_ranges(&ed.cursor_smooth);
        ed.cursor = marks.cursor;
        ed.no_cursor = marks.no_cursor;
        ed.mutes = marks.mutes;
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

    /// 空的剪輯視窗（還沒載入影片或圖）
    fn base(app: &UiApp, entry: LibraryEntry, ffmpeg: PathBuf, player: Pl, tab: Tab) -> Editor {
        Editor {
            entry,
            ffmpeg,
            duration: 0.0,
            fps: 30.0,
            vw: 0.0,
            vh: 0.0,
            spec: EditSpec { start: 0.0, end: 0.0, removed: vec![], crop: None, overlays: vec![], ..Default::default() },
            sel: None,
            sel_more: vec![],
            sel_add: false,
            previewing: false,
            crop_on: false,
            crop_ratio: 0,
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
            banner_hidden: false,
            player,
            shot: None,
            undo: vec![],
            redo: vec![],
            undo_labels: vec![],
            redo_labels: vec![],
            committed: String::new(),
            changed_at: None,
            history_at: None,
            uncommitted: false,
            pending: None,
            markers: vec![],
            rec_markers: vec![],
            muted: false,
            out_volume: 1.0,
            sub_ids: HashSet::new(),
            sub_picked: HashSet::new(),
            sub_anchor: None,
            fillers: None,
            splits: vec![],
            tl_menu: None,
            zoom_skip: vec![],
            vpending: None,
            out_path: None,
            out_auto: None,
            save_now: false,
            show_removed: false,
            reset_toast: false,
            clicks: vec![],
            zooms: vec![],
            zoom_all_clicks: false,
            zoom: 0.0,
            cursor: vec![],
            cursor_smooth: vec![],
            no_cursor: false,
            mutes: vec![],
            cursor_size: 0.0,
            cursor_hide: false,
            speed_target: String::new(),
            sel_speed: 1.5,
            transcript_time: true,
            transcript_clean: true,
            zoom_pick: false,
            zoom_from: None,
            cursor_idle: vec![],
            sub_style: Default::default(),
            cursor_tex: None,
            vertical: 0,
            vertical_pan: vec![],
            last_picture: app.s.last_picture.clone(),
            ann_styles: app.s.ann_styles.clone(),
            subs: None,
            subs_lang: 0,
            subs_model: screenrecorder_core::subtitles::MODELS.iter().rposition(screenrecorder_core::subtitles::model_ready).unwrap_or(0),
            subs_out: 0,
            ffmpeg_whisper: app.env.ffmpeg.has_whisper,
            finding_pii: false,
            finding_idle: false,
            idle_found: None,
            clean_kinds: [true; 3],
            fast_from: None,
            shuttle: 0,
            shuttle_at: None,
            press_playhead: 0.0,
            snap_at: None,
            hold_until: None,
            last_tick: 0.0,
            seg_cur: None,
            voice: voice::VoiceState::new(&app.s.mic_id),
            voice_msg: None,
            frame: None,
            video_tex: None,
            video_key: 0,
            frame_preview: None,
            stage_frame: None,
            overlay: stage::Overlay::default(),
            strip: timeline::Strip::default(),
            wave: timeline::Wave::default(),
            card_tex: [None, None],
            emoji_tex: emoji_textures(&app.ctx),
            drag: Drag::None,
            wheel_at: Instant::now(),
            focus_text: false,
            time_bad: None,
            saving: false,
            close: false,
            typing: false,
            initial: String::new(),
            draft: None,
            draft_saved: String::new(),
            draft_at: Instant::now(),
            draft_written: false,
            discard_draft: false,
        }
    }

    /// 記下目前的設定（之後關閉時比對有沒有改過）；復原從這裡開始
    fn mark_saved(&mut self) {
        self.initial = self.snapshot();
        self.committed = self.initial.clone();
        self.undo.clear();
        self.redo.clear();
        self.undo_labels.clear();
        self.redo_labels.clear();
    }

    /// 有還沒儲存的修改
    fn dirty(&self) -> bool {
        self.snapshot() != self.initial
    }

    /// 有還沒儲存的修改（給主畫面在結束程式前確認用；不重新產生快照，最多慢一秒反映）
    pub fn has_unsaved(&self) -> bool {
        !self.saving && (self.uncommitted || self.committed != self.initial)
    }

    /// 草稿的內容：剪輯設定與標註（和剪輯專案同一個格式）、要取代的剪輯版、選的儲存位置；exporting = 已經開始輸出
    fn draft_value(&self, exporting: bool) -> Value {
        serde_json::json!({
            "v": 1,
            "project": self.project_value(),
            "replace": self.replace_target,
            "outPath": self.out_path,
            "exporting": exporting,
        })
    }

    /// 套用草稿（還沒完成的剪輯）：不算儲存過，關閉時會問要不要儲存
    fn apply_draft(&mut self, d: &Value) {
        if let Some(p) = d.get("project") {
            self.apply_project(p);
        }
        // 要取代的剪輯版還在才取代，不在就另存新的
        self.replace_target = d.get("replace").and_then(Value::as_str).filter(|r| std::path::Path::new(r).is_file()).map(String::from);
        if self.replace_target.is_none() {
            self.banner = BannerInfo::None;
        }
        // 選的儲存位置：資料夾還在才用
        self.out_path = d.get("outPath").and_then(Value::as_str).filter(|o| self.replace_target.is_none() && std::path::Path::new(o).parent().is_some_and(std::path::Path::is_dir)).map(String::from);
        self.draft = None;
    }

    /// 目前的編輯內容（復原 / 重做、比對有沒有改過）
    fn snapshot(&self) -> String {
        let opts = self.shot.as_ref().map(|s| s.opts());
        let extras = self.extras();
        serde_json::to_string(&(Saved { data: self.project_data(), extras: &extras }, opts)).unwrap_or_default()
    }

    fn restore(&mut self, snap: &str) {
        let Ok((v, opts)) = serde_json::from_str::<(Value, Option<shot::ShotOpts>)>(snap) else { return };
        let Ok(data) = serde_json::from_value::<ProjectData>(v.clone()) else { return };
        let extras = serde_json::from_value::<Extras>(v).unwrap_or_default();
        self.spec.start = data.spec.start;
        self.spec.end = data.spec.end;
        self.spec.removed = data.spec.removed;
        self.spec.crop = data.spec.crop;
        self.spec.audio = data.audio;
        self.spec.fast = data.fast;
        self.zoom = data.zoom;
        self.cursor_size = data.cursor;
        self.vertical = data.vertical.min(2);
        self.spec.frame = data.frame;
        self.crop_on = data.crop_on;
        self.anns = data.anns;
        self.apply_extras(extras);
        if let (Some(s), Some(o)) = (&mut self.shot, opts) {
            s.set_opts(o);
        }
        // 旋轉的角度可能變了：畫面跟著轉
        shot::sync_rotation(self);
        if self.ann_sel.is_some_and(|id| !self.anns.iter().any(|a| a.id == id)) {
            self.ann_sel = None;
        }
        self.next_id = self.next_id.max(self.anns.iter().map(|a| a.id + 1).max().unwrap_or(1));
        self.drag = Drag::None;
        // 重新產生：JSON 讀回來的小數最後一位可能不同，直接用存的字串會被當成又改了一次（重做就被清掉）
        self.committed = self.snapshot();
        self.uncommitted = false;
    }

    /// 內容變了、而且停下來了（沒有在拖曳、打字停了 0.8 秒）：記成新的一步
    /// input = 這一格有按鍵、點擊、貼上之類可能改到內容的輸入
    fn track_history(&mut self, busy: bool, typing: bool, input: bool) {
        // 快照要把所有標註轉成 JSON（字幕一多就很大），不要每一格都算：
        // 拖曳中不算（放開後再記）；沒有輸入時每秒最多算一次（背景工作加的字幕等也會記下）
        if busy {
            return;
        }
        if !input && self.changed_at.is_none() && self.history_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1)) {
            return;
        }
        // 打字中：已經知道改了，停手 0.8 秒才記成一步，這段時間不用每一格都算快照
        if typing && self.changed_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(800)) {
            return;
        }
        // 一般操作（按鍵、點擊）也最多每 0.2 秒算一次
        if input && self.history_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(200)) {
            if self.changed_at.is_none() {
                // 0.2 秒後再看一次（可能剛好改了東西）
                self.changed_at = Some(Instant::now());
            }
            return;
        }
        self.history_at = Some(Instant::now());
        let now = self.snapshot();
        if now == self.committed {
            self.changed_at = None;
            self.uncommitted = false;
            return;
        }
        self.uncommitted = true;
        let since = *self.changed_at.get_or_insert_with(Instant::now);
        if typing && since.elapsed().as_millis() < 800 {
            return;
        }
        self.commit(now);
    }

    fn commit(&mut self, now: String) {
        let label = history::describe(&self.committed, &now);
        let prev = std::mem::replace(&mut self.committed, now);
        self.undo.push(prev);
        self.undo_labels.push(label);
        // 最多 200 步；字幕很多時每一步都很大，總共超過 64 MB 也先丟掉最舊的
        while self.undo.len() > 200 || (self.undo.len() > 1 && self.undo.iter().map(String::len).sum::<usize>() > 64 << 20) {
            self.undo.remove(0);
            self.undo_labels.remove(0);
        }
        self.redo.clear();
        self.redo_labels.clear();
        self.changed_at = None;
        self.uncommitted = false;
    }

    fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.uncommitted
    }

    fn undo(&mut self) {
        let now = self.snapshot();
        if now != self.committed {
            self.commit(now);
        }
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::take(&mut self.committed));
            self.redo_labels.extend(self.undo_labels.pop());
            self.restore(&prev);
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::take(&mut self.committed));
            self.undo_labels.extend(self.redo_labels.pop());
            self.restore(&next);
        }
    }

    /// 還沒記成一步的修改先記下來（復原清單才列得到）
    fn commit_pending(&mut self) {
        let now = self.snapshot();
        if now != self.committed {
            self.commit(now);
        }
    }

    /// 復原清單選了第 n 步（0 = 最近一步）：一次復原到那一步之前
    fn undo_to(&mut self, n: usize) {
        for _ in 0..=n {
            self.undo();
        }
    }

    /// 編輯截圖（不是影片）
    pub fn is_shot(&self) -> bool {
        self.shot.is_some()
    }

    /// 說明文字裡怎麼稱呼畫面
    fn what(&self) -> &'static str {
        if self.is_shot() {
            tr!("圖", "image")
        } else {
            tr!("影片", "video")
        }
    }

    /// 載入影片並清除所有剪輯設定
    fn load(&mut self, ctx: &egui::Context, e: LibraryEntry) {
        if e.media.path != self.entry.media.path {
            self.player = Pl(Some(make_player(ctx, &self.ffmpeg, &e)));
        }
        self.duration = e.media.duration_sec.unwrap_or(0.0);
        self.fps = e.media.fps.filter(|f| *f > 0.0).unwrap_or(30.0);
        self.vw = e.media.width.unwrap_or(0) as f64;
        self.vh = e.media.height.unwrap_or(0) as f64;
        self.spec = EditSpec { start: 0.0, end: self.duration, removed: vec![], crop: None, overlays: vec![], ..Default::default() };
        self.sel = None;
        self.previewing = false;
        self.crop_on = false;
        self.view = (0.0, self.duration);
        self.anns.clear();
        self.sub_ids.clear();
        self.splits.clear();
        self.zoom_skip.clear();
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
        self.spec = EditSpec { start, end, removed: spec.removed.iter().copied().filter(|(a, b)| a.is_finite() && b.is_finite()).collect(), crop: spec.crop, overlays: vec![], ..Default::default() };
        if self.spec.end - self.spec.start < 0.1 {
            self.spec = EditSpec { start: 0.0, end: d, removed: vec![], crop: None, overlays: vec![], ..Default::default() };
        }
        self.spec.audio = data.get("audio").and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or_default();
        self.spec.fast = data.get("fast").and_then(|a| serde_json::from_value::<Vec<FastRange>>(a.clone()).ok()).unwrap_or_default();
        self.spec.fast.retain(|f| f.end() <= d + 0.05 && screenrecorder_core::edit::valid_speed(f.speed));
        self.zoom = data.get("zoom").and_then(Value::as_f64).filter(|z| (1.0..=4.0).contains(z)).unwrap_or(0.0);
        self.cursor_size = data.get("cursor").and_then(Value::as_f64).filter(|s| screenrecorder_core::cursor::SIZES.contains(s)).unwrap_or(0.0);
        self.vertical = data.get("vertical").and_then(Value::as_u64).filter(|v| *v <= 2).unwrap_or(0) as u8;
        self.spec.frame = data.get("frame").and_then(|f| serde_json::from_value(f.clone()).ok());
        self.crop_on = data.get("cropOn").and_then(Value::as_bool).unwrap_or(false) && self.spec.crop.is_some();
        // 一個一個讀：格式不對的標註略過，不影響其他的
        let list = data.get("anns").and_then(Value::as_array).cloned().unwrap_or_default();
        self.anns = list.into_iter().filter_map(|v| serde_json::from_value::<Ann>(v).ok()).filter(|a| [a.x, a.y, a.w, a.h, a.start, a.end, a.size].iter().all(|v| v.is_finite())).collect();
        for a in &mut self.anns {
            a.id = self.next_id;
            self.next_id += 1;
            annotate::measure(a);
        }
        self.apply_extras(serde_json::from_value::<Extras>(data.clone()).unwrap_or_default());
    }

    /// 剪輯視窗自己用的資料（字幕、分割點、不放大的點擊、改過的標記）
    fn extras(&self) -> Extras {
        let markers_changed = self.markers_changed();
        Extras {
            subs: self.anns.iter().enumerate().filter(|(_, a)| self.sub_ids.contains(&a.id)).map(|(i, _)| i).collect(),
            splits: self.splits.clone(),
            zoom_skip: self.zoom_skip.clone(),
            markers: markers_changed.then(|| self.markers.clone()),
            zoom_all_clicks: self.zoom_all_clicks,
            intro: self.spec.intro.clone(),
            outro: self.spec.outro.clone(),
            fade_cuts: self.spec.fade_cuts,
            freezes: self.spec.freezes.clone(),
            cursor_hide: self.cursor_hide,
            zoom_regions: self.spec.zoom_regions.clone(),
            order: self.spec.order.clone(),
            speed: self.spec.speed,
            speed_audio: self.spec.speed_audio,
            sub_style: (self.sub_style != Default::default()).then(|| self.sub_style.clone()),
            narration: (self.spec.narration != Default::default()).then(|| screenrecorder_core::narration::Narration { clicks: vec![], ..self.spec.narration.clone() }),
            look: self.spec.look.clamped(),
            stamp: self.spec.stamp.map(|st| screenrecorder_core::look::Stamp { start_ms: 0, ..st }),
        }
    }

    fn apply_extras(&mut self, x: Extras) {
        self.sub_ids = x.subs.iter().filter_map(|&i| self.anns.get(i)).filter(|a| a.kind == AnnKind::Text).map(|a| a.id).collect();
        let d = self.duration;
        self.splits = x.splits.into_iter().filter(|t| t.is_finite() && *t > 0.0 && *t < d).collect();
        self.zoom_skip = x.zoom_skip.into_iter().filter(|t| t.is_finite()).collect();
        self.zoom_all_clicks = x.zoom_all_clicks;
        self.markers = match x.markers {
            Some(list) => list.into_iter().filter(|m| m.t.is_finite() && m.t >= 0.0 && m.t <= d).collect(),
            None => self.rec_markers.iter().map(|&t| Marker { t, name: String::new() }).collect(),
        };
        self.markers.sort_by(|a, b| a.t.total_cmp(&b.t));
        self.spec.intro = x.intro;
        self.spec.outro = x.outro;
        self.spec.fade_cuts = x.fade_cuts;
        self.spec.freezes = x.freezes.into_iter().filter(|f| f.at() <= d + 0.05 && (100..=60_000).contains(&f.ms)).collect();
        self.spec.narration = x.narration.unwrap_or_default();
        self.cursor_hide = x.cursor_hide;
        self.spec.zoom_regions = screenrecorder_core::zoom::clean_regions(&x.zoom_regions);
        self.spec.order = x.order;
        self.spec.speed = if x.speed.is_finite() && (1.0..=16.0).contains(&x.speed) { x.speed } else { 1.0 };
        self.spec.speed_audio = x.speed_audio;
        self.sub_style = x.sub_style.unwrap_or_default();
        self.spec.look = x.look.clamped();
        self.spec.stamp = x.stamp.map(|st| screenrecorder_core::look::Stamp { start_ms: 0, corner: st.corner.min(3), size: st.size.min(2) });
    }

    /// 錄影開始的時間（1970 年起的毫秒）：檔名裡的錄影時間，否則用修改時間減掉長度
    pub(crate) fn rec_start_ms(&self) -> Option<i64> {
        let m = &self.entry.media;
        screenrecorder_core::library::name_time_ms(&m.name).or_else(|| (m.mtime > 0.0).then_some(m.mtime - self.duration * 1000.0)).map(|t| t as i64)
    }

    /// 片頭預設的標題：自己取過的檔名（預設的 Rec_時間 名稱就空白）
    fn default_card_title(&self) -> String {
        let stem = self.entry.media.name.rsplit_once('.').map_or(self.entry.media.name.as_str(), |(s, _)| s);
        let stem = stem.trim_end_matches("_cut");
        if screenrecorder_core::naming::is_auto_name(stem) {
            String::new()
        } else {
            stem.replace('_', " ")
        }
    }

    /// 片頭加片尾的秒數
    fn cards_secs(&self) -> f64 {
        [&self.spec.intro, &self.spec.outro].iter().filter_map(|c| c.as_ref().filter(|c| !c.is_empty())).map(|c| c.secs()).sum()
    }

    /// 剪輯專案的 JSON（ProjectData 加上剪輯視窗自己用的資料）
    fn project_value(&self) -> Value {
        let extras = self.extras();
        serde_json::to_value(Saved { data: self.project_data(), extras: &extras }).unwrap_or(Value::Null)
    }

    /// 標記和錄影時加的不一樣（新增、刪除、重新命名）
    fn markers_changed(&self) -> bool {
        self.markers.len() != self.rec_markers.len() || self.markers.iter().zip(&self.rec_markers).any(|(m, &t)| m.t != t || !m.name.is_empty())
    }

    /// 輸出的章節（原影片的時間）：標記改過時用剪輯視窗的（有名稱的用名稱），沒改過就沿用原影片的章節（None）
    fn chapters(&self) -> Option<Vec<screenrecorder_core::types::Chapter>> {
        self.markers_changed().then(|| screenrecorder_core::chapters::from_named(&self.markers, self.duration))
    }

    /// 這個標註是字幕
    fn is_sub(&self, id: u64) -> bool {
        self.sub_ids.contains(&id)
    }

    /// 目前的剪輯設定與標註（存起來供之後修改）
    fn project_data(&self) -> ProjectData {
        ProjectData {
            v: 1,
            duration: self.duration,
            spec: ProjectSpec { start: self.spec.start, end: self.spec.end, removed: self.spec.removed.clone(), crop: self.spec.crop },
            crop_on: self.crop_on,
            anns: self.anns.clone(),
            audio: self.spec.audio,
            fast: self.spec.fast.clone(),
            zoom: self.zoom,
            frame: self.spec.frame.clone(),
            cursor: self.cursor_size,
            vertical: self.vertical,
        }
    }

    // ───────────── 計算 ─────────────

    fn now(&self) -> f64 {
        self.player.time()
    }

    fn keep(&self) -> Vec<Range> {
        keep_ranges(self.duration, &self.spec)
    }

    /// 保留的片段，依調整過的順序
    pub(crate) fn ordered_keep(&self) -> Vec<Range> {
        screenrecorder_core::edit::ordered_ranges(self.duration, &self.spec)
    }

    /// 片段的順序和原本不一樣
    pub(crate) fn is_reordered(&self) -> bool {
        !self.spec.order.is_empty() && self.ordered_keep() != self.keep()
    }

    /// 第 i 段（依目前的順序）往前（-1）或往後（+1）移
    pub(crate) fn move_segment(&mut self, i: usize, dir: i32) {
        let mut list: Vec<u64> = self.ordered_keep().iter().map(|r| (r.0 * 1000.0).round() as u64).collect();
        let j = i as i64 + dir as i64;
        if j < 0 || j as usize >= list.len() || i >= list.len() {
            return;
        }
        list.swap(i, j as usize);
        let natural: Vec<u64> = self.keep().iter().map(|r| (r.0 * 1000.0).round() as u64).collect();
        self.spec.order = if list == natural { vec![] } else { list };
    }

    /// 有開啟平滑、放大的游標（而且有記下游標的位置）
    fn cursor_on(&self) -> bool {
        self.cursor_size > 0.0 && !self.cursor.is_empty()
    }

    /// 跟著點擊放大用哪些點：有按快速鍵標記的地方就只用那些（除非選了每次點擊），否則用錄影時的點擊
    pub(crate) fn zoom_source(&self) -> &[[f64; 3]] {
        if !self.zooms.is_empty() && !self.zoom_all_clicks {
            &self.zooms
        } else {
            &self.clicks
        }
    }

    /// 有開啟跟著點擊放大（而且有點擊或標記、沒有裁切、不是直式影片）
    fn zoom_on(&self) -> bool {
        self.zoom > 1.0 && !self.zoom_source().is_empty() && !self.crop_on && self.vertical == 0
    }

    /// 跟著點擊放大會真的放大（有開、而且至少一個點要放大）
    fn zoom_active(&self) -> bool {
        self.zoom_on() && self.zoom_source().iter().any(|c| !self.click_skipped(c[0]))
    }

    /// 跟著游標的直式影片要跟的位置：游標（沒有記下時用點擊）
    fn vertical_track(&self) -> Vec<[f64; 3]> {
        if self.cursor.is_empty() {
            self.clicks.clone()
        } else {
            self.cursor.clone()
        }
    }

    /// 直式影片的設定（輸出用）
    fn vertical_spec(&self) -> Option<screenrecorder_core::vertical::Vertical> {
        use screenrecorder_core::vertical::Vertical;
        match self.vertical {
            1 => Some(Vertical::Fit),
            2 if !self.vertical_track().is_empty() => Some(Vertical::Follow { track: self.vertical_track() }),
            _ => None,
        }
    }

    /// 輸出的大小（直式影片時是直式畫面的大小）
    fn output_size(&self, crop: Option<screenrecorder_core::types::Rect>) -> Option<(i32, i32)> {
        use screenrecorder_core::vertical;
        if self.vw <= 0.0 {
            return None;
        }
        let (w, h) = crop.map(|c| (c.width, c.height)).unwrap_or((self.vw as i32, self.vh as i32));
        Some(match self.vertical_spec() {
            Some(vertical::Vertical::Fit) => vertical::fit_size(h),
            Some(vertical::Vertical::Follow { .. }) => {
                let (cw, ch) = vertical::follow_size(self.vw as i32, self.vh as i32);
                vertical::follow_scale(cw, ch).unwrap_or((cw, ch))
            }
            None => (w, h),
        })
    }

    /// 「速度」分頁上的數字：變速的片段與定格各算一個，整支加速算一個
    pub(crate) fn speed_count(&self) -> usize {
        self.spec.fast.len() + self.spec.freezes.len() + usize::from(screenrecorder_core::edit::whole_speed(&self.spec) > 1.0)
    }

    /// 保留的片段依加速切開：(開始, 結束, 倍率)
    fn parts(&self) -> Vec<Part> {
        keep_parts(self.duration, &self.spec)
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

    /// 標註大小的基準（以 1080 為 1 倍）：畫面的短邊。用高度的話長截圖（例如 1280×8000）的標註會大得離譜
    pub fn size_base(&self) -> f64 {
        let h = if self.vh > 0.0 { self.vh } else { 1080.0 };
        if self.vw > 0.0 {
            h.min(self.vw)
        } else {
            h
        }
    }

    fn new_ann(&mut self, kind: AnnKind, x: f64, y: f64) -> Ann {
        let vh = self.size_base();
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
            color: style_of(kind, vh).0,
            size: style_of(kind, vh).1,
            text: None,
            bg: false,
            n: None,
            shape: None,
            invert: false,
            rot: 0.0,
            pts: vec![],
            anim: 0,
            bend: 0.0,
        };
        self.next_id += 1;
        if kind == AnnKind::Text {
            a.text = Some(if emoji { EMOJIS[self.emoji].to_string() } else { default_text().into() });
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
        if kind == AnnKind::Spotlight {
            a.shape = Some(annotate::Shape::Round);
        }
        annotate::measure(&mut a);
        if matches!(kind, AnnKind::Text | AnnKind::Step) {
            // 以點的位置為中心
            a.x = (x - a.w / 2.0).clamp(0.0, (self.vw - a.w).max(0.0));
            a.y = (y - a.h / 2.0).clamp(0.0, (self.vh - a.h).max(0.0));
        }
        a
    }

    /// 加上圖片標註：截圖放在中間；影片當作 Logo 放在右下角、整支影片都顯示
    fn add_picture(&mut self, path: &str) -> Result<(), String> {
        if self.vw <= 0.0 {
            return Err(trf!("無法讀取{}尺寸，不能加上圖片", "Couldn't read the {} size, so an image can't be added", self.what()));
        }
        let pic = picture::load(path).ok_or_else(|| picture_error(path))?;
        let shot = self.is_shot();
        let (w, h) = picture::fit_size(pic.width(), pic.height(), self.vw, self.vh, if shot { 0.3 } else { 0.16 });
        let mut a = self.new_ann(AnnKind::Image, 0.0, 0.0);
        a.text = Some(path.to_string());
        (a.w, a.h) = (w, h);
        if shot {
            (a.x, a.y) = (((self.vw - w) / 2.0).round(), ((self.vh - h) / 2.0).round());
        } else {
            let m = (self.vw.min(self.vh) * 0.03).round();
            (a.x, a.y) = ((self.vw - w - m).max(0.0), (self.vh - h - m).max(0.0));
            (a.start, a.end) = (0.0, self.duration);
        }
        let id = a.id;
        self.anns.push(a);
        self.tool = None;
        self.select_ann(Some(id));
        self.last_picture = path.to_string();
        Ok(())
    }

    /// 選取的圖片換成另一張（位置與寬度不變，高度依新圖的比例）
    fn replace_picture(&mut self, path: &str) -> Result<(), String> {
        let pic = picture::load(path).ok_or_else(|| picture_error(path))?;
        let Some(a) = self.selected_mut().filter(|a| a.kind == AnnKind::Image) else {
            return self.add_picture(path);
        };
        a.text = Some(path.to_string());
        let cy = a.y + a.h / 2.0;
        a.h = (a.w * pic.height() as f64 / pic.width().max(1) as f64).round().max(8.0);
        a.y = cy - a.h / 2.0;
        self.last_picture = path.to_string();
        Ok(())
    }

    /// 選取標註：同時取消時間軸上選取的範圍（Delete 只會刪掉最後選的那一個）
    fn select_ann(&mut self, id: Option<u64>) {
        self.ann_sel = id;
        if let Some(id) = id {
            self.sel = None;
            self.tab = if self.is_sub(id) { Tab::Subs } else { Tab::Ann };
        }
    }

    /// 選取時間軸上的範圍：同時取消選取的標註（Delete 只會刪掉最後選的那一個）；取消多選的其他段
    fn set_sel(&mut self, r: Option<Range>) {
        if r.is_some() {
            self.ann_sel = None;
        }
        self.sel = r;
        self.sel_more.clear();
    }

    /// 選取的每一段（Ctrl 多選的加上最後選的；重疊的合併），由前到後
    fn sels(&self) -> Vec<Range> {
        let Some(s) = self.sel else { return vec![] };
        let mut all = self.sel_more.clone();
        all.push(s);
        normalize_ranges(&all, self.duration)
    }

    /// Ctrl＋點一段：加進選取，已經選了就移除
    fn toggle_sel(&mut self, r: Range) {
        let had = self.sel.is_some();
        let mut all: Vec<Range> = if had { self.sel_more.iter().copied().chain(self.sel).collect() } else { vec![] };
        let n = all.len();
        all.retain(|&(a, b)| !((a - r.0).abs() < 0.02 && (b - r.1).abs() < 0.02));
        if all.len() == n {
            all.push(r);
        }
        self.ann_sel = None;
        self.sel = all.pop();
        self.sel_more = all;
    }

    /// 選取的幾段一共幾段、幾秒（說明用）
    fn sel_summary(&self) -> Option<(usize, f64)> {
        let all = self.sels();
        (!all.is_empty()).then(|| (all.len(), all.iter().map(|(a, b)| b - a).sum()))
    }

    fn delete_ann(&mut self, id: u64) {
        self.anns.retain(|a| a.id != id);
        self.sub_ids.remove(&id);
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

    /// 播放 / 暫停。播放時預設跳過刪掉的片段、加速的片段快轉（看到的就是剪輯後的樣子）；開啟「顯示刪掉的部分」時全部照原樣播
    fn toggle_play(&mut self) {
        self.shuttle = 0;
        if self.previewing {
            return self.stop_preview();
        }
        if self.player.is_playing() {
            self.player.pause();
        } else {
            if self.show_removed {
                if self.now() >= self.duration - 0.05 {
                    self.seek(0.0);
                }
            } else {
                // 播放頭在刪掉的地方：從下一段保留的開始（後面沒有了就從頭）
                let keep = self.keep();
                let t = self.now();
                if !keep.iter().any(|&(a, b)| t >= a - 0.02 && t < b - 0.05) {
                    if let Some(&(a, _)) = keep.iter().find(|&&(a, _)| a > t).or(keep.first()) {
                        self.seek(a);
                    }
                }
            }
            self.fast_from = None;
            self.hold_until = None;
            self.last_tick = self.now();
            self.player.play();
        }
    }

    /// J / K / L（剪片軟體常用的）：L 播放，再按一次越來越快（2、4、8 倍）；J 倒轉，再按一次越來越快；K 停
    fn shuttle_key(&mut self, dir: i8) {
        self.stop_preview();
        match dir {
            0 => {
                self.shuttle = 0;
                self.player.pause();
            }
            1 if self.shuttle > 0 => self.shuttle = (self.shuttle + 1).min(4),
            1 => {
                if !self.player.is_playing() {
                    self.toggle_play();
                }
                self.shuttle = 1;
            }
            _ if self.shuttle < 0 => self.shuttle = (self.shuttle - 1).max(-4),
            _ => {
                self.player.pause();
                self.shuttle = -1;
            }
        }
        self.shuttle_at = Some(Instant::now());
    }

    /// 快轉、倒轉的倍數
    fn shuttle_speed(&self) -> f64 {
        [1.0, 2.0, 4.0, 8.0][(self.shuttle.unsigned_abs() as usize).clamp(1, 4) - 1]
    }

    /// 快轉時的播放速度（播放器最快 2 倍，更快的另外往後跳）
    fn shuttle_rate(&self) -> f64 {
        if self.shuttle >= 2 {
            2.0
        } else {
            1.0
        }
    }

    /// 快轉 4、8 倍：用 2 倍速播，每 0.5 秒再往後跳；倒轉：暫停著每一小段時間往前跳（跳過刪掉的片段）
    fn shuttle_tick(&mut self) {
        let Some(at) = self.shuttle_at else { return };
        let dt = at.elapsed().as_secs_f64();
        if self.shuttle > 0 {
            if !self.player.is_playing() {
                self.shuttle = 0;
            } else if self.shuttle >= 3 && dt >= 0.5 {
                self.shuttle_at = Some(Instant::now());
                self.seek(self.now() + dt * (self.shuttle_speed() / 2.0 - 1.0));
            }
        } else if self.shuttle < 0 {
            if self.player.is_playing() {
                self.shuttle = 0;
                return;
            }
            if dt < 0.12 {
                return;
            }
            self.shuttle_at = Some(Instant::now());
            let mut t = self.now() - dt.min(0.5) * self.shuttle_speed();
            if !self.show_removed {
                let keep = self.keep();
                if !keep.iter().any(|&(a, b)| t >= a && t < b) {
                    match keep.iter().rev().find(|&&(_, b)| b <= t + 1e-6) {
                        Some(&(_, b)) => t = b - 0.05,
                        None => t = -1.0,
                    }
                }
            }
            let floor = if self.show_removed { 0.0 } else { self.keep().first().map_or(0.0, |r| r.0) };
            if t <= floor {
                t = floor;
                self.shuttle = 0;
            }
            self.seek(t);
        }
    }

    /// 時間軸上刪掉的片段收起來（沒有開啟「顯示刪掉的部分」時）
    fn collapse_removed(&self) -> bool {
        !self.show_removed
    }

    /// 播放時跳過刪掉的片段（預覽結果，或沒有開啟「顯示刪掉的部分」）
    fn skipping(&self) -> bool {
        self.previewing || (!self.show_removed && self.player.is_playing())
    }

    /// 前後移動：frame = 一張，否則秒數
    /// 預覽的音量（靜音時先記著）
    fn set_out_volume(&mut self, v: f32) {
        self.out_volume = v;
        self.player.set_volume(if self.muted { 0.0 } else { v });
    }

    fn toggle_mute(&mut self) -> (String, bool) {
        self.muted = !self.muted;
        let v = self.out_volume;
        self.set_out_volume(v);
        (if self.muted { tr!("預覽靜音（再按 M 取消）", "Preview muted (press M again to unmute)") } else { tr!("預覽有聲音了", "Preview sound on") }.into(), false)
    }

    fn step(&mut self, frames: Option<i32>, secs: f64) {
        self.shuttle = 0;
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

    /// 刪除選取的範圍（提示刪了哪一段、可以復原）；碰到開頭或結尾的就是剪掉頭尾（和拖曳黃色把手一樣）
    fn delete_selection(&mut self, app_toast: &mut Option<(String, bool)>) {
        let (start, end) = (self.spec.start, self.spec.end);
        let mut all: Vec<Range> = self.sels().into_iter().map(|(a, b)| (a.max(start), b.min(end))).filter(|(a, b)| b - a >= 0.05).collect();
        self.sel = None;
        self.sel_more.clear();
        if all.iter().any(|&(a, b)| a <= start + 0.05 && b >= end - 0.05) {
            *app_toast = Some((tr!("不能刪掉全部；要留下一段", "You can't remove everything. Keep at least one part").into(), true));
            return;
        }
        let (mut new_start, mut new_end) = (start, end);
        all.retain(|&(a, b)| {
            if a <= start + 0.05 {
                new_start = new_start.max(b);
                false
            } else if b >= end - 0.05 {
                new_end = new_end.min(a);
                false
            } else {
                true
            }
        });
        let trimmed = new_start != start || new_end != end;
        if trimmed {
            self.set_start(new_start);
            self.set_end(new_end);
            if all.is_empty() {
                let msg = match (new_start != start, new_end != end) {
                    (true, false) => trf!("已剪掉開頭（到 {}），可按 Ctrl+Z 復原", "Trimmed the start (up to {}). Press Ctrl+Z to undo", video_clock(new_start)),
                    (false, true) => trf!("已剪掉結尾（從 {}），可按 Ctrl+Z 復原", "Trimmed the end (from {}). Press Ctrl+Z to undo", video_clock(new_end)),
                    _ => tr!("已剪掉開頭和結尾，可按 Ctrl+Z 復原", "Trimmed the start and end. Press Ctrl+Z to undo").into(),
                };
                *app_toast = Some((msg, false));
                return;
            }
        }
        match all.as_slice() {
            [] => *app_toast = Some((tr!("選取的片段太短", "The selected section is too short").into(), true)),
            [one] => {
                let mut list = self.spec.removed.clone();
                list.push(*one);
                self.spec.removed = normalize_ranges(&list, self.duration);
                *app_toast = Some((trf!("已刪除 {}–{}，可按 Ctrl+Z 復原", "Removed {}–{}. Press Ctrl+Z to undo", video_clock(one.0), video_clock(one.1)), false));
            }
            many => {
                let mut list = self.spec.removed.clone();
                list.extend(many.iter().copied());
                self.spec.removed = normalize_ranges(&list, self.duration);
                let secs: f64 = many.iter().map(|(a, b)| b - a).sum();
                *app_toast = Some((trf!("已刪除 {} 段（共 {}），可按 Ctrl+Z 復原", "Removed {} sections ({} in total). Press Ctrl+Z to undo", many.len(), video_clock(secs)), false));
            }
        }
    }

    /// 找到的片段裡還保留著的部分（太短的不算）
    fn kept_parts(&self, found: &[Range]) -> Vec<Range> {
        let keep = self.keep();
        let mut out: Vec<Range> = vec![];
        for &(a, b) in found {
            for &(c, d) in &keep {
                let (x, y) = (a.max(c), b.min(d));
                if y - x >= 0.5 {
                    out.push((x, y));
                }
            }
        }
        out
    }

    /// 找到的片段加速（只算保留範圍內的）；回傳要顯示的訊息
    pub(crate) fn speed_found(&mut self, found: &[Range], speed: u32) -> String {
        let speed_f = speed as f64;
        let add = self.kept_parts(found);
        if add.is_empty() {
            return tr!("找到的片段都已經刪掉了", "The parts found have already been removed").into();
        }
        for &(a, b) in &add {
            screenrecorder_core::edit::set_fast(&mut self.spec.fast, a, b, speed_f);
        }
        self.sel = None;
        let secs: f64 = add.iter().map(|(a, b)| b - a).sum();
        let n = add.len();
        if screenrecorder_core::i18n::is_en() {
            format!("Sped up {n} part{} ({} in total) to {speed}×. Press Ctrl+Z to undo", if n == 1 { "" } else { "s" }, video_clock(secs))
        } else {
            format!("{n} 段（共 {}）改成 {speed} 倍速，可按 Ctrl+Z 復原", video_clock(secs))
        }
    }

    /// 刪掉找到的沒動靜片段（只算保留範圍內的）；回傳要顯示的訊息
    pub(crate) fn remove_idle(&mut self, found: &[Range]) -> String {
        // 只刪還保留著的部分
        let add = self.kept_parts(found);
        if add.is_empty() {
            return if found.is_empty() {
                trf!("沒有找到超過 {} 秒不動又沒聲音的片段", "No still, silent parts longer than {} s were found", screenrecorder_core::idle::MIN_IDLE)
            } else {
                tr!("找到的片段都已經刪掉了", "The parts found have already been removed").into()
            };
        }
        let secs: f64 = add.iter().map(|(a, b)| b - a).sum();
        let mut list = self.spec.removed.clone();
        list.extend(add.iter().copied());
        self.spec.removed = normalize_ranges(&list, self.duration);
        self.sel = None;
        let n = add.len();
        if screenrecorder_core::i18n::is_en() {
            format!("Removed {n} idle part{} ({} in total). Press Ctrl+Z to undo", if n == 1 { "" } else { "s" }, video_clock(secs))
        } else {
            format!("刪除了 {n} 段沒動靜的片段（共 {}），可按 Ctrl+Z 復原", video_clock(secs))
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
        let keep = self.ordered_keep();
        let Some(first) = keep.first() else { return };
        self.sel = None;
        self.previewing = true;
        let t = self.now();
        // 播放頭已在保留範圍內（且不在最後）就從那裡開始，否則從頭（片段改了順序時從第一段開始）
        let inside = keep.iter().position(|&(a, b)| t >= a && t < b - 0.1);
        self.seg_cur = Some(inside.unwrap_or(0));
        let from = if inside.is_some() { t } else { first.0 };
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

    /// 預覽或播放中：播到刪除的片段就跳到下一段保留的開頭，播完最後一段就停
    fn preview_tick(&mut self) {
        // 定格中：時間到了繼續播放
        if let Some((_, until)) = self.hold_until {
            if std::time::Instant::now() >= until {
                self.hold_until = None;
                self.last_tick = self.now() + 0.001;
                self.player.play();
            } else {
                return;
            }
        }
        voice::tick(self);
        if !self.skipping() {
            self.player.set_rate(self.shuttle_rate());
            return;
        }
        let t = self.now();
        // 片段改了順序：播完一段跳到下一段（依新的順序），全部播完就停
        if self.is_reordered() {
            let order = self.ordered_keep();
            let cur = self.seg_cur.filter(|&i| order.get(i).is_some_and(|r| t >= r.0 - 0.05 && t < r.1 - 0.03));
            match cur.or_else(|| order.iter().position(|r| t >= r.0 - 0.02 && t < r.1 - 0.03)) {
                Some(i) => self.seg_cur = Some(i),
                None => {
                    let next = self.seg_cur.map_or(0, |i| i + 1);
                    match order.get(next) {
                        Some(r) => {
                            self.seg_cur = Some(next);
                            self.seek(r.0);
                        }
                        None => {
                            self.seg_cur = None;
                            self.stop_preview();
                            self.player.pause();
                        }
                    }
                    return;
                }
            }
        }
        let keep = self.keep();
        if keep.iter().any(|&(a, b)| t >= a - 0.02 && t < b) {
            // 經過定格的地方：停在那一格，停夠了再繼續播放
            let last = std::mem::replace(&mut self.last_tick, t);
            if let Some(f) = self.spec.freezes.iter().find(|f| f.at() > last && f.at() <= t + 0.02 && t - last < 1.0).copied() {
                self.player.pause();
                self.seek(f.at());
                self.hold_until = Some((f.at(), std::time::Instant::now() + std::time::Duration::from_secs_f64(f.secs())));
                return;
            }
            // 變速的片段：加速時每播 0.5 秒往後跳 (倍率 - 1) × 0.5 秒，看起來像快轉；放慢時用慢速播放
            // 2 倍以內（含整支加速）直接用播放速度，更快的跳著播
            let part = self.parts().into_iter().find(|p| !p.is_hold() && (p.rate - 1.0).abs() > 1e-9 && t >= p.a - 0.02 && t < p.b);
            let rate = part.map_or(1.0, |p| p.rate);
            self.player.set_rate(if rate <= 2.0 { (rate * self.shuttle_rate()).min(2.0) } else { 1.0 });
            match part.filter(|p| p.rate > 2.0) {
                Some(p) => {
                    let from = *self.fast_from.get_or_insert(t);
                    if t - from >= 0.5 || t < from {
                        let to = (t + 0.5 * (p.rate - 1.0)).min(p.b);
                        self.fast_from = Some(to);
                        self.seek(to);
                    }
                }
                None => self.fast_from = None,
            }
            return;
        }
        if let Some(next) = keep.iter().find(|&&(a, _)| a > t) {
            let a = next.0;
            self.seek(a);
        } else {
            self.stop_preview();
            self.player.pause();
            if let Some(&(_, b)) = keep.last() {
                self.seek(b - 0.001);
            }
        }
    }

    // ───────────── 分割、標記、點擊 ─────────────

    /// 時間軸上的分界：保留範圍的頭尾、分割點、刪掉的片段的兩端（排序、不重複）
    fn boundaries(&self) -> Vec<f64> {
        let (s, e) = (self.spec.start, self.spec.end);
        let mut list = vec![s, e];
        list.extend(self.splits.iter().copied().filter(|t| *t > s && *t < e));
        for (a, b) in normalize_ranges(&self.spec.removed, self.duration) {
            list.extend([a, b].into_iter().filter(|t| *t > s && *t < e));
        }
        list.sort_by(f64::total_cmp);
        list.dedup_by(|a, b| (*a - *b).abs() < 0.02);
        list
    }

    /// 時間 t 所在的一段（分割點之間、沒有被刪掉的）；沒有分割點時沒有（整支影片不算一段）
    fn segment_at(&self, t: f64) -> Option<Range> {
        if self.splits.is_empty() {
            return None;
        }
        let b = self.boundaries();
        let (x, y) = b.windows(2).map(|w| (w[0], w[1])).find(|&(x, y)| t >= x && t < y)?;
        let mid = (x + y) / 2.0;
        let removed = normalize_ranges(&self.spec.removed, self.duration).iter().any(|&(a, b)| mid >= a && mid <= b);
        (!removed && y - x >= 0.05).then_some((x, y))
    }

    /// 在播放頭的位置分割；回傳要顯示的訊息
    fn split_here(&mut self) -> (String, bool) {
        self.split_at(self.now())
    }

    /// 在時間 t 分割（時間軸按右鍵「在這裡分割」）；回傳要顯示的訊息
    fn split_at(&mut self, t: f64) -> (String, bool) {
        if t <= self.spec.start + 0.05 || t >= self.spec.end - 0.05 {
            return (tr!("開頭、結尾不用分割，請把播放頭移到中間", "No need to split at the start or end. Move the playhead somewhere in between").into(), true);
        }
        if self.boundaries().iter().any(|b| (b - t).abs() < 0.05) {
            return (tr!("這裡已經分割了", "It's already split here").into(), false);
        }
        self.splits.push(t);
        self.splits.sort_by(f64::total_cmp);
        (trf!("已在 {} 分割，按兩下一段就能選取它（刪除、改速度）", "Split at {}. Double-click a section to select it (delete or change its speed)", video_clock(t)), false)
    }

    /// 在播放頭加上標記（輸出時變成章節）；回傳要顯示的訊息
    fn add_marker(&mut self) -> (String, bool) {
        self.add_marker_at(self.now())
    }

    fn add_marker_at(&mut self, t: f64) -> (String, bool) {
        if self.markers.iter().any(|m| (m.t - t).abs() < 0.1) {
            return (tr!("這裡已經有標記了", "There's already a marker here").into(), false);
        }
        self.markers.push(Marker { t, name: String::new() });
        self.markers.sort_by(|a, b| a.t.total_cmp(&b.t));
        let n = self.markers.iter().position(|m| m.t == t).map(|i| i + 1).unwrap_or(1);
        (trf!("已在 {} 加上標記 {}（按兩下旗子可以重新命名，輸出時變成章節）", "Added marker {1} at {0} (double-click the flag to rename it; it becomes a chapter)", video_clock(t), n), false)
    }

    /// 標記的名稱（沒有名稱時是「標記 N」）
    fn marker_name(&self, i: usize) -> String {
        match self.markers.get(i) {
            Some(m) if !m.name.trim().is_empty() => m.name.trim().to_string(),
            _ => trf!("標記 {}", "Marker {}", i + 1),
        }
    }

    /// 跟著點擊放大要用的點擊（不含設定成這次不放大的）
    /// 手動放大（原影片的時間；裁切、直式影片時不用）
    pub(crate) fn region_spans(&self) -> Vec<screenrecorder_core::zoom::Span> {
        if self.crop_on || self.vertical != 0 {
            return vec![];
        }
        screenrecorder_core::zoom::clean_regions(&self.spec.zoom_regions)
            .iter()
            .map(|r| {
                let (cx, cy) = r.center();
                (r.start, r.end, r.factor(), cx, cy)
            })
            .collect()
    }

    /// 有開啟點擊音效（而且錄影時有記下點擊）
    pub(crate) fn click_sound_on(&self) -> bool {
        self.spec.narration.click_volume > 0 && !self.clicks.is_empty()
    }

    fn zoom_clicks(&self) -> Vec<[f64; 3]> {
        self.zoom_source().iter().filter(|c| !self.click_skipped(c[0])).copied().collect()
    }

    fn click_skipped(&self, t: f64) -> bool {
        self.zoom_skip.iter().any(|s| (s - t).abs() < 1e-3)
    }

    /// 這次點擊放不放大
    fn toggle_click(&mut self, t: f64) {
        if self.click_skipped(t) {
            self.zoom_skip.retain(|s| (s - t).abs() >= 1e-3);
        } else {
            self.zoom_skip.push(t);
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

    /// 選了比例：在目前的裁切範圍（沒有時是整張）裡放一個最大、置中的那個比例的範圍
    fn apply_crop_ratio(&mut self) {
        let Some(r) = CROP_RATIOS.get(self.crop_ratio).and_then(|c| c.2) else { return };
        if self.vw <= 0.0 || self.vh <= 0.0 {
            return;
        }
        let base = if self.crop_on { self.crop_rect() } else { CropInput { x: 0.0, y: 0.0, width: self.vw, height: self.vh } };
        let (w, h) = if base.width / base.height > r { (base.height * r, base.height) } else { (base.width, base.width / r) };
        self.crop_on = true;
        self.set_crop(CropInput { x: base.x + (base.width - w) / 2.0, y: base.y + (base.height - h) / 2.0, width: w, height: h });
    }

    /// 標註在輸出影片中看得到嗎（與保留的部分重疊）
    fn in_output(a: &Ann, keep: &[Range]) -> bool {
        keep.iter().map(|&(x, y)| (y.min(a.end) - x.max(a.start)).max(0.0)).sum::<f64>() >= 0.05
    }

    fn gone_count(&self, keep: &[Range]) -> usize {
        self.anns.iter().filter(|a| !Self::in_output(a, keep)).count()
    }

    /// 什麼都還沒改（和原影片 / 原圖一樣，不用存）
    fn unchanged(&self) -> bool {
        if self.is_shot() {
            // 與 shot.rs 的判斷相同
            let s = shot::spec(self);
            return s.crop.is_none() && s.anns.is_empty() && s.border.is_none() && !s.shadow && s.scale == 100 && s.rotate == 0 && s.radius == 0 && s.background.is_none();
        }
        let keep = self.keep();
        let crop = if self.crop_on && self.vertical != 2 { normalize_crop(self.spec.crop, self.vw as i32, self.vh as i32) } else { None };
        keep.len() == 1
            && keep[0].0 == 0.0
            && keep[0].1 >= self.duration - 0.05
            && !self.parts().iter().any(Part::retimed)
            && self.spec.zoom_regions.is_empty()
            && self.spec.narration.clips.is_empty()
            && !self.click_sound_on()
            && !self.zoom_active()
            && crop.is_none()
            && self.anns.is_empty()
            && self.spec.audio.is_default()
            && self.spec.frame.is_none()
            && !self.cursor_on()
            && self.vertical_spec().is_none()
            && self.spec.intro.is_none()
            && self.spec.outro.is_none()
            && self.spec.look.is_default()
            && self.spec.stamp.is_none()
    }

    /// 可以儲存（有修改、剪完還有長度）
    fn can_save(&self) -> bool {
        !self.voice.recording() && !self.unchanged() && (self.is_shot() || output_length(&self.parts()) >= 0.1)
    }

    /// 儲存的位置：取代的剪輯版、選的位置，或原影片旁邊實際會用的檔名（*_cut.mp4，已經有就 _cut_2…）
    fn output_target(&mut self) -> String {
        if let Some(r) = &self.replace_target {
            return r.clone();
        }
        if let Some(o) = &self.out_path {
            return o.clone();
        }
        let video = &self.entry.media.path;
        // 確認檔案在不在要讀磁碟：每 3 秒重新算一次（期間其他工作存了 _cut 也跟得上）
        let fresh = self.out_auto.as_ref().is_some_and(|(v, _, at)| v == video && at.elapsed() < std::time::Duration::from_secs(3));
        if !fresh {
            let p = std::path::Path::new(video);
            let dir = p.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
            let out = screenrecorder_core::paths::unique_path(&dir, &screenrecorder_core::format::strip_mp4(&cut_file_name(&self.entry.media.name)), ".mp4");
            self.out_auto = Some((video.clone(), out.to_string_lossy().to_string(), Instant::now()));
        }
        self.out_auto.as_ref().map(|x| x.1.clone()).unwrap_or_default()
    }

    /// 全部重設：剪輯、裁切、標註、字幕、加速、聲音、跟著點擊放大、游標、直式、背景、分割點、標記都回到原本的樣子（可以復原）
    fn reset(&mut self) {
        self.spec = EditSpec { start: 0.0, end: self.duration, removed: vec![], crop: None, overlays: vec![], ..Default::default() };
        self.sel = None;
        self.stop_preview();
        self.crop_on = false;
        self.anns.clear();
        self.sub_ids.clear();
        self.ann_sel = None;
        self.tool = None;
        self.zoom = 0.0;
        self.cursor_size = 0.0;
        self.vertical = 0;
        self.vertical_pan.clear();
        self.splits.clear();
        self.zoom_skip.clear();
        self.markers = self.rec_markers.iter().map(|&t| Marker { t, name: String::new() }).collect();
    }

    /// 開發用：自動操作（截圖檢查）
    #[cfg(debug_assertions)]
    pub fn dev(&mut self, cmd: &str) {
        let Some((k, v)) = cmd.split_once('=') else {
            return;
        };
        match k {
            "tab" => {
                self.tab = match v {
                    "ann" => Tab::Ann,
                    "output" => Tab::Output,
                    "crop" => Tab::Crop,
                    "subs" => Tab::Subs,
                    "audio" => Tab::Audio,
                    "speed" => Tab::Speed,
                    _ => Tab::Time,
                }
            }
            "sel" => self.select_ann(v.parse().ok()),
            "act" if v == "autosubs" => self.pending = Some(shot::Act::AutoSubs),
            "act" if v == "transcript" || v == "transcript-txt" => self.vpending = Some(VAct::ExportTranscript(v == "transcript")),
            // 開發用：fast=開始,結束,倍率；freeze=時間,秒數；voice=時間,秒數[,文字]；clicks=音量
            "fast" | "freeze" | "voice" => {
                let n: Vec<&str> = v.splitn(3, ',').collect();
                let f = |i: usize| n.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
                match k {
                    "fast" => screenrecorder_core::edit::set_fast(&mut self.spec.fast, f(0), f(1), f(2)),
                    "freeze" => self.spec.freezes.push(screenrecorder_core::edit::Freeze { at_ms: (f(0) * 1000.0) as u64, ms: (f(1) * 1000.0) as u64 }),
                    _ => {
                        let path = screenrecorder_core::narration::new_voice_path("dev");
                        let _ = std::fs::write(&path, screenrecorder_core::narration::wav_mono16(&vec![0.0; (f(1) * 48000.0) as usize], 48000));
                        let text = n.get(2).map(|s| s.to_string()).unwrap_or_default();
                        self.spec.narration.put(screenrecorder_core::narration::VoiceClip {
                            file: path.to_string_lossy().into_owned(),
                            at_ms: (f(0) * 1000.0) as u64,
                            ms: (f(1) * 1000.0) as u64,
                            text,
                        });
                    }
                }
            }
            "clicks" => {
                if self.clicks.is_empty() {
                    self.clicks = vec![[1.0, 0.5, 0.5], [3.0, 0.3, 0.3]];
                }
                self.spec.narration.click_volume = v.parse().unwrap_or(60);
            }
            "seek" => self.seek(v.parse().unwrap_or(0.0)),
            // 加一個文字標註：ann=開始,結束（秒）
            "ann" => {
                let t: Vec<f64> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                let (vw, vh) = (self.vw, self.vh);
                let mut a = self.new_ann(AnnKind::Text, vw / 2.0, vh / 2.0);
                if let [s0, s1] = t[..] {
                    (a.start, a.end) = (s0, s1);
                }
                self.anns.push(a);
            }
            "zoommark" => {
                let v: Vec<f64> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                if let [t, x, y] = v[..] {
                    self.zooms.push([t, x, y]);
                }
            }
            "mute" => {
                if let Some((a, b)) = v.split_once(',').and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?))) {
                    self.mutes.push([a, b]);
                }
            }
            "remove" => {
                if let Some((a, b)) = v.split_once(',').and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?))) {
                    self.spec.removed.push((a, b));
                }
            }
            // 開發用：removed=開始,結束；order=毫秒,毫秒…；region=開始,結束,x,y,w,h；subcues=段數；mystyle=顏色,大小
            // 開發用：look=亮度,對比,飽和度,銳利；stamp=角落
            "look" => {
                let n: Vec<i32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                let g = |i: usize| n.get(i).copied().unwrap_or(0);
                self.spec.look = screenrecorder_core::look::Look { brightness: g(0), contrast: g(1), saturation: g(2), sharpen: g(3).max(0) as u32 };
            }
            "stamp" => self.spec.stamp = Some(screenrecorder_core::look::Stamp { corner: v.parse().unwrap_or(0), ..Default::default() }),
            "wspeed" => {
                self.spec.speed = v.parse().unwrap_or(1.0);
                self.spec.speed_audio = self.spec.speed <= 2.0;
            }
            "removed" | "order" | "region" | "subcues" | "mystyle" => {
                let n: Vec<f64> = v.split(',').filter_map(|x| x.trim().trim_start_matches('#').parse().ok()).collect();
                match k {
                    "removed" if n.len() == 2 => self.spec.removed.push((n[0], n[1])),
                    "order" => self.spec.order = n.iter().map(|x| *x as u64).collect(),
                    "region" if n.len() == 6 => self.spec.zoom_regions.push(screenrecorder_core::zoom::Region { start: n[0], end: n[1], x: n[2], y: n[3], w: n[4], h: n[5] }),
                    "subcues" => {
                        let cues: Vec<screenrecorder_core::subtitles::Cue> = (0..n.first().copied().unwrap_or(3.0) as usize)
                            .map(|i| screenrecorder_core::subtitles::Cue { start: i as f64 * 2.0, end: i as f64 * 2.0 + 1.8, text: format!("這是第 {} 句字幕", i + 1) })
                            .collect();
                        self.add_subtitles(&cues);
                    }
                    "mystyle" => {
                        if let Some((c, sz)) = v.split_once(',') {
                            screenrecorder_core::annotate::save_style(&mut self.ann_styles, c, sz.parse().unwrap_or(6.0));
                        }
                    }
                    _ => {}
                }
            }
            "tool" => self.tool = Some(if v == "emoji" { Tool::Emoji } else { Tool::Ann(serde_json::from_value(Value::String(v.into())).unwrap_or(AnnKind::Text)) }),
            "crop" => {
                self.crop_on = true;
                self.set_crop(CropInput { x: 200.0, y: 100.0, width: 1280.0, height: 720.0 });
            }
            "zoom" => self.set_view(2.0, 2.0 + v.parse::<f64>().unwrap_or(4.0)),
            "selrange" => {
                let (a, b) = v.split_once(',').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).unwrap_or((7.0, 8.5));
                self.set_sel(Some((a, b)));
            }
            "picture" => {
                let _ = self.add_picture(v);
            }
            "frame" => self.spec.frame = Some(screenrecorder_core::video_frame::VideoFrame::default()),
            "cursor" => self.cursor_size = v.parse().unwrap_or(2.0),
            "clickzoom" => self.zoom = v.parse().unwrap_or(2.0),
            "preview" => self.start_preview(),
            "vertical" => self.vertical = v.parse().unwrap_or(1),
            "intro" => self.spec.intro = Some(screenrecorder_core::cards::TitleCard { subtitle: "業務部教學".into(), ..screenrecorder_core::cards::TitleCard::new(v) }),
            "outro" => self.spec.outro = Some(screenrecorder_core::cards::TitleCard { style: screenrecorder_core::cards::CardStyle::Blue, ..screenrecorder_core::cards::TitleCard::new(v) }),
            "srt" => {
                if let Ok(c) = screenrecorder_core::subtitles::read_file(std::path::Path::new(v)) {
                    self.add_subtitles(&c);
                }
            }
            "shadow" => {
                if let Some(s) = &mut self.shot {
                    let mut o = s.opts();
                    (o.shadow, o.border, o.scale) = (true, Some("#d0d4da".into()), 50);
                    s.set_opts(o);
                }
            }
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
            // 上一張的記憶體還給播放器，下一張接著用
            if let Some(old) = self.frame.replace(f) {
                self.player.recycle(old.rgba);
            }
        }
        if self.player.take_ended() {
            self.previewing = false;
        }
        self.preview_tick();
        self.shuttle_tick();
        // 播放時播放頭跑出放大的範圍：跟著捲動
        let t = self.now();
        if self.player.is_playing() && self.zoomed() && (t > self.view.1 || t < self.view.0) {
            let span = self.view.1 - self.view.0;
            self.set_view(t - span * 0.1, t + span * 0.9);
        }
    }

    /// 影片畫面（套用目前看得到的馬賽克 / 模糊）
    fn update_video(&mut self, ctx: &egui::Context) {
        if self.is_shot() {
            return shot::update_tiles(self, ctx);
        }
        let Some(f) = &self.frame else { return };
        let effects: Vec<&Ann> = self.anns.iter().filter(|a| a.kind.is_effect() && f.time >= a.start && f.time <= a.end).collect();
        let look = if self.is_shot() { Default::default() } else { self.spec.look.clamped() };
        let key = hash_of(&(f.time.to_bits(), f.width, serde_json::to_string(&effects).unwrap_or_default(), look));
        if key == self.video_key && self.video_tex.is_some() {
            return;
        }
        self.video_key = key;
        let size = [f.width as usize, f.height as usize];
        // 沒有馬賽克 / 模糊時直接用解出來的畫面（不多複製一份）
        let img = if (effects.is_empty() || self.vw <= 0.0) && look.is_default() {
            crate::ui::rgba_image(f.width, f.height, &f.rgba)
        } else {
            let mut rgba = f.rgba.clone();
            // 調色先做（輸出時也是先調色、再疊標註的馬賽克 / 模糊結果相近）
            look.apply_rgba(&mut rgba);
            if self.vw > 0.0 {
                for a in &effects {
                    screenrecorder_core::effects::apply(&mut rgba, f.width as usize, f.height as usize, a, self.vw, self.vh);
                }
            }
            egui::ColorImage::from_rgba_unmultiplied(size, &rgba)
        };
        match &mut self.video_tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.video_tex = Some(ctx.load_texture("editor-video", img, TextureOptions::LINEAR)),
        }
    }
}

/// 新文字標註的預設文字（放好後會全選，直接打字取代）
pub(super) fn default_text() -> &'static str {
    tr!("說明文字", "Your text")
}

/// 讀不到圖片時的訊息
fn picture_error(path: &str) -> String {
    trf!("無法讀取圖片「{}」（支援 PNG、JPG、BMP、WebP）", "Couldn't read the image “{}” (PNG, JPG, BMP and WebP are supported)", file_name(path))
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

/// 表情符號標註（文字只有一個表情符號）
pub fn is_emoji_text(t: &str) -> bool {
    EMOJIS.contains(&t)
}

/// 每種標註上次用的顏色與大小（大小以 1080 高的畫面為準）：新放的沿用，程式執行期間都記得
static TOOL_STYLE: std::sync::LazyLock<std::sync::Mutex<HashMap<AnnKind, (String, f64)>>> = std::sync::LazyLock::new(Default::default);

/// 新標註的顏色與大小（vh = 大小的基準，Editor::size_base）
pub fn style_of(kind: AnnKind, vh: f64) -> (String, f64) {
    let k = vh / 1080.0;
    if let Some((c, s)) = TOOL_STYLE.lock().unwrap_or_else(|e| e.into_inner()).get(&kind) {
        return (c.clone(), (s * k).round().max(1.0));
    }
    let color = match kind {
        AnnKind::Highlight => "#f5b301",
        AnnKind::Text => "#ffffff",
        _ => COLORS[0],
    };
    (color.into(), annotate::default_size(kind, vh))
}

/// 記住這種標註的顏色與大小（之後新放的沿用）
pub fn remember_style(kind: AnnKind, color: &str, size: f64, vh: f64) {
    if kind.is_effect() {
        return;
    }
    let k = (vh / 1080.0).max(1e-6);
    TOOL_STYLE.lock().unwrap_or_else(|e| e.into_inner()).insert(kind, (color.to_string(), size / k));
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

/// 表情符號選單用的彩色表情符號圖（用與影片相同的繪製，Windows 上是彩色的）
fn emoji_textures(ctx: &egui::Context) -> Vec<TextureHandle> {
    // 彩色表情符號畫起來慢：畫過一次就記在 egui 的記憶裡，之後開啟編輯視窗直接用
    let id = Id::new("editor-emoji-textures");
    if let Some(list) = ctx.data(|d| d.get_temp::<Vec<TextureHandle>>(id)) {
        return list;
    }
    let list: Vec<TextureHandle> = EMOJIS
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
                pts: vec![],
                anim: 0,
                bend: 0.0,
            };
            annotate::measure(&mut a);
            // 只要表情符號本身：用深色底的版面量大小，但不畫底色
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
        .collect();
    ctx.data_mut(|d| d.insert_temp(id, list.clone()));
    list
}

// ───────────── 視窗 ─────────────

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut ed) = app.editor.take() else {
        return;
    };
    // 截圖還在讀：只顯示「正在開啟圖片…」（按了關閉就直接關，還沒有修改）
    if shot::show_opening(&mut ed, ctx) {
        if !ed.close {
            app.editor = Some(ed);
        }
        return;
    }
    let mut toast: Option<(String, bool)> = None;
    // 「我的樣式」改了：存回設定（下次開啟、截圖編輯也用得到）
    if ed.ann_styles != app.s.ann_styles {
        app.s.ann_styles = ed.ann_styles.clone();
        app.save_settings();
    }
    ed.poll_player();
    ed.renumber_steps();
    if app.ask.is_none() {
        keyboard(&mut ed, ctx, &mut toast);
    }
    ed.update_video(ctx);
    // 播放中：新的一格到了解碼器會叫畫面更新；這裡只是讓時間與播放點順順地走（不用跟著螢幕更新率每秒畫 60～144 次）
    if ed.player.is_playing() {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
    drop_pictures(app, &mut ed, ctx, &mut toast);

    let screen = ctx.content_rect();
    // 視窗小的時候剪輯視窗佔滿整個視窗（四周不留空）
    let margin = if screen.width() < 1200.0 || screen.height() < 800.0 { 6.0 } else { 32.0 };
    let size = vec2((screen.width() - margin).max(600.0), (screen.height() - margin).max(400.0));
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
                    ui.label(RichText::new(if ed.is_shot() { tr!("編輯截圖", "Edit screenshot") } else { tr!("剪輯影片", "Edit video") }).font(theme::font_bold(16.0)));
                    // 顯示開啟的那個檔案（開啟剪輯版時實際是從原始影片重新套用）
                    let name = match &ed.opened_cut {
                        Some(c) => trf!("{}（從原始影片「{}」重新套用剪輯）", "{} (edits reapplied from the original “{}”)", c.media.name, ed.entry.media.name),
                        None => ed.entry.media.name.clone(),
                    };
                    if ed.is_shot() {
                        ui.add(egui::Label::new(RichText::new(name).font(theme::mono(12.0)).color(p.muted)).truncate());
                    } else {
                        // 檔名 → 輸出的摘要（一行；放不下時滑鼠移上去看完整的）
                        let mut job = egui::text::LayoutJob::default();
                        let fmt = |font: egui::FontId, color: egui::Color32| egui::TextFormat { font_id: font, color, ..Default::default() };
                        job.append(&name, 0.0, fmt(theme::mono(12.0), p.muted));
                        job.append("  →  ", 0.0, fmt(theme::font(12.5), p.muted));
                        let mut full = format!("{name} → ");
                        for (text, bold, warn) in summary_parts(&ed) {
                            let color = if warn {
                                p.warn
                            } else if bold {
                                p.text
                            } else {
                                p.muted
                            };
                            job.append(&text, 0.0, fmt(if bold { theme::font_bold(12.5) } else { theme::font(12.5) }, color));
                            full.push_str(&text);
                        }
                        ui.add(egui::Label::new(job).truncate()).on_hover_text(full);
                    }
                });
                // 標題列只放復原、重做與關閉；其他功能在右側分頁與下方
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                        ed.close = true;
                    }
                    ui.add_space(10.0);
                    // 影片：全部重設、另存剪輯版放在這裡（原本在最下面一排，搬上來讓影片和時間軸大一點）
                    if !ed.is_shot() {
                        let r = ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            save = save_buttons(&mut ed, ui);
                        });
                        tour::mark(ui, Spot::EdFoot, r.response.rect);
                        ui.add(egui::Separator::default().vertical().spacing(12.0));
                    }
                    // 收起 / 開啟右側面板（畫面小的時候影片可以大一點）：畫出視窗和右邊那一欄，箭頭是要往哪邊；
                    // 和復原、重做用分隔線隔開，不會看成同一組的箭頭
                    let open = SIDE_OPEN.load(Ordering::Relaxed);
                    let (icon, tip) = if open {
                        (Icon::PanelHide, tr!("收起右側面板（影片變大）", "Hide the side panel (bigger video)"))
                    } else {
                        (Icon::PanelShow, tr!("開啟右側面板", "Show the side panel"))
                    };
                    if Btn::icon_only(icon).ghost().tooltip(tip).show(ui).clicked() {
                        SIDE_OPEN.store(!open, Ordering::Relaxed);
                    }
                    ui.add(egui::Separator::default().vertical().spacing(12.0));
                    let redo_tip = match ed.redo_labels.last() {
                        Some(l) => trf!("重做：{l}（Ctrl+Y）", "Redo: {l} (Ctrl+Y)"),
                        None => tr!("重做（Ctrl+Y）", "Redo (Ctrl+Y)").into(),
                    };
                    if Btn::icon_only(Icon::Redo).ghost().enabled(!ed.redo.is_empty()).tooltip(redo_tip).show(ui).clicked() {
                        ed.redo();
                    }
                    ui.add_space(4.0);
                    // 復原和復原清單黏成一組（↶⌄，像「另存剪輯版 ▾」）：清單是復原的，不會看成復原、重做共用
                    let (undo, list) = ui
                        .scope(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            // 復原清單：看得到每一步改了什麼，可以一次跳回好幾步之前
                            let list = Btn::icon_only(Icon::ChevD).ghost().small().enabled(ed.can_undo()).tooltip(tr!("復原清單", "Undo history")).show(ui);
                            let undo_tip = match (ed.uncommitted, ed.undo_labels.last()) {
                                (false, Some(l)) => trf!("復原：{l}（Ctrl+Z）", "Undo: {l} (Ctrl+Z)"),
                                _ => tr!("復原（Ctrl+Z）", "Undo (Ctrl+Z)").into(),
                            };
                            let undo = Btn::icon_only(Icon::Undo).ghost().enabled(ed.can_undo()).tooltip(undo_tip).show(ui);
                            (undo, list)
                        })
                        .inner;
                    // 外框框住兩個、中間一條分隔線，看得出是同一個按鈕的兩半
                    let group = undo.rect.union(list.rect);
                    ui.painter().rect_stroke(group, egui::CornerRadius::same(6), egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
                    ui.painter().vline(list.rect.min.x, group.y_range().shrink(6.0), egui::Stroke::new(1.0, p.border));
                    if list.clicked() {
                        ed.commit_pending();
                    }
                    let mut pick = None;
                    egui::Popup::menu(&list).show(|ui| {
                        ui.set_min_width(240.0);
                        ui.label(theme::muted(ui, tr!("點一步：復原到那一步之前", "Click a step to undo back to before it")).font(theme::font(12.0)));
                        for (i, l) in ed.undo_labels.iter().rev().take(20).enumerate() {
                            if ui.button(l).clicked() {
                                pick = Some(i);
                            }
                        }
                        if ed.undo_labels.len() > 20 {
                            ui.label(theme::muted(ui, &trf!("還有 {} 步", "{} more steps", ed.undo_labels.len() - 20)).font(theme::font(12.0)));
                        }
                    });
                    if let Some(n) = pick {
                        ed.undo_to(n);
                    }
                    if undo.clicked() {
                        ed.undo();
                    }
                });
            });
        });
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top() - 4.0, egui::Stroke::new(1.0, p.border));
        if ed.is_shot() {
            shot::banner(&mut ed, ui);
        } else {
            banner(&mut ed, ui, ctx);
        }

        // 截圖編輯下方有一排固定高度的按鈕（影片的摘要和儲存在標題列），其他給影片、時間軸與右側面板
        let foot_h = if ed.is_shot() { 64.0 } else { 8.0 };
        let body_h = (ui.available_height() - foot_h).max(200.0);
        let full_w = ui.available_width();
        let side_open = SIDE_OPEN.load(Ordering::Relaxed);
        ui.allocate_ui_with_layout(vec2(full_w, body_h), Layout::left_to_right(Align::Min), |ui| {
            ui.add_space(18.0);
            let side_w = if side_open { 300.0 } else { 0.0 };
            let main_w = if side_open { (full_w - side_w - 18.0 * 2.0 - 18.0).max(300.0) } else { (full_w - 18.0 * 2.0).max(300.0) };
            ui.allocate_ui_with_layout(vec2(main_w, body_h), Layout::top_down(Align::Min), |ui| {
                ui.set_width(main_w);
                ui.set_height(body_h);
                main_column(&mut ed, ui, ctx, &mut toast);
            });
            if side_open {
                ui.add_space(8.0);
                let r = ui.cursor();
                ui.painter().vline(r.left(), r.top()..=r.top() + body_h - 8.0, egui::Stroke::new(1.0, p.border));
                ui.add_space(12.0);
                let side = ui.allocate_ui_with_layout(vec2(side_w, body_h), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(side_w);
                    ui.set_height(body_h - 8.0);
                    side::show(&mut ed, ui, ctx, &mut toast);
                });
                tour::mark(ui, if ed.is_shot() { Spot::ShSide } else { Spot::EdSide }, side.response.rect);
            }
        });

        // 截圖編輯：下方的儲存那一排
        if ed.is_shot() {
            let r = ui.cursor();
            ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, p.border));
            let foot = egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(a) = shot::footer(&mut ed, ui) {
                        ed.pending = Some(a);
                    }
                });
            });
            tour::mark(ui, Spot::ShFoot, foot.response.rect);
            // 提示放在下方「儲存」那一排的上方
            super::dialogs::toast_spot(ui.ctx(), foot.response.rect.right_top());
        } else {
            // 提示放在右下角
            let r = ui.max_rect();
            super::dialogs::toast_spot(ui.ctx(), pos2(r.right() - 12.0, r.bottom() - 12.0));
        }
    });

    // Ctrl+S：和按「另存剪輯版 / 儲存修改」一樣；還不能存時說明原因
    if std::mem::take(&mut ed.save_now) && !ed.is_shot() && !ed.saving {
        if ed.can_save() {
            save = true;
        } else {
            toast = Some((tr!("還沒有任何剪輯、裁切或標註，不用儲存", "There are no cuts, crop or annotations to save yet").into(), false));
        }
    }
    if std::mem::take(&mut ed.reset_toast) {
        toast = Some((tr!("已全部重設，可按 Ctrl+Z 復原", "Everything was reset. Press Ctrl+Z to undo").into(), false));
    }
    if save {
        start_save(app, &mut ed);
    }
    let busy = !matches!(ed.drag, Drag::None) || ctx.input(|i| i.pointer.any_down());
    let input = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::PointerButton { .. } | egui::Event::Paste(_) | egui::Event::Cut)));
    ed.track_history(busy, ctx.egui_wants_keyboard_input(), input);
    if let Some(act) = ed.vpending.take() {
        run_vact(app, &mut ed, act);
    }
    if let Some(act) = ed.pending.take() {
        if act == shot::Act::Plain {
            // 換成直接編輯開啟的那張：不放回這個視窗
            return shot::run(app, &mut ed, act);
        }
        shot::run(app, &mut ed, act);
    }
    if let Some((t, err)) = toast {
        app.toast(t, err);
    }
    if !ed.is_shot() {
        LAST_TAB.store(ed.tab as u8, Ordering::Relaxed);
    }
    ed.typing = ctx.egui_wants_keyboard_input();
    if std::mem::take(&mut ed.discard_draft) {
        clear_draft(app, &ed);
    }
    autosave_draft(app, &mut ed);
    if std::mem::take(&mut ed.close) {
        if !ed.dirty() || ed.saving {
            // 這次存的草稿和儲存過的一樣（例如改了又復原）：不用再提示接著剪
            if ed.draft_written && !ed.saving {
                clear_draft(app, &ed);
            }
            // 關閉時釋放影片檔（Player 結束時停止 FFmpeg）
            ed.strip.cancel();
            return;
        }
        ed.pause();
        app.ask = Some(close_dialog(&ed));
    }
    app.editor = Some(ed);
}

/// 關閉時還有沒儲存的修改：「儲存後關閉」（主要）／「不儲存」／「取消」；存不了（例如全部改回原樣）時只問要不要放棄
fn close_dialog(ed: &Editor) -> super::dialogs::Ask {
    use super::dialogs::Ask;
    let shot = ed.is_shot();
    let (title, msg) = if shot {
        (
            tr!("要儲存這次的編輯嗎？", "Save your changes?"),
            tr!("標註、裁切或輸出設定還沒有儲存，不儲存就關閉的話會不見。", "Your annotations, crop and output settings haven't been saved. They'll be lost if you close without saving."),
        )
    } else {
        (
            tr!("要儲存這次的剪輯嗎？", "Save your edits?"),
            tr!("剪輯、裁切或標註還沒有儲存，不儲存就關閉的話會不見。", "Your cuts, crop and annotations haven't been saved. They'll be lost if you close without saving."),
        )
    };
    let discard = |app: &mut UiApp, _: String| {
        app.ask = None;
        if let Some(e) = app.editor.take() {
            // 自己選了不儲存：草稿也不留
            if !e.is_shot() {
                clear_draft(app, &e);
            }
            e.strip.cancel();
        }
    };
    if !ed.can_save() {
        let mut ask = Ask::confirm(
            if shot { tr!("放棄這次的編輯？", "Discard your changes?") } else { tr!("放棄這次的剪輯？", "Discard your edits?") },
            tr!("目前的內容和原本一樣，沒有可以儲存的修改。", "Nothing has changed from the original, so there's nothing to save."),
            tr!("不儲存", "Don't save"),
            discard,
        );
        ask.danger = true;
        return ask;
    }
    let save = if shot {
        tr!("儲存後關閉", "Save and close")
    } else if ed.replace_target.is_some() {
        tr!("儲存修改後關閉", "Save changes and close")
    } else {
        tr!("另存剪輯版後關閉", "Save edited version and close")
    };
    Ask::confirm(title, msg, save, |app, _| {
        app.ask = None;
        // 儲存（影片：開始輸出；截圖：存檔）成功後會關閉剪輯視窗，失敗時留著並顯示原因
        if let Some(mut e) = app.editor.take() {
            if e.is_shot() {
                shot::run(app, &mut e, shot::Act::Save);
            } else {
                start_save(app, &mut e);
            }
            if app.editor.is_none() {
                app.editor = Some(e);
            }
        }
    })
    .with_alt(tr!("不儲存", "Don't save"), discard)
}

/// 影片的動作（需要 app）
fn run_vact(app: &mut UiApp, ed: &mut Editor, act: VAct) {
    match act {
        VAct::ExportSrt => export_srt(app, ed),
        VAct::ExportTranscript(word) => export_transcript(app, ed, word),
        VAct::PickOutput => pick_output(app, ed),
        VAct::RenameMarker(i) => {
            let Some(m) = ed.markers.get(i) else { return };
            let t = m.t;
            let name = ed.marker_name(i);
            app.ask = Some(super::dialogs::Ask::input(
                trf!("標記重新命名（{}）", "Rename marker ({})", video_clock(t)),
                tr!("名稱（輸出時當作章節名稱，空白 = 標記 N）", "Name (used as the chapter title; leave empty for “Marker N”)"),
                name,
                tr!("確定", "OK"),
                move |app, v| {
                    app.ask = None;
                    if let Some(m) = app.editor.as_mut().and_then(|e| e.markers.iter_mut().find(|m| m.t == t)) {
                        m.name = v.trim().to_string();
                    }
                },
            ));
        }
    }
}

/// 選擇剪輯版的儲存位置（另存新檔視窗，預設是目前的位置與檔名）；選了原本要取代的剪輯版以外的地方就改成另存新檔
fn pick_output(app: &mut UiApp, ed: &mut Editor) {
    let target = ed.output_target();
    let p = std::path::Path::new(&target);
    let (dir, name) = (p.parent().map(std::path::Path::to_path_buf), file_name(&target));
    let video = ed.entry.media.path.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || screenrecorder_core::filepick::save_file(tr!("另存剪輯版", "Save edited version"), dir, &name, &[(tr!("MP4 影片", "MP4 video"), "*.mp4")]))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| {
            let Some(ed) = app.editor.as_mut().filter(|e| e.entry.media.path == video) else { return };
            match r {
                Ok(Some((mut path, _))) => {
                    if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("mp4")) {
                        path.set_extension("mp4");
                    }
                    let path = path.to_string_lossy().to_string();
                    if path.eq_ignore_ascii_case(&video) {
                        return app.toast(tr!("不能覆蓋正在剪輯的影片，請換一個檔名", "Can't overwrite the video being edited. Choose another name"), true);
                    }
                    if ed.replace_target.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(&path)) {
                        ed.out_path = None;
                    } else {
                        ed.replace_target = None;
                        if matches!(ed.banner, BannerInfo::None) {
                            ed.banner_hidden = true;
                        }
                        ed.out_path = Some(path);
                    }
                }
                Ok(None) => {}
                Err(e) => app.toast(e, true),
            }
        },
    );
}

/// 字幕匯出成 SRT 或 VTT（剪輯後的時間），用另存新檔視窗選位置與類型
fn export_srt(app: &mut UiApp, ed: &Editor) {
    let cues = ed.cut_cues();
    if cues.is_empty() {
        return app.toast(tr!("剪輯後沒有留下任何字幕（都在刪除的片段裡）", "No subtitles are left after editing (they're all in removed sections)"), true);
    }
    let dir = std::path::Path::new(&ed.entry.media.path).parent().map(std::path::Path::to_path_buf);
    let name = format!("{}.srt", screenrecorder_core::format::strip_mp4(&cut_file_name(&ed.entry.media.name)));
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<Option<PathBuf>, String> {
                let filters = [(tr!("SRT 字幕檔（YouTube）", "SRT subtitles (YouTube)"), "*.srt"), (tr!("WebVTT 字幕檔（網頁、Teams）", "WebVTT subtitles (web, Teams)"), "*.vtt")];
                let picked = screenrecorder_core::filepick::save_file(tr!("匯出字幕檔", "Export subtitles"), dir, &name, &filters)?;
                let Some((mut path, kind)) = picked else { return Ok(None) };
                // 打的副檔名優先，沒打就看選的類型
                let typed = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
                let vtt = match typed.as_deref() {
                    Some("vtt") => true,
                    Some("srt") => false,
                    _ => kind == 1,
                };
                if typed.as_deref() != Some(if vtt { "vtt" } else { "srt" }) {
                    path.set_extension(if vtt { "vtt" } else { "srt" });
                }
                let body = if vtt { screenrecorder_core::subtitles::to_vtt(&cues) } else { screenrecorder_core::subtitles::to_srt(&cues) };
                // SRT 加上 BOM：Windows 的記事本與舊的播放器才不會變成亂碼（VTT 規定不能有 BOM 以外的東西在前面，BOM 可以省略）
                let body = if vtt { body } else { format!("\u{feff}{body}") };
                std::fs::write(&path, body).map_err(|e| trf!("無法儲存字幕檔：{e}", "Couldn't save the subtitle file: {e}"))?;
                Ok(Some(path))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| match r {
            Ok(Some(p)) => {
                let p = p.to_string_lossy().to_string();
                app.toast_file(trf!("已匯出字幕檔 {}", "Exported {}", file_name(&p)), &p);
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

/// 字幕整理成逐字稿（剪輯後的時間），存成 Word 或 TXT
fn export_transcript(app: &mut UiApp, ed: &Editor, word: bool) {
    let cues = ed.cut_cues();
    let paras = screenrecorder_core::transcript::paragraphs(&cues, ed.transcript_clean);
    if paras.is_empty() {
        return app.toast(tr!("剪輯後沒有留下任何字幕（都在刪除的片段裡）", "No subtitles are left after editing (they're all in removed sections)"), true);
    }
    let with_time = ed.transcript_time;
    let stem = screenrecorder_core::format::strip_mp4(&ed.entry.media.name);
    let title = if screenrecorder_core::naming::is_auto_name(&stem) { tr!("逐字稿", "Transcript").to_string() } else { stem.clone() };
    let date = ed.rec_start_ms().map(|ms| screenrecorder_core::look::stamp_preview(ms, 0.0)).unwrap_or_default();
    let dir = std::path::Path::new(&ed.entry.media.path).parent().map(std::path::Path::to_path_buf);
    let (ext, filter) = if word { ("docx", tr!("Word 文件", "Word document")) } else { ("txt", tr!("文字檔", "Text file")) };
    let name = format!("{stem}_{}.{ext}", tr!("逐字稿", "transcript"));
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<Option<PathBuf>, String> {
                let pattern = format!("*.{ext}");
                let picked = screenrecorder_core::filepick::save_file(tr!("匯出逐字稿", "Export transcript"), dir, &name, &[(filter, pattern.as_str())])?;
                let Some((mut path, _)) = picked else { return Ok(None) };
                if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case(ext)) {
                    path.set_extension(ext);
                }
                let bytes = if word {
                    screenrecorder_core::transcript::docx(&title, &date, &paras, with_time)
                } else {
                    // 加上 BOM：記事本才不會變成亂碼
                    format!("\u{feff}{}", screenrecorder_core::transcript::text(&title, &paras, with_time)).into_bytes()
                };
                std::fs::write(&path, bytes).map_err(|e| trf!("無法儲存逐字稿：{e}", "Couldn't save the transcript: {e}"))?;
                Ok(Some(path))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| match r {
            Ok(Some(p)) => {
                let p = p.to_string_lossy().to_string();
                app.toast_file(trf!("已匯出逐字稿 {}", "Exported {}", file_name(&p)), &p);
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

/// 剪輯中每隔一段時間存一份草稿（有改過、內容變了才存）：程式當掉或輸出失敗時可以接著剪。
/// 開啟時有上次的草稿、還沒決定要不要接著剪時不存（不蓋掉它）
fn autosave_draft(app: &mut UiApp, ed: &mut Editor) {
    if ed.is_shot() || ed.saving || ed.draft.is_some() || ed.draft_at.elapsed() < DRAFT_EVERY {
        return;
    }
    ed.draft_at = Instant::now();
    if !ed.has_unsaved() || ed.committed == ed.draft_saved {
        return;
    }
    ed.draft_saved = ed.committed.clone();
    ed.draft_written = true;
    let (core, video, data) = (app.core.clone(), ed.entry.media.path.clone(), ed.draft_value(false));
    app.spawn(async move { actions::save_edit_draft(&core, &video, data).await }, |_, r| {
        if let Err(e) = r {
            screenrecorder_core::info!("[剪輯] 無法存草稿：{}", e.message());
        }
    });
}

fn clear_draft(app: &UiApp, ed: &Editor) {
    let (core, video) = (app.core.clone(), ed.entry.media.path.clone());
    app.spawn(async move { actions::clear_edit_draft(&core, &video).await }, |_, _| {});
}

/// 把圖片檔拖曳到視窗：加上圖片（Logo）；拖曳中蓋上提示
fn drop_pictures(app: &mut UiApp, ed: &mut Editor, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()));
    if hovering {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("drop-picture")));
        let r = ctx.content_rect();
        painter.rect_filled(r, 0, Color32::from_black_alpha(140));
        painter.text(r.center(), egui::Align2::CENTER_CENTER, trf!("放開滑鼠，把圖片加到{}上", "Release to add the image to the {}", ed.what()), theme::font_bold(20.0), Color32::WHITE);
    }
    for path in dropped {
        if !picture::is_picture(&path) {
            *toast = Some((tr!("只能拖曳圖片檔進來（PNG、JPG、BMP、WebP）", "Only image files can be dropped here (PNG, JPG, BMP, WebP)").into(), true));
            continue;
        }
        let p = path.to_string_lossy().to_string();
        match ed.add_picture(&p) {
            Ok(()) => {
                if app.s.last_picture != p {
                    app.s.last_picture = p;
                    app.save_settings();
                }
            }
            Err(e) => *toast = Some((e, true)),
        }
    }
}

/// 說明列（各一行，放不下的字看提示）：上次沒有完成的剪輯（草稿）可以接著剪；
/// 正在修改哪個剪輯版、找不到原片、或原片有上次的剪輯可以載入（可以按 ✕ 收起來）
fn banner(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    if let Some(d) = &ed.draft {
        let exporting = d.data.get("exporting").and_then(Value::as_bool).unwrap_or(false);
        let when = local_time(d.saved_at);
        let text = if exporting {
            trf!("上次的剪輯沒有完成（輸出失敗或取消了），{when} 的剪輯與標註還留著。", "Your last edit didn't finish (it failed or was canceled). The cuts and annotations from {when} are kept.")
        } else {
            trf!("這支影片有上次沒有完成的剪輯（{when}）。", "This video has unfinished edits from {when}.")
        };
        let mut act = 0;
        banner_row(ui, true, &text, |ui| {
            if Btn::new(tr!("捨棄", "Discard")).ghost().small().tooltip(tr!("刪掉上次沒有完成的剪輯", "Delete the unfinished edits")).show(ui).clicked() {
                act = 2;
            }
            if Btn::new(tr!("繼續上次未完成的剪輯", "Continue the unfinished edit")).primary().small().show(ui).clicked() {
                act = 1;
            }
        });
        match act {
            1 => {
                let data = d.data.clone();
                ed.apply_draft(&data);
            }
            2 => {
                ed.draft = None;
                ed.discard_draft = true;
            }
            _ => {}
        }
    }
    if ed.banner_hidden {
        return;
    }
    let (text, full, warn): (String, String, bool) = if let Some(target) = &ed.replace_target {
        (
            trf!("正在修改剪輯版「{}」，儲存時會取代它。", "Changing the edited version “{}”. Saving replaces it.", file_name(target)),
            trf!(
                "正在修改剪輯版「{}」：已從原始影片「{}」載入上次的剪輯與標註，可以直接修改。儲存時會取代這個剪輯版。",
                "Changing the edited version “{}”: your previous edits and annotations were loaded from the original video “{}” and can be changed. Saving replaces this edited version.",
                file_name(target),
                ed.entry.media.name
            ),
            false,
        )
    } else {
        match &ed.banner {
            BannerInfo::None => return,
            BannerInfo::MissingSource(src) => {
                let t = trf!(
                    "找不到這個剪輯版的原始影片「{src}」，之前的標註已燒進影片、無法修改；只能在這個檔案上繼續剪輯。",
                    "Can't find “{src}”, the original video of this edited version. Earlier annotations are burned into the video and can't be changed; you can only keep editing this file."
                );
                (t.clone(), t, true)
            }
            BannerInfo::HasProject { output, .. } => {
                let t = trf!("這支影片之前剪輯成「{}」。", "This video was previously edited into “{}”.", file_name(output));
                (t.clone(), t, false)
            }
        }
    };
    let mut close = false;
    let mut load = None;
    let mut other = 0;
    banner_row(ui, warn, &text, |ui| {
        if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("收起說明", "Hide this message")).show(ui).clicked() {
            close = true;
        }
        if ed.replace_target.is_some() {
            // 不常用的做法收進選單（一行放得下）
            let more = Btn::new(tr!("其他做法", "Other options")).icon(Icon::ChevD).ghost().small().show(ui);
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(260.0);
                if ui.button(tr!("改為另存新的剪輯版", "Save as a new edited version instead")).clicked() {
                    other = 1;
                }
                if ed.opened_cut.is_some()
                    && ui
                        .button(tr!("改在這個剪輯版上繼續剪（舊標註已燒進影片，無法修改）", "Keep editing this edited version instead (earlier annotations are burned in and can't be changed)"))
                        .clicked()
                {
                    other = 2;
                }
            });
        } else if let BannerInfo::HasProject { output, data } = &ed.banner {
            if Btn::new(tr!("載入上次的剪輯來修改", "Load the previous edits to change them")).small().show(ui).clicked() {
                load = Some((output.clone(), data.clone()));
            }
        }
    })
    .on_hover_text(full);
    if close {
        ed.banner_hidden = true;
    }
    match other {
        1 => {
            ed.replace_target = None;
            ed.banner = BannerInfo::None;
        }
        2 => {
            if let Some(cut) = ed.opened_cut.take() {
                ed.replace_target = None;
                ed.banner = BannerInfo::None;
                ed.load(ctx, cut);
                ed.mark_saved();
            }
        }
        _ => {}
    }
    if let Some((output, data)) = load {
        ed.apply_project(&data);
        ed.replace_target = Some(output);
        ed.banner = BannerInfo::None;
        ed.mark_saved();
    }
}

/// 說明列的一行：文字放不下時截斷（滑鼠移上去看完整內容）；buttons 由右往左排
fn banner_row(ui: &mut egui::Ui, warn: bool, text: &str, buttons: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let p = theme::pal(ui);
    egui::Frame::new()
        .inner_margin(egui::Margin { left: 18, right: 18, top: 0, bottom: 4 })
        .show(ui, |ui| {
            egui::Frame::new()
                .fill(if warn { p.warn_soft } else { p.accent_soft })
                .corner_radius(theme::RADIUS_SM)
                .inner_margin(egui::Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.set_min_height(28.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            buttons(ui);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| ui.add(egui::Label::new(RichText::new(text).color(if warn { p.warn } else { p.text })).truncate())).inner
                        })
                        .inner
                    })
                    .inner
                })
                .inner
        })
        .inner
}

/// 時間（Unix 毫秒）→「10/13 14:05」（當地時間）
fn local_time(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms as i64).single().map(|t| t.format("%m/%d %H:%M").to_string()).unwrap_or_default()
}

/// 左邊：影片、時間軸、標註軌、播放控制
fn main_column(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    if ed.is_shot() {
        // 截圖：沒有時間軸，畫面用全部的高度
        let h = (ui.available_height() - 8.0).max(140.0);
        tour::mark(ui, Spot::ShCanvas, egui::Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), h)));
        return stage::show(ed, ui, ctx, h);
    }
    let below = timeline::RULER_H + timeline::TL_H + 4.0 + timeline::ann_track_h(ed) + 4.0 + timeline::SCROLL_H + 8.0 + 34.0 + 8.0;
    let stage_h = (ui.available_height() - below - 8.0).max(140.0);
    stage::show(ed, ui, ctx, stage_h);
    let top = ui.cursor().min.y;
    timeline::show(ed, ui, ctx, toast);
    tour::mark(ui, Spot::EdTimeline, egui::Rect::from_min_max(pos2(ui.max_rect().min.x, top), pos2(ui.max_rect().max.x, ui.cursor().min.y)));
    transport(ed, ui, toast);
}

/// 時間軸下方一排：播放、預覽結果｜往前往後｜分割、標記、擷取、顯示刪掉的部分｜時間、快速鍵。
/// 每個功能都一按就到；放不下時有文字的按鈕改成只有圖示（滑鼠移上去看名稱與快速鍵）
fn transport(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    ui.horizontal(|ui| {
        ui.set_height(34.0);
        let playing = ed.player.is_playing() && !ed.previewing;
        let play_tip = if ed.show_removed {
            tr!("播放 / 暫停（空白鍵）", "Play / pause (Space)")
        } else {
            tr!("播放 / 暫停（空白鍵）：跳過刪掉的片段", "Play / pause (Space): removed parts are skipped")
        };
        if Btn::new(if playing { tr!("暫停", "Pause") } else { tr!("播放", "Play") })
            .icon(if playing { theme::Icon::Pause } else { theme::Icon::Play })
            .min_width(84.0)
            .tooltip(play_tip)
            .show(ui)
            .clicked()
        {
            ed.toggle_play();
        }
        if Btn::new(if ed.previewing { tr!("停止預覽", "Stop preview") } else { tr!("預覽結果", "Preview result") })
            .tooltip(tr!(
                "播放剪輯後的樣子：跳過刪掉的片段，並顯示裁切、跟著點擊放大、直式影片、背景與圓角",
                "Play the edited result: removed parts are skipped, and the crop, click zoom, vertical video and background are shown"
            ))
            .show(ui)
            .clicked()
        {
            if ed.previewing {
                ed.stop_preview();
            } else {
                ed.start_preview();
            }
        }
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.x = 2.0;
        let step = |ui: &mut egui::Ui, b: Btn| b.ghost().small().show(ui).clicked();
        if step(ui, Btn::new(tr!("−1 秒", "−1 s")).tooltip(tr!("往前 1 秒（Shift+←）", "Back 1 second (Shift+←)"))) {
            ed.step(None, -1.0);
        }
        if step(ui, Btn::icon_only(theme::Icon::ChevL).tooltip(tr!("上一格（←）", "Previous frame (←)"))) {
            ed.step(Some(-1), 0.0);
        }
        if step(ui, Btn::icon_only(theme::Icon::ChevR).tooltip(tr!("下一格（→）", "Next frame (→)"))) {
            ed.step(Some(1), 0.0);
        }
        if step(ui, Btn::new(tr!("+1 秒", "+1 s")).tooltip(tr!("往後 1 秒（Shift+→）", "Forward 1 second (Shift+→)"))) {
            ed.step(None, 1.0);
        }
        ui.add_space(8.0);
        // 右邊留給時間、快速鍵（放大時還有「顯示全部」）；放不下時分割、標記只顯示圖示
        let clock_w = ui.painter().layout_no_wrap("00:00.0 / 00:00.0".into(), theme::mono(14.0), Color32::WHITE).size().x + 12.0;
        let icon_w = Btn::icon_only(theme::Icon::Camera).ghost().small().width(ui) + 2.0;
        let all_w = if ed.zoomed() { Btn::new(tr!("顯示全部", "Show all")).ghost().small().width(ui) + 2.0 } else { 0.0 };
        let split_full = Btn::new(tr!("分割（S）", "Split (S)")).icon(theme::Icon::Cut).ghost().small();
        let mark_full = Btn::new(tr!("＋ 標記（B）", "+ Marker (B)")).ghost().small();
        let room = ui.available_width() - clock_w - icon_w - all_w - icon_w * 2.0;
        let text = room >= split_full.width(ui) + mark_full.width(ui) + 4.0;
        let split = if text { split_full } else { Btn::icon_only(theme::Icon::Cut).ghost().small() };
        if split
            .tooltip(tr!(
                "在這裡分割（S）：在播放頭把影片切開，之後按兩下一段就能選取它（刪除、改速度）",
                "Split here (S): cut the video at the playhead, then double-click a section to select it (delete or change its speed)"
            ))
            .show(ui)
            .clicked()
        {
            *toast = Some(ed.split_here());
        }
        let mark = if text { mark_full } else { Btn::icon_only(theme::Icon::Pin).ghost().small() };
        if mark
            .enabled(ed.duration > 0.0)
            .tooltip(tr!(
                "在播放頭加上標記（B），輸出時變成章節（按兩下旗子重新命名、右鍵刪除；章節清單在「字幕」分頁）",
                "Add a marker at the playhead (B); it becomes a chapter in the output (double-click a flag to rename, right-click to delete; the chapter list is in the Subtitles tab)"
            ))
            .show(ui)
            .clicked()
        {
            *toast = Some(ed.add_marker());
        }
        if Btn::icon_only(theme::Icon::Camera)
            .ghost()
            .small()
            .tooltip(tr!("擷取這一格：把目前這一格存成截圖（原尺寸 PNG），並複製到剪貼簿", "Grab frame: save the current frame as a screenshot (full-size PNG) and copy it to the clipboard"))
            .show(ui)
            .clicked()
        {
            ed.pending = Some(shot::Act::Grab);
        }
        if Btn::icon_only(theme::Icon::Eye)
            .ghost()
            .small()
            .selected(ed.show_removed)
            .tooltip(if ed.show_removed {
                tr!(
                    "顯示刪掉的部分：開（時間軸展開刪掉的片段，播放時照原樣播）。按一下關掉",
                    "Show removed parts: on (the timeline shows removed parts and everything plays as recorded). Click to turn off"
                )
            } else {
                tr!(
                    "顯示刪掉的部分：關（時間軸上刪掉的片段收起來，播放時跳過、加速的片段快轉，看到的就是剪輯後的樣子）。按一下開啟",
                    "Show removed parts: off (removed parts are folded away on the timeline and skipped when playing; sped-up parts fast-forward). Click to turn on"
                )
            })
            .show(ui)
            .clicked()
        {
            ed.show_removed = !ed.show_removed;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let keys = Btn::icon_only(theme::Icon::Keyboard).ghost().small().tooltip(tr!("剪輯的快速鍵", "Editing shortcuts")).show(ui);
            timeline::shortcut_sheet(&keys);
            ui.label(RichText::new(format!("{} / {}", video_clock(ed.now()), video_clock(ed.duration))).font(theme::mono(14.0)));
            if ed.shuttle != 0 {
                let speed = screenrecorder_core::format::speed_label(ed.shuttle_speed());
                let text = if ed.shuttle > 0 { trf!("快轉 {speed}×", "Fast forward {speed}×") } else { trf!("倒轉 {speed}×", "Rewind {speed}×") };
                ui.label(RichText::new(text).font(theme::font_bold(12.5)).color(theme::pal(ui).accent));
            }
            if ed.zoomed()
                && Btn::new(tr!("顯示全部", "Show all"))
                    .ghost()
                    .small()
                    .tooltip(tr!("時間軸縮回整支影片（在時間軸上轉滾輪可以放大縮小）", "Zoom the timeline out to the whole video (scroll on the timeline to zoom)"))
                    .show(ui)
                    .clicked()
            {
                let d = ed.duration;
                ed.set_view(0.0, d);
            }
        });
    });
}

/// 輸出的摘要（標題列檔名後面那一行）：一段一段的字（粗體？、警告色？）
fn summary_parts(ed: &Editor) -> Vec<(String, bool, bool)> {
    let keep = ed.keep();
    let parts = ed.parts();
    let length = output_length(&parts) + ed.cards_secs();
    // 局部變速：和整支加速不一樣的片段
    let whole_rate = screenrecorder_core::edit::whole_speed(&ed.spec);
    let fast = parts.iter().filter(|p| !p.is_hold() && (p.rate - whole_rate).abs() > 1e-9).count();
    let holds = parts.iter().filter(|p| p.is_hold()).count();
    let voices = ed.spec.narration.clips.len();
    let crop = if ed.crop_on && ed.vertical != 2 { normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32) } else { None };
    let size = ed.output_size(crop).map(|(w, h)| format!("{w}×{h}")).unwrap_or_default();
    let gone = ed.gone_count(&keep);
    let en = screenrecorder_core::i18n::is_en();
    let s = |n: usize| if n == 1 { "" } else { "s" };
    let mut out: Vec<(String, bool, bool)> = vec![];
    let mut t = |x: String| out.push((x, false, false));
    t(tr!("輸出 ", "Output ").into());
    let mut v = vec![(video_clock(length), true, false)];
    let (orig, kept) = (video_clock(ed.duration), keep.len());
    let mut more: Vec<String> = vec![if en { format!(" (original {orig}) · {kept} section{} kept", s(kept)) } else { format!("（原 {orig}）・保留 {kept} 段") }];
    if fast > 0 {
        more.push(if en { format!(" · {fast} speed change{}", s(fast)) } else { format!("・變速 {fast} 段") });
    }
    if holds > 0 {
        more.push(if en { format!(" · {holds} freeze frame{}", s(holds)) } else { format!("・定格 {holds} 處") });
    }
    if voices > 0 {
        more.push(if en { format!(" · {voices} narration clip{}", s(voices)) } else { format!("・旁白 {voices} 段") });
    }
    if ed.click_sound_on() {
        more.push(tr!("・點擊音效", " · Click sounds").into());
    }
    if ed.is_reordered() {
        more.push(tr!("・改了片段順序", " · Sections reordered").into());
    }
    if whole_rate > 1.0 {
        more.push(trf!("・整支 {}×", " · Whole video {}×", screenrecorder_core::format::speed_label(whole_rate)));
    }
    let zr = ed.region_spans().len();
    if zr > 0 {
        more.push(if en { format!(" · {zr} manual zoom{}", s(zr)) } else { format!("・手動放大 {zr} 處") });
    }
    if ed.zoom_active() {
        more.push(tr!("・跟著點擊放大", " · Zoom on clicks").into());
    }
    if ed.cursor_on() {
        more.push(tr!("・平滑放大的游標", " · Smooth cursor").into());
    }
    if ed.vertical_spec().is_some() {
        more.push(tr!("・直式影片", " · Vertical video").into());
    }
    if ed.spec.frame.is_some() && ed.vertical == 0 {
        more.push(tr!("・背景與圓角", " · Background and rounded corners").into());
    }
    if ed.cards_secs() > 0.0 {
        more.push(tr!("・片頭片尾", " · Intro/outro").into());
    }
    if !ed.spec.look.is_default() {
        more.push(tr!("・調色", " · Color").into());
    }
    if ed.spec.stamp.is_some() {
        more.push(tr!("・日期時間", " · Date and time").into());
    }
    out.append(&mut v);
    for m in more {
        out.push((m, false, false));
    }
    if !size.is_empty() {
        out.push((tr!("・", " · ").into(), false, false));
        out.push((size, true, false));
    }
    // 字幕另外算
    let subs = ed.sub_ids.len().min(ed.anns.len());
    let n = ed.anns.len() - subs;
    if n > 0 {
        out.push((if en { format!(" · {n} annotation{}", s(n)) } else { format!("・標註 {n} 個") }, false, false));
    }
    if subs > 0 {
        out.push((if en { format!(" · {subs} subtitle{}", s(subs)) } else { format!("・字幕 {subs} 段") }, false, false));
    }
    if gone > 0 {
        let t = if !en {
            format!("（{gone} 個在刪除的片段中，不會出現）")
        } else if gone == 1 {
            " (1 is in a removed section and won't appear)".to_string()
        } else {
            format!(" ({gone} are in removed sections and won't appear)")
        };
        out.push((t, false, true));
    }
    out
}

/// 標題列右上角：全部重設、另存剪輯版（旁邊 ▾ 看儲存位置、變更）；回傳是否按了儲存
/// （右到左的排版裡呼叫：先放 ▾，再放儲存，最後全部重設）
fn save_buttons(ed: &mut Editor, ui: &mut egui::Ui) -> bool {
    let p = theme::pal(ui);
    let parts = ed.parts();
    let length = output_length(&parts) + ed.cards_secs();
    let unchanged = ed.unchanged();
    let mut save = false;
    // 實際存到哪裡：資料夾 › 檔名，可以按「變更儲存位置…」另外選
    let target = ed.output_target();
    let what = if ed.replace_target.is_some() { tr!("取代：", "Replaces: ") } else { tr!("儲存到：", "Saves to: ") };
    let place = format!("{what}{}", pretty_path(&target));
    let menu = Btn::icon_only(Icon::ChevD).small().tooltip(format!("{place}\n{}", tr!("按一下變更儲存位置", "Click to change where it's saved"))).show(ui);
    let mut pick = false;
    egui::Popup::menu(&menu).show(|ui| {
        ui.set_min_width(280.0);
        ui.label(RichText::new(&place).font(theme::font(12.0)).color(p.muted)).on_hover_text(&target);
        if ui.button(tr!("變更儲存位置…", "Change where it's saved…")).clicked() {
            pick = true;
        }
    });
    if pick {
        ed.vpending = Some(VAct::PickOutput);
    }
    let label = if ed.replace_target.is_some() { tr!("儲存修改", "Save changes") } else { tr!("另存剪輯版", "Save edited version") };
    let tip = if unchanged {
        tr!("還沒有任何剪輯、裁切或標註", "No cuts, crop or annotations yet").to_string()
    } else if ed.replace_target.is_some() {
        format!("{}\n{place}", tr!("儲存修改（Ctrl+S）", "Save changes (Ctrl+S)"))
    } else {
        format!("{}\n{place}", tr!("另存剪輯版（Ctrl+S）", "Save edited version (Ctrl+S)"))
    };
    if Btn::new(label).primary().enabled(!unchanged && length >= 0.1 && !ed.saving).tooltip(tip).show(ui).clicked() {
        save = true;
    }
    let can_reset = !unchanged || !ed.splits.is_empty();
    if Btn::new(tr!("全部重設", "Reset all"))
        .ghost()
        .enabled(can_reset)
        .tooltip(tr!(
            "剪輯、裁切、標註、字幕、加速、聲音、放大、游標、直式、背景都回到原本的樣子（可以按 Ctrl+Z 復原）",
            "Undo all cuts, crop, annotations, subtitles, speed-ups, audio, zoom, cursor, vertical and background settings (press Ctrl+Z to undo)"
        ))
        .show(ui)
        .clicked()
    {
        ed.reset();
        ed.reset_toast = true;
    }
    save
}

/// 路徑 → 「影片 › Timelapse › 操作示範_cut.mp4」（使用者資料夾底下從那裡算起，太深時前面省略）
fn pretty_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let rel = home.as_deref().and_then(|h| p.strip_prefix(h).ok());
    let mut parts: Vec<String> = rel
        .unwrap_or(p)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            std::path::Component::Prefix(x) => Some(x.as_os_str().to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    if rel.is_some() {
        // 使用者資料夾裡常用的資料夾用 Windows 顯示的名稱
        if let Some(first) = parts.first_mut() {
            let shown = match first.as_str() {
                "Videos" => tr!("影片", "Videos"),
                "Desktop" => tr!("桌面", "Desktop"),
                "Downloads" => tr!("下載", "Downloads"),
                "Pictures" => tr!("圖片", "Pictures"),
                "Music" => tr!("音樂", "Music"),
                _ => "",
            };
            if !shown.is_empty() {
                *first = shown.to_string();
            }
        }
    }
    if parts.len() > 3 {
        let tail = parts.split_off(parts.len() - 3);
        parts = std::iter::once("…".to_string()).chain(tail).collect();
    }
    parts.join(" › ")
}

fn start_save(app: &mut UiApp, ed: &mut Editor) {
    ed.saving = true;
    // 標註轉成圖（每個都要畫並壓成 PNG，字幕多時要好幾秒）放到背景做，畫面不會卡住
    let anns: Vec<Ann> = ed.ordered().into_iter().cloned().collect();
    let (vw, vh) = (ed.vw, ed.vh);
    let crop = if ed.crop_on && ed.vertical != 2 {
        normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32).map(|r| CropInput { x: r.x as f64, y: r.y as f64, width: r.width as f64, height: r.height as f64 })
    } else {
        None
    };
    let spec = EditSpec {
        start: ed.spec.start,
        end: ed.spec.end,
        removed: ed.spec.removed.clone(),
        crop,
        overlays: vec![],
        audio: ed.spec.audio,
        fast: ed.spec.fast.clone(),
        zoom: ed.zoom_active().then(|| ClickZoom { factor: ed.zoom, clicks: ed.zoom_clicks() }),
        frame: ed.spec.frame.clone().filter(|_| ed.vertical == 0),
        cursor: ed.cursor_on().then(|| screenrecorder_core::cursor::CursorFx { size: ed.cursor_size, track: ed.cursor.clone(), hide_idle: ed.cursor_hide }),
        vertical: ed.vertical_spec(),
        intro: ed.spec.intro.clone().filter(|c| !c.is_empty()),
        outro: ed.spec.outro.clone().filter(|c| !c.is_empty()),
        fade_cuts: ed.spec.fade_cuts && ed.spec.removed.iter().any(|r| r.1 > r.0),
        freezes: ed.spec.freezes.clone(),
        zoom_regions: ed.spec.zoom_regions.clone(),
        order: ed.spec.order.clone(),
        speed: ed.spec.speed,
        speed_audio: ed.spec.speed_audio,
        narration: screenrecorder_core::narration::Narration { clicks: if ed.click_sound_on() { ed.clicks.iter().map(|c| c[0]).collect() } else { vec![] }, ..ed.spec.narration.clone() },
        look: ed.spec.look.clamped(),
        stamp: ed.spec.stamp.and_then(|st| ed.rec_start_ms().map(|start_ms| screenrecorder_core::look::Stamp { start_ms, ..st })),
    };
    let project = Some(ed.project_value());
    // 開始輸出時先存草稿（標記為輸出中）：輸出失敗或取消時可以「回到剪輯」，完成時會刪掉
    let draft = ed.draft_value(true);
    let (core, source) = (app.core.clone(), ed.entry.media.path.clone());
    let target = actions::CutTarget { replace: ed.replace_target.clone(), output: ed.out_path.clone().filter(|_| ed.replace_target.is_none()), chapters: ed.chapters() };
    let replacing = target.replace.is_some();
    let mut spec = spec;
    app.spawn(
        async move {
            if let Err(e) = actions::save_edit_draft(&core, &source, draft).await {
                screenrecorder_core::info!("[剪輯] 無法存草稿：{}", e.message());
            }
            spec.overlays = tokio::task::spawn_blocking(move || anns.iter().filter_map(|a| annotate::to_overlay(a, vw, vh)).collect()).await.unwrap_or_default();
            actions::cut_start_with(&core, &source, &spec, target, project).await
        },
        move |app, r| match r {
            Ok(()) => {
                if let Some(e) = app.editor.take() {
                    e.strip.cancel();
                }
                app.dismissed_job = None;
                app.toast(
                    if replacing {
                        tr!("已開始更新剪輯版，進度顯示在右側", "Updating the edited version; progress is shown on the right")
                    } else {
                        tr!("已開始剪輯，進度顯示在右側", "Creating the edited version; progress is shown on the right")
                    },
                    false,
                );
            }
            Err(e) => {
                if let Some(ed) = &mut app.editor {
                    ed.saving = false;
                }
                app.toast(e.message().to_string(), true);
            }
        },
    );
}

/// 快速鍵（和播放畫面一樣）：空白 = 播放/暫停、←/→ = 5 秒（Shift = 1 秒）、, / . = 一張、B = 標記、M = 靜音、
/// I/O = 開頭/結尾、Delete = 刪除選取的片段或標註、Esc = 取消或關閉
fn keyboard(ed: &mut Editor, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    if ctx.egui_wants_keyboard_input() || ed.typing {
        // 正在輸入文字（egui 在這一格開始時就因 Esc 離開輸入欄）：Esc 只離開輸入欄
        return;
    }
    let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    // 復原 / 重做（兩種都可以）
    if key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || key(Modifiers::COMMAND, Key::Y) {
        ed.redo();
    }
    if key(Modifiers::COMMAND, Key::Z) {
        ed.undo();
    }
    if !ed.is_shot() && key(Modifiers::COMMAND, Key::S) {
        ed.save_now = true;
    }
    if ed.is_shot() {
        shot::keys(ed, ctx, toast);
    }
    shortcuts(ed, ctx, toast, key);
}

/// 其他快速鍵（截圖沒有時間軸：播放、移動、開頭結尾不適用）
fn shortcuts(ed: &mut Editor, ctx: &egui::Context, toast: &mut Option<(String, bool)>, key: impl Fn(Modifiers, Key) -> bool) {
    let _ = ctx;
    let video = !ed.is_shot();
    if video && key(Modifiers::NONE, Key::Space) {
        if ed.previewing {
            ed.stop_preview();
        } else {
            ed.toggle_play();
        }
    }
    if video && key(Modifiers::NONE, Key::J) {
        ed.shuttle_key(-1);
    }
    if video && key(Modifiers::NONE, Key::K) {
        ed.shuttle_key(0);
    }
    if video && key(Modifiers::NONE, Key::L) {
        ed.shuttle_key(1);
    }
    if video && key(Modifiers::SHIFT, Key::ArrowLeft) {
        ed.step(None, -1.0);
    }
    if video && key(Modifiers::SHIFT, Key::ArrowRight) {
        ed.step(None, 1.0);
    }
    if video && key(Modifiers::NONE, Key::ArrowLeft) {
        ed.step(None, -5.0);
    }
    if video && key(Modifiers::NONE, Key::ArrowRight) {
        ed.step(None, 5.0);
    }
    // , / .：一張一張看
    if video && key(Modifiers::NONE, Key::Comma) {
        ed.step(Some(-1), 0.0);
    }
    if video && key(Modifiers::NONE, Key::Period) {
        ed.step(Some(1), 0.0);
    }
    if video && key(Modifiers::NONE, Key::M) {
        *toast = Some(ed.toggle_mute());
    }
    // B：加標記；Shift+B / Ctrl+Shift+B：跳到下一個 / 上一個標記（先比對有修飾鍵的）
    if video {
        let now = ed.now();
        if key(Modifiers::COMMAND | Modifiers::SHIFT, Key::B) {
            match ed.markers.iter().rev().map(|m| m.t).find(|&t| t < now - 0.05) {
                Some(t) => ed.seek(t),
                None => *toast = Some((tr!("前面沒有標記了", "No earlier markers").into(), false)),
            }
        }
        if key(Modifiers::SHIFT, Key::B) {
            match ed.markers.iter().map(|m| m.t).find(|&t| t > now + 0.05) {
                Some(t) => ed.seek(t),
                None => *toast = Some((tr!("後面沒有標記了", "No more markers ahead").into(), false)),
            }
        }
        if key(Modifiers::NONE, Key::B) {
            *toast = Some(ed.add_marker());
        }
        // S：在播放頭分割
        if key(Modifiers::NONE, Key::S) {
            *toast = Some(ed.split_here());
        }
    }
    if video && key(Modifiers::NONE, Key::I) {
        let t = ed.now();
        ed.set_start(t);
    }
    if video && key(Modifiers::NONE, Key::O) {
        let t = ed.now();
        ed.set_end(t);
    }
    // Delete：刪掉最後選的（範圍與標註不會同時選取）；D 只刪範圍
    let del = key(Modifiers::NONE, Key::Delete) || key(Modifiers::NONE, Key::Backspace);
    let d = key(Modifiers::NONE, Key::D);
    if (del || d) && ed.sel.is_some() {
        ed.delete_selection(toast);
    } else if del {
        if let Some(a) = ed.selected() {
            let (id, name) = (a.id, annotate::label(a));
            ed.delete_ann(id);
            *toast = Some((trf!("已刪除{}，可按 Ctrl+Z 復原", "Deleted {}. Press Ctrl+Z to undo", name), false));
        }
    }
    // [ / ]：選取的標註轉 15 度（Shift：1 度）
    for (k, dir) in [(Key::OpenBracket, -1.0), (Key::CloseBracket, 1.0)] {
        let step = if key(Modifiers::SHIFT, k) {
            1.0
        } else if key(Modifiers::NONE, k) {
            15.0
        } else {
            continue;
        };
        if let Some(a) = ed.ann_sel.and_then(|id| ed.ann_mut(id)).filter(|a| a.kind.rotatable()) {
            a.rot = norm_deg(a.rot + dir * step);
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

/// 角度換到 -180（不含）～180
pub fn norm_deg(d: f64) -> f64 {
    let r = (d + 180.0).rem_euclid(360.0) - 180.0;
    if r == -180.0 {
        180.0
    } else {
        r
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
