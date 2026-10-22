//! 一步一步的教學：畫面變暗，只亮出這一步要按的地方，旁邊有說明框與「第 2／7 步」。
//! - 第一次安裝：帶著錄第一部影片（選範圍 → 聲音 → 開始錄影 → 錄完在最近錄影播放、剪輯 → 截圖）；
//!   要使用者真的做的步驟（選範圍、開始錄影、停止）做完會自動到下一步，也可以跳過這一步
//! - 剪輯視窗、截圖編輯、全部截圖第一次打開時：幾步短提示（新安裝或按了「重新看教學」才有）
//!
//! 要亮出來的元件畫出來時用 mark() 記下位置（這一格的），教學照著畫框；
//! 找不到位置（例如視窗太窄收起來了）時說明框放在畫面中間。
//! 教學不擋住操作：變暗的地方也點得到；開了其他視窗時先收起來，關掉後接著顯示。

use super::settings::SourceType;
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, LayerId, Layout, Order, Rect, RichText, Stroke, StrokeKind, Ui};
use screenrecorder_core::types::RecorderState;
use screenrecorder_core::{tr, trf};

/// 教學會亮出來的地方
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Spot {
    /// 主畫面：下載錄影元件、範圍分頁、聲音那一列、開始 / 停止錄影、最近錄影（第一張）、截圖那一列、設定
    Download,
    Source,
    Audio,
    Record,
    Recent,
    RecentCard,
    Shot,
    Settings,
    /// 剪輯視窗：影片與時間軸、右側分頁、下方儲存
    EdTimeline,
    EdSide,
    EdFoot,
    /// 截圖編輯：圖、右側工具、下方儲存與複製
    ShCanvas,
    ShSide,
    ShFoot,
    /// 全部截圖：搜尋框、用點的找、月曆
    LbSearch,
    LbBrowse,
    LbCalendar,
}

fn spot_id(spot: Spot) -> Id {
    Id::new(("tour-spot", spot))
}

/// 記下這一格某個元件的位置（只記看得到的部分）
pub fn mark(ui: &Ui, spot: Spot, rect: Rect) {
    let r = rect.intersect(ui.clip_rect());
    if r.is_positive() {
        let ctx = ui.ctx();
        let n = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(spot_id(spot), (n, r)));
    }
}

/// 這一格記下的位置（舊的不算：元件這一格沒畫出來）
fn spot_rect(ctx: &egui::Context, spot: Spot) -> Option<Rect> {
    let n = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(u64, Rect)>(spot_id(spot))).filter(|(at, _)| *at == n).map(|(_, r)| r)
}

/// 哪一套教學
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// 第一次錄影
    First,
    /// 剪輯視窗
    Editor,
    /// 截圖編輯
    Shot,
    /// 全部截圖
    Library,
}

impl Kind {
    /// 存在設定 tours_done 裡的名稱
    fn key(self) -> &'static str {
        match self {
            Kind::First => "first",
            Kind::Editor => "editor",
            Kind::Shot => "shot",
            Kind::Library => "library",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Welcome,
    Download,
    Source,
    Audio,
    Record,
    /// 錄影中（不算一步：和「開始錄影」同一個編號）
    Recording,
    Recent,
    Shot,
    Done,
    EdTimeline,
    EdSide,
    EdSave,
    ShTools,
    ShCanvas,
    ShSave,
    LbSearch,
    LbBrowse,
    LbCalendar,
}

/// 這套提示會用到的地方（有任何一個畫出來，就表示視窗開好了）
fn kind_spots(kind: Kind) -> &'static [Spot] {
    match kind {
        Kind::First => &[],
        Kind::Editor => &[Spot::EdTimeline, Spot::EdSide, Spot::EdFoot],
        Kind::Shot => &[Spot::ShCanvas, Spot::ShSide, Spot::ShFoot],
        Kind::Library => &[Spot::LbSearch, Spot::LbBrowse, Spot::LbCalendar],
    }
}

fn steps(kind: Kind) -> &'static [Step] {
    use Step::*;
    match kind {
        Kind::First => &[Welcome, Download, Source, Audio, Record, Recording, Recent, Shot, Done],
        Kind::Editor => &[EdTimeline, EdSide, EdSave],
        Kind::Shot => &[ShTools, ShCanvas, ShSave],
        Kind::Library => &[LbSearch, LbBrowse, LbCalendar],
    }
}

pub struct Tour {
    kind: Kind,
    idx: usize,
    /// 這一步開始時的錄影範圍（選了別的就到下一步）
    base_source: SourceType,
    /// 這次教學真的錄了一部（最近錄影那一步的說明不同）
    recorded: bool,
    /// 開始錄影那一步：看到已經在錄影了（倒數取消時回到開始錄影那一步）
    saw_recording: bool,
}

impl Tour {
    fn step(&self) -> Step {
        steps(self.kind)[self.idx]
    }
}

/// 開始一套教學（at：從第幾步開始，看到一半的教學接著看）
pub fn start(app: &mut UiApp, kind: Kind, at: usize) {
    let idx = at.min(steps(kind).len() - 1);
    app.tour = Some(Tour { kind, idx, base_source: app.s.source_type, recorded: false, saw_recording: false });
    // 停在不用做的步驟（例如錄影元件已經有了）：往下找
    if skip(app, steps(kind)[idx]) {
        next(app);
    } else {
        save_progress(app);
    }
}

/// 從設定的「關於」重新看：第一次錄影的教學，之後各功能第一次打開時的提示也再顯示
pub fn restart(app: &mut UiApp) {
    app.s.tours_done.clear();
    app.s.tips = true;
    app.settings_dlg = None;
    start(app, Kind::First, 0);
}

/// 測試用（SCREENRECORDER_DEV=tour:<first|editor|shot|library>[,<第幾步>]）
#[cfg(debug_assertions)]
pub fn dev(app: &mut UiApp, arg: &str) {
    let (k, at) = arg.split_once(',').unwrap_or((arg, "0"));
    let kind = match k {
        "editor" => Kind::Editor,
        "shot" => Kind::Shot,
        "library" => Kind::Library,
        _ => Kind::First,
    };
    start(app, kind, at.parse().unwrap_or(0));
}

/// 第一次安裝：直接開始；之前看到一半就關掉程式：問要不要接著看。回傳 true = 有處理（不顯示新功能介紹）
pub fn on_launch(app: &mut UiApp, first_install: bool) -> bool {
    if first_install {
        app.s.tips = true;
        start(app, Kind::First, 0);
        return true;
    }
    let at = app.s.tour_step as usize;
    if at == 0 || app.s.tours_done.iter().any(|k| k == Kind::First.key()) {
        return false;
    }
    let mut ask = super::dialogs::Ask::confirm(
        tr!("要接著看教學嗎？", "Continue the tutorial?"),
        tr!("上次的教學還沒看完，可以從停下來的地方接著看。", "You didn't finish the tutorial last time. You can pick up where you left off."),
        tr!("接著看", "Continue"),
        move |app, _| {
            app.ask = None;
            start(app, Kind::First, at.saturating_sub(1));
        },
    );
    // 不用了：不再問；取消（之後再說）：下次啟動再問
    ask.alt = Some((
        tr!("不用了", "No thanks").into(),
        Box::new(|app: &mut UiApp, _| {
            app.ask = None;
            finish(app, false);
        }),
    ));
    ask.cancel = Some(tr!("之後再說", "Later").into());
    app.ask = Some(ask);
    true
}

/// 這一步不用做（直接到下一步）
fn skip(app: &UiApp, step: Step) -> bool {
    match step {
        Step::Download => app.env.ffmpeg.found,
        // 錄影中只能從「開始錄影」進來
        Step::Recording => true,
        _ => false,
    }
}

fn next(app: &mut UiApp) {
    let Some(t) = app.tour.as_mut() else { return };
    let list = steps(t.kind);
    let mut i = t.idx + 1;
    while i < list.len() && skip(app, list[i]) {
        i += 1;
    }
    if i >= list.len() {
        return finish(app, true);
    }
    let src = app.s.source_type;
    if let Some(t) = app.tour.as_mut() {
        t.idx = i;
        t.base_source = src;
    }
    save_progress(app);
}

fn go_to(app: &mut UiApp, step: Step) {
    if let Some(t) = app.tour.as_mut() {
        if let Some(i) = steps(t.kind).iter().position(|s| *s == step) {
            t.idx = i;
        }
    }
    save_progress(app);
}

/// 第一次錄影的教學：記下看到第幾步（關掉程式後可以接著看）
fn save_progress(app: &mut UiApp) {
    let Some(t) = &app.tour else { return };
    if t.kind == Kind::First && app.s.tour_step != t.idx as u32 + 1 {
        app.s.tour_step = t.idx as u32 + 1;
        app.save_settings();
    }
}

/// 看完或跳過：記下來，之後不再自動顯示
fn finish(app: &mut UiApp, completed: bool) {
    let kind = app.tour.take().map(|t| t.kind).unwrap_or(Kind::First);
    if !app.s.tours_done.iter().any(|k| k == kind.key()) {
        app.s.tours_done.push(kind.key().to_string());
    }
    if kind == Kind::First {
        app.s.tour_step = 0;
        if !completed {
            app.toast(tr!("之後可以在「設定 → 關於」按「重新看教學」", "You can see it again from Settings → About → “Replay tutorial”"), false);
        }
    }
    app.save_settings();
}

/// 各功能第一次打開時的短提示：剪輯視窗、截圖編輯、全部截圖
fn auto_tips(app: &mut UiApp) {
    if !app.s.tips || app.tour.is_some() || app.ask.is_some() || app.settings_dlg.is_some() {
        return;
    }
    let kind = if let Some(e) = &app.editor {
        if e.is_shot() {
            Kind::Shot
        } else {
            Kind::Editor
        }
    } else if app.library.as_ref().is_some_and(|d| d.is_shot()) && app.viewer.is_none() {
        Kind::Library
    } else {
        return;
    };
    if !app.s.tours_done.iter().any(|k| k == kind.key()) {
        start(app, kind, 0);
    }
}

/// 這套教學現在要不要顯示（開了其他視窗時先收起來）；回傳 None = 教學的視窗已經關了（結束這套教學）
fn visible(app: &UiApp, kind: Kind) -> Option<bool> {
    if !app.visible {
        return Some(false);
    }
    let others = app.ask.is_some() || app.changelog_open || app.whats_new_open || app.snip.is_some() || app.ocr.is_some() || app.settings_dlg.is_some();
    match kind {
        // 倒數時不蓋住倒數的數字
        Kind::First => Some(
            !others
                && app.status.recorder.state != RecorderState::Countdown
                && app.library.is_none()
                && app.viewer.is_none()
                && app.editor.is_none()
                && app.settings_dlg.is_none()
                && app.export_dlg.is_none()
                && app.schedule_dlg.is_none()
                && app.steps_review.is_none(),
        ),
        Kind::Editor | Kind::Shot => {
            let e = app.editor.as_ref()?;
            (e.is_shot() == (kind == Kind::Shot)).then_some(!others)
        }
        Kind::Library => {
            let d = app.library.as_ref().filter(|d| d.is_shot())?;
            Some(!others && !d.covered() && app.viewer.is_none() && app.editor.is_none() && app.export_dlg.is_none())
        }
    }
}

/// 教學顯示中按 Esc：結束教學（要在各視窗檢查 Esc 之前，才不會連視窗一起關掉）
pub fn esc(app: &mut UiApp, ctx: &egui::Context) {
    let Some(kind) = app.tour.as_ref().map(|t| t.kind) else { return };
    if visible(app, kind) == Some(true) && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        finish(app, false);
    }
}

/// 依使用者做了什麼自動到下一步
fn advance(app: &mut UiApp) {
    let Some(t) = &app.tour else { return };
    let state = app.status.recorder.state;
    match t.step() {
        Step::Download if app.env.ffmpeg.found => next(app),
        Step::Source if app.s.source_type != t.base_source => next(app),
        Step::Record if state != RecorderState::Idle => {
            go_to(app, Step::Recording);
            if let Some(t) = app.tour.as_mut() {
                t.saw_recording = matches!(state, RecorderState::Recording | RecorderState::Paused | RecorderState::Stopping);
            }
        }
        Step::Recording => {
            let rec = matches!(state, RecorderState::Recording | RecorderState::Paused | RecorderState::Stopping);
            let saw = t.saw_recording;
            if rec {
                if let Some(t) = app.tour.as_mut() {
                    t.saw_recording = true;
                }
            } else if state == RecorderState::Idle {
                if saw {
                    if let Some(t) = app.tour.as_mut() {
                        t.recorded = true;
                    }
                    next(app);
                } else {
                    // 倒數時取消了：回到開始錄影
                    go_to(app, Step::Record);
                }
            }
        }
        _ => {}
    }
}

/// 第幾步／共幾步（錄影中和開始錄影同一步；不用做的步驟不算）
fn progress(app: &UiApp, t: &Tour) -> (usize, usize) {
    let list = steps(t.kind);
    let counted = |s: Step| s != Step::Recording && !(s == Step::Download && app.env.ffmpeg.found);
    let total = list.iter().filter(|s| counted(**s)).count();
    let cur = list[..=t.idx].iter().filter(|s| counted(**s)).count().max(1);
    (cur, total)
}

/// 快速鍵的寫法：「（Ctrl+Alt+R）」；停用時是空的
fn key(app: &UiApp, i: usize) -> String {
    let k = app.keys.label(i);
    if k.is_empty() {
        String::new()
    } else {
        trf!("（{k}）", " ({k})")
    }
}

/// 下一步怎麼走
enum Go {
    /// 「下一步」（或其他文字）
    Button(String),
    /// 做完會自動到下一步；也可以跳過這一步
    Wait,
    /// 歡迎：帶我走一遍 / 我自己來
    Welcome,
}

/// 這一步：亮出哪裡、標題、說明、怎麼到下一步
fn content(app: &UiApp, t: &Tour) -> (Option<Spot>, String, String, Go) {
    let next = || Go::Button(tr!("下一步", "Next").into());
    let audio_only = app.s.source_type == SourceType::Audio;
    match t.step() {
        Step::Welcome => (
            None,
            tr!("歡迎使用螢幕錄影", "Welcome to Screen Recorder").into(),
            if app.env.ffmpeg.found {
                tr!(
                    "花一分鐘帶你錄第一部影片：選範圍、確認聲音、開始錄影，錄完在哪裡播放、剪輯。每一步都會亮出要按的地方，隨時可以按「跳過教學」或 Esc 離開。",
                    "Take a minute to record your first video: choose an area, check the audio, start recording, then see where to play and edit it. Each step highlights what to click; you can leave any time with “Skip tutorial” or Esc."
                )
                .into()
            } else {
                tr!(
                    "花一分鐘帶你錄第一部影片：先下載錄影元件，再選範圍、確認聲音、開始錄影，錄完在哪裡播放、剪輯。每一步都會亮出要按的地方，隨時可以按「跳過教學」或 Esc 離開。",
                    "Take a minute to record your first video: download the recording component, choose an area, check the audio and start recording, then see where to play and edit it. Each step highlights what to click; you can leave any time with “Skip tutorial” or Esc."
                )
                .into()
            },
            Go::Welcome,
        ),
        Step::Download => (
            Some(Spot::Download),
            tr!("先下載錄影元件", "First, download the recording component").into(),
            tr!(
                "錄影、截圖都要用到免費的 FFmpeg 錄影元件。按「下載並開始使用」，只要下載一次（約 110 MB），完成後會自動到下一步。",
                "Recording and screenshots use the free FFmpeg component. Click “Download and get started”; it's a one-time download (about 110 MB) and the tutorial continues when it's done."
            )
            .into(),
            Go::Wait,
        ),
        Step::Source => (
            Some(Spot::Source),
            tr!("選要錄的範圍", "Choose what to record").into(),
            tr!(
                "「單一螢幕」錄一整個螢幕（預設），「所有螢幕」錄全部螢幕，「自訂範圍」可以框選一塊，「只錄聲音」不錄畫面。點一個試試看，或維持單一螢幕按「下一步」。",
                "“Single screen” records one whole screen (the default), “All screens” records every screen, “Custom area” lets you select part of it, and “Audio only” records sound without video. Click one to try it, or keep Single screen and click Next."
            )
            .into(),
            next(),
        ),
        Step::Audio => (
            Some(Spot::Audio),
            tr!("確認聲音", "Check the audio").into(),
            tr!(
                "預設會錄電腦播放的聲音和麥克風。按聲音旁邊的「測試音量」講幾句話，麥克風的條有跳動就表示收得到；要換麥克風或不錄聲音，點聲音設定修改。",
                "By default the computer's sound and your microphone are recorded. Click “Test levels” next to the audio setting and say a few words: if the microphone bar moves, you're being heard. To change microphones or record without sound, click the audio setting."
            )
            .into(),
            next(),
        ),
        Step::Record => (
            Some(Spot::Record),
            if audio_only { tr!("開始錄音", "Start recording audio") } else { tr!("開始錄影", "Start recording") }.into(),
            {
                let n = app.s.countdown_sec;
                let when = if n == 0 { tr!("馬上開始", "Recording starts right away").to_string() } else { trf!("倒數 {n} 秒後開始", "Recording starts after a {n}-second countdown") };
                trf!(
                    "按紅色的按鈕{}，{when}。錄幾秒就好，要停止時按螢幕上方控制列的「停止」，或再按一次快速鍵。開始後教學會自動接著走。",
                    "Click the red button{}. {when}. A few seconds is enough; to stop, click “Stop” on the bar at the top of the screen or press the shortcut again. The tutorial continues once you've started.",
                    key(app, 0)
                )
            },
            Go::Wait,
        ),
        Step::Recording => (
            Some(Spot::Record),
            tr!("錄影中", "Recording").into(),
            tr!("錄幾秒後按「停止」（或螢幕上方控制列的停止）。停止後就會出現在下方的「最近錄影」。", "After a few seconds, click “Stop” (or Stop on the bar at the top of the screen). It will then appear under “Recent recordings” below.")
                .into(),
            Go::Wait,
        ),
        Step::Recent => {
            let has = app.recent.as_ref().is_some_and(|r| !r.items.is_empty());
            let body = if t.recorded && has {
                tr!(
                    "剛錄好的在這裡。下面一排是：播放、剪輯（剪掉頭尾、加標註）、製作（加速版、GIF、壓縮）、傳到 Jellyfin、在資料夾中顯示；複製檔案、改名等其他功能在「⋯」，也可以按右鍵。",
                    "Your new recording is here. The buttons below are Play, Edit (trim, annotate), Make (sped-up video, GIF, compress), Send to library and Show in folder; copy file, rename and more are under ⋯ or a right-click."
                )
            } else {
                tr!(
                    "錄好的影片會出現在這裡。下面一排是：播放、剪輯、製作（加速版、GIF、壓縮）、傳到 Jellyfin、在資料夾中顯示；其他功能在「⋯」，也可以按右鍵。右邊「檔案庫 → 錄影」看以前錄的。",
                    "Your recordings appear here. The buttons below are Play, Edit, Make (sped-up video, GIF, compress), Send to library and Show in folder; everything else is under ⋯ or a right-click. “Library → Videos” on the right shows older ones."
                )
            };
            (Some(if has { Spot::RecentCard } else { Spot::Recent }), tr!("在「最近錄影」播放、剪輯", "Play and edit in “Recent recordings”").into(), body.into(), next())
        }
        Step::Shot => (
            Some(Spot::Shot),
            tr!("截圖", "Screenshots").into(),
            trf!(
                "「框選截圖」{}在螢幕上拖曳框選一塊（或點一下視窗），存成圖片並複製到剪貼簿，可以直接貼上。旁邊的箭頭還有延遲截圖、長截圖、步驟截圖等。截過的圖在右下角的「檔案庫 → 截圖」。",
                "“Select area”{} lets you drag over part of the screen (or click a window); it's saved as an image and copied to the clipboard, ready to paste. The arrow next to it has delayed, scrolling and step screenshots. Find your screenshots under “Library → Screenshots” at the bottom right.",
                key(app, 3)
            ),
            next(),
        ),
        Step::Done => (
            Some(Spot::Settings),
            tr!("完成了！", "You're all set!").into(),
            tr!(
                "畫質、每秒張數、存檔位置、快速鍵、語言都在「設定」；回溯錄影、排程錄影、講稿小視窗在標題列右邊的「⋯」。之後第一次打開剪輯、截圖編輯、全部截圖時，也會有幾步小提示。想再看一次這個教學：「設定 → 關於 → 重新看教學」。",
                "Quality, frame rate, where files are saved, shortcuts and language are in “Settings”; the replay buffer, scheduled recording and the script window are under “⋯” on the right of the title bar. The first time you open the editor, screenshot editing or All screenshots, you'll get a few quick tips too. To see this tutorial again: Settings → About → “Replay tutorial”."
            )
            .into(),
            Go::Button(tr!("開始使用", "Get started").into()),
        ),
        Step::EdTimeline => (
            Some(Spot::EdTimeline),
            tr!("時間軸：剪掉不要的部分", "Timeline: cut what you don't need").into(),
            tr!(
                "在縮圖上拖過不要的部分（開頭、中間、結尾都可以），按上方的「刪除」；兩端的黃色把手也能剪頭尾。滾輪放大時間軸看細一點，點一下跳到那個位置。",
                "Drag across the part you don't want on the thumbnails (start, middle or end) and press “Delete” above it; the yellow handles at either end trim too. Scroll to zoom the timeline and click to jump to a point."
            )
            .into(),
            next(),
        ),
        Step::EdSide => (
            Some(Spot::EdSide),
            tr!("右邊：裁切、標註、聲音", "On the right: crop, annotate, audio").into(),
            tr!(
                "右邊的分頁可以裁切畫面、加文字和箭頭、遮住個資、調整聲音（降噪、調大小聲）等。改錯了按 Ctrl+Z 復原。",
                "Use the tabs on the right to crop, add text and arrows, hide personal info, adjust audio (noise reduction, volume) and more. Made a mistake? Press Ctrl+Z to undo."
            )
            .into(),
            next(),
        ),
        Step::EdSave => (
            Some(Spot::EdFoot),
            tr!("存成新的影片", "Save as a new video").into(),
            tr!(
                "剪好後按這裡另存剪輯版，原本的影片不會被改掉；沒存就關掉也沒關係，下次打開會問要不要接著剪。",
                "When you're done, save it here as an edited copy; the original stays untouched. If you close without saving, you'll be asked whether to continue next time."
            )
            .into(),
            Go::Button(tr!("知道了", "Got it").into()),
        ),
        Step::ShTools => (
            Some(Spot::ShSide),
            tr!("標註工具", "Annotation tools").into(),
            tr!(
                "右邊選工具：箭頭、框線、文字、步驟編號、畫筆、馬賽克遮個資等，先選顏色和粗細再畫。",
                "Pick a tool on the right: arrows, boxes, text, step numbers, pen, blur to hide personal info and more. Choose a color and thickness first, then draw."
            )
            .into(),
            next(),
        ),
        Step::ShCanvas => (
            Some(Spot::ShCanvas),
            tr!("直接在圖上畫", "Draw right on the image").into(),
            tr!(
                "在圖上拖曳就畫上去；點一下畫好的標註可以移動、改顏色，按 Delete 刪掉。改錯了按 Ctrl+Z 復原。",
                "Drag on the image to draw. Click an annotation to move it or change its color, and press Delete to remove it. Made a mistake? Press Ctrl+Z to undo."
            )
            .into(),
            next(),
        ),
        Step::ShSave => (
            Some(Spot::ShFoot),
            tr!("複製或儲存", "Copy or save").into(),
            tr!(
                "畫好後按「複製」就能直接貼到 LINE、Email 或文件；也可以存回原圖或另存一張。",
                "When you're done, click “Copy” to paste it straight into a chat, email or document, or save it over the original or as a new image."
            )
            .into(),
            Go::Button(tr!("知道了", "Got it").into()),
        ),
        Step::LbSearch => (
            Some(Spot::LbSearch),
            tr!("用圖裡的字搜尋", "Search the text in your screenshots").into(),
            tr!(
                "打幾個字就能找到圖裡有這些字的截圖（例如「報價單」），也可以打日期（例如 10/11）。打字時下方會列出圖裡真的有的詞。",
                "Type a few words to find screenshots containing that text, or a date such as 10/11. Suggestions of words that really appear in your screenshots show up as you type."
            )
            .into(),
            next(),
        ),
        Step::LbBrowse => (
            Some(Spot::LbBrowse),
            tr!("不記得關鍵字？用點的找", "Can't remember a keyword? Browse").into(),
            tr!(
                "左邊依截圖時的程式、內容（錯誤訊息、網頁、對話…）、常出現的詞、時間與顏色分類，點幾下就縮小範圍，每個選項旁有張數。",
                "On the left, screenshots are grouped by the app you were using, content (errors, web pages, chats…), common words, date and color. Click a few to narrow down; each shows how many match."
            )
            .into(),
            next(),
        ),
        Step::LbCalendar => (
            Some(Spot::LbCalendar),
            tr!("月曆", "Calendar").into(),
            tr!("記得大概是哪一天截的：按「月曆」看每天截了幾張、那天在忙什麼，點一天就列出那天的截圖。", "Remember roughly which day it was? Open the calendar to see how many screenshots each day had and what they were about, then click a day to list them.")
                .into(),
            Go::Button(tr!("知道了", "Got it").into()),
        ),
    }
}

/// 每一格：檢查要不要開始提示、依操作到下一步，畫出變暗的畫面與說明框
pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    auto_tips(app);
    let Some(kind) = app.tour.as_ref().map(|t| t.kind) else { return };
    match visible(app, kind) {
        // 教學的視窗關掉了：這套提示算看過
        None => return finish(app, true),
        Some(false) => {
            // 看不到時仍要判斷錄影開始、停止（錄影時主視窗會縮小）
            advance(app);
            return;
        }
        Some(true) => {}
    }
    advance(app);
    // 開著選單（例如截圖的更多選項）：先不蓋住，選單才看得清楚
    if egui::Popup::is_any_open(ctx) {
        return;
    }
    let Some(t) = &app.tour else { return };
    let (spot, title, body, go) = content(app, t);
    let (cur, total) = progress(app, t);
    let rect = spot.and_then(|s| spot_rect(ctx, s));
    // 剪輯、截圖、全部截圖的提示：視窗還沒畫出來（例如還在開啟）時先不顯示；
    // 視窗有了但這一步的地方看不到（例如收起了用點的找）：說明框放中間
    if spot.is_some() && rect.is_none() && kind != Kind::First && !kind_spots(kind).iter().any(|s| spot_rect(ctx, *s).is_some()) {
        ctx.request_repaint();
        return;
    }
    let screen = ctx.content_rect();
    let hole = rect.map(|r| r.expand(6.0).intersect(screen));
    paint_dim(ctx, screen, hole);
    let mut act: Option<Act> = None;
    bubble(app, ctx, screen, hole, &title, &body, &go, (cur, total), kind, &mut act);
    match act {
        Some(Act::Next) => next(app),
        Some(Act::Skip) => finish(app, false),
        Some(Act::SkipStep) => {
            // 開始錄影那一步跳過：不錄，直接看最近錄影
            if app.tour.as_ref().is_some_and(|t| matches!(t.step(), Step::Record | Step::Recording)) {
                go_to(app, Step::Recording);
            }
            next(app);
        }
        None => {}
    }
    // 亮出的框有一點呼吸的效果
    ctx.request_repaint_after(std::time::Duration::from_millis(80));
}

enum Act {
    Next,
    SkipStep,
    Skip,
}

/// 把亮出來的地方以外變暗，並在周圍畫一圈藍色的框
fn paint_dim(ctx: &egui::Context, screen: Rect, hole: Option<Rect>) {
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("tour-dim")));
    let dim = Color32::from_black_alpha(150);
    let Some(h) = hole else {
        painter.rect_filled(screen, CornerRadius::ZERO, dim);
        return;
    };
    // 上、下、左、右四塊
    painter.rect_filled(Rect::from_min_max(screen.min, pos2(screen.max.x, h.min.y)), CornerRadius::ZERO, dim);
    painter.rect_filled(Rect::from_min_max(pos2(screen.min.x, h.max.y), screen.max), CornerRadius::ZERO, dim);
    painter.rect_filled(Rect::from_min_max(pos2(screen.min.x, h.min.y), pos2(h.min.x, h.max.y)), CornerRadius::ZERO, dim);
    painter.rect_filled(Rect::from_min_max(pos2(h.max.x, h.min.y), pos2(screen.max.x, h.max.y)), CornerRadius::ZERO, dim);
    let p = theme::pal_ctx(ctx);
    let t = ctx.input(|i| i.time) as f32;
    let pulse = (t * 3.0).sin() * 0.5 + 0.5;
    painter.rect_stroke(h, CornerRadius::same(8), Stroke::new(2.5, p.accent), StrokeKind::Outside);
    painter.rect_stroke(h.expand(2.0 + pulse * 4.0), CornerRadius::same(10), Stroke::new(2.0, p.accent.gamma_multiply(0.5 * (1.0 - pulse))), StrokeKind::Outside);
}

/// 說明框：放在亮出來的地方下方（放不下時上方、右邊、左邊），沒有時放中間
#[allow(clippy::too_many_arguments)]
fn bubble(app: &UiApp, ctx: &egui::Context, screen: Rect, hole: Option<Rect>, title: &str, body: &str, go: &Go, (cur, total): (usize, usize), kind: Kind, act: &mut Option<Act>) {
    let w = 360.0f32.min(screen.width() - 32.0);
    // 先用上一格量到的高度決定位置
    let size_id = Id::new("tour-bubble-size");
    let h = ctx.data(|d| d.get_temp::<f32>(size_id)).unwrap_or(190.0);
    let gap = 14.0;
    let pos = match hole {
        None => pos2(screen.center().x - w / 2.0, screen.center().y - h / 2.0),
        Some(r) => {
            let x = (r.center().x - w / 2.0).max(screen.min.x + 16.0).min(screen.max.x - w - 16.0);
            if r.max.y + gap + h <= screen.max.y - 8.0 {
                pos2(x, r.max.y + gap)
            } else if r.min.y - gap - h >= screen.min.y + 8.0 {
                pos2(x, r.min.y - gap - h)
            } else {
                let y = (r.center().y - h / 2.0).max(screen.min.y + 16.0).min(screen.max.y - h - 16.0);
                if r.max.x + gap + w <= screen.max.x - 8.0 {
                    pos2(r.max.x + gap, y)
                } else if r.min.x - gap - w >= screen.min.x + 8.0 {
                    pos2(r.min.x - gap - w, y)
                } else {
                    // 亮出來的地方很大（例如整個時間軸）：放在它裡面靠下
                    pos2(x, (r.max.y - h - 16.0).max(screen.min.y + 16.0))
                }
            }
        }
    };
    let p = theme::pal_ctx(ctx);
    let first = kind == Kind::First;
    let resp = egui::Area::new(Id::new("tour-bubble")).order(Order::Debug).fixed_pos(pos).constrain(true).show(ctx, |ui| {
        egui::Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.accent.gamma_multiply(0.6)))
            .corner_radius(CornerRadius::same(12))
            .shadow(egui::Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(90) })
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(w - 32.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    let label = if total > 1 { trf!("第 {cur}／{total} 步", "Step {cur} of {total}") } else { String::new() };
                    ui.label(RichText::new(label).font(theme::font(12.0)).color(p.accent));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let tip = if first { tr!("跳過教學（Esc）", "Skip tutorial (Esc)") } else { tr!("關閉提示（Esc）", "Close tips (Esc)") };
                        if Btn::icon_only(Icon::Close).ghost().small().tooltip(tip).show(ui).clicked() {
                            *act = Some(Act::Skip);
                        }
                    });
                });
                ui.label(RichText::new(title).font(theme::font_bold(16.0)).color(p.text));
                ui.add(egui::Label::new(RichText::new(body).font(theme::font(13.5)).color(p.text)).wrap());
                if total > 1 {
                    dots(ui, cur, total);
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| match go {
                    Go::Welcome => {
                        if Btn::new(tr!("我自己來", "I'll explore on my own")).ghost().show(ui).clicked() {
                            *act = Some(Act::Skip);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if Btn::new(tr!("帶我走一遍", "Show me around")).primary().show(ui).clicked() {
                                *act = Some(Act::Next);
                            }
                        });
                    }
                    Go::Button(label) => {
                        // 最後一步不用「跳過」
                        if first && cur < total && Btn::new(tr!("跳過教學", "Skip tutorial")).ghost().small().show(ui).clicked() {
                            *act = Some(Act::Skip);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if Btn::new(label.as_str()).primary().show(ui).clicked() {
                                *act = Some(Act::Next);
                            }
                        });
                    }
                    Go::Wait => {
                        ui.label(RichText::new(tr!("做完會自動到下一步", "Continues automatically when done")).font(theme::font(12.0)).color(p.muted));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // 下載錄影元件不能跳過（沒有它不能錄影、截圖）
                            let label = if app.tour.as_ref().is_some_and(|t| matches!(t.step(), Step::Record | Step::Recording)) {
                                tr!("先不錄，下一步", "Skip recording for now")
                            } else {
                                tr!("跳過這一步", "Skip this step")
                            };
                            if !app.tour.as_ref().is_some_and(|t| t.step() == Step::Download) && Btn::new(label).small().show(ui).clicked() {
                                *act = Some(Act::SkipStep);
                            }
                        });
                    }
                });
            });
    });
    let h_now = resp.response.rect.height();
    if (h_now - h).abs() > 0.5 {
        ctx.data_mut(|d| d.insert_temp(size_id, h_now));
        ctx.request_repaint();
    }
    // 小箭頭指向亮出來的地方（說明框在上方或下方時）
    if let Some(r) = hole {
        let b = resp.response.rect;
        let painter = ctx.layer_painter(LayerId::new(Order::Debug, Id::new("tour-arrow")));
        let x = r.center().x.max(b.min.x + 20.0).min(b.max.x - 20.0);
        if b.min.y >= r.max.y {
            painter.add(egui::Shape::convex_polygon(vec![pos2(x, b.min.y - 8.0), pos2(x + 9.0, b.min.y + 1.0), pos2(x - 9.0, b.min.y + 1.0)], p.surface, Stroke::NONE));
        } else if b.max.y <= r.min.y {
            painter.add(egui::Shape::convex_polygon(vec![pos2(x - 9.0, b.max.y - 1.0), pos2(x + 9.0, b.max.y - 1.0), pos2(x, b.max.y + 8.0)], p.surface, Stroke::NONE));
        }
    }
}

/// 進度的小圓點
fn dots(ui: &mut Ui, cur: usize, total: usize) {
    let p = theme::pal(ui);
    let (r, _) = ui.allocate_exact_size(vec2(total as f32 * 14.0, 8.0), egui::Sense::hover());
    for i in 0..total {
        let c = pos2(r.min.x + 4.0 + i as f32 * 14.0, r.center().y);
        let color = if i + 1 == cur {
            p.accent
        } else if i + 1 < cur {
            p.accent.gamma_multiply(0.45)
        } else {
            p.border_strong
        };
        ui.painter().circle_filled(c, if i + 1 == cur { 4.0 } else { 3.0 }, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tour_has_steps_and_unique_keys() {
        let kinds = [Kind::First, Kind::Editor, Kind::Shot, Kind::Library];
        for k in kinds {
            assert!(!steps(k).is_empty());
        }
        let mut keys: Vec<_> = kinds.iter().map(|k| k.key()).collect();
        keys.dedup();
        assert_eq!(keys.len(), kinds.len());
        // 第一次錄影：開始錄影之後才是錄影中
        let f = steps(Kind::First);
        let rec = f.iter().position(|s| *s == Step::Record).unwrap();
        assert_eq!(f[rec + 1], Step::Recording);
    }
}
