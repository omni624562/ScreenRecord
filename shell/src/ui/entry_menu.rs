//! 一支錄影（影片、聲音檔）能做的事：主畫面卡片、全部錄影、檢視器共用同一份清單，
//! 名稱、圖示、分組、順序都一樣。各處只差在直接露出幾個（播放、剪輯、製作、傳到媒體庫、資料夾），
//! 其他在「⋯」裡、順序不變，所以在哪裡學會的，到別的地方也找得到

use super::theme::{Btn, Icon};
use super::{EntryAction, UiApp};
use eframe::egui;
use screenrecorder_core::tr;
use screenrecorder_core::types::LibraryEntry;

/// 一個動作
#[derive(Clone)]
pub struct Item {
    pub action: EntryAction,
    pub label: String,
    pub icon: Option<Icon>,
    pub tip: String,
    /// 現在不能用的原因（轉檔進行中等）
    pub blocked: Option<String>,
}

/// 清單裡的一格：一個動作，或「製作 ▾」「旋轉 ▾」這種子選單
#[derive(Clone)]
pub enum Node {
    One(Item),
    Sub { label: String, icon: Icon, tip: String, items: Vec<Item> },
}

impl Node {
    pub fn label(&self) -> &str {
        match self {
            Node::One(i) => &i.label,
            Node::Sub { label, .. } => label,
        }
    }

    pub fn icon(&self) -> Option<Icon> {
        match self {
            Node::One(i) => i.icon,
            Node::Sub { icon, .. } => Some(*icon),
        }
    }

    pub fn tip(&self) -> &str {
        match self {
            Node::One(i) => &i.tip,
            Node::Sub { tip, .. } => tip,
        }
    }

    /// 卡片、清單的每一列直接露出的那幾個：播放、剪輯、製作、傳到媒體庫、在資料夾中顯示（常用的都要一按就到）
    pub fn quick(&self) -> bool {
        match self {
            Node::One(i) => matches!(i.action, EntryAction::Play | EntryAction::PlayHere | EntryAction::Edit | EntryAction::SendToLibrary | EntryAction::Reveal),
            Node::Sub { items, .. } => items.iter().any(|i| i.action == EntryAction::Export),
        }
    }
}

/// 在哪裡顯示：主畫面的「播放」在主畫面播；檢視器裡不用「播放」，資料夾、刪除固定在右上角
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Home,
    Library,
    Viewer,
}

fn item(action: EntryAction, label: &str, icon: Option<Icon>, tip: &str) -> Item {
    Item { action, label: label.to_string(), icon, tip: tip.to_string(), blocked: None }
}

/// 一支錄影的全部動作，分成「看、改、做、分享、管理」五組（空的組不列）
pub fn groups(app: &UiApp, e: &LibraryEntry, place: Place) -> Vec<Vec<Node>> {
    let path = &e.media.path;
    // 聲音檔（M4A）只能播放、分享、管理；加速版、GIF 這些另存的版本不能再剪輯、製作，名稱跟著原片改
    let m4a = e.media.name.to_lowercase().ends_with(".m4a");
    let export = super::viewer::is_export_name(&e.media.name);
    let audio_rec = screenrecorder_core::after_record::is_audio_recording(path);
    let gate = |mut i: Item| {
        i.blocked = app.blocked(i.action);
        i
    };
    let one = |i: Item| Node::One(gate(i));

    let mut look = vec![];
    match place {
        Place::Home => {
            look.push(one(item(
                EntryAction::PlayHere,
                tr!("播放", "Play"),
                Some(Icon::Play),
                tr!("在這裡播放（點兩下卡片：在播放器開啟）", "Play here (double-click the card to open in the player)"),
            )));
            // 「⋯」裡也列出來（只靠點兩下卡片，第一次用的人找不到）
            look.push(one(item(
                EntryAction::Play,
                tr!("在播放器開啟", "Open in player"),
                Some(Icon::Fullscreen),
                tr!("用大的播放器看：拖曳進度、調速度、擷取這一格（也可以點兩下卡片）", "Watch in the full player: seek, change speed, grab a frame (or double-click the card)"),
            )));
        }
        Place::Library => look.push(one(item(EntryAction::Play, tr!("播放", "Play"), Some(Icon::Play), tr!("播放（Enter）", "Play (Enter)")))),
        Place::Viewer => {}
    }

    let mut change = vec![];
    if !m4a && !export {
        change.push(one(item(EntryAction::Edit, tr!("剪輯", "Edit"), Some(Icon::Cut), tr!("剪掉頭尾、刪除片段、加上標註", "Trim, cut out parts, add annotations"))));
        if !audio_rec && screenrecorder_core::cover::can_embed(path) {
            let tip = if place == Place::Viewer {
                tr!(
                    "把現在這一格設為封面：檔案總管和多數播放器看到的縮圖就是這一格（影片內容不變）",
                    "Use the current frame as the cover: File Explorer and most players show it as the thumbnail (the video itself is unchanged)"
                )
            } else {
                tr!(
                    "開啟播放器選一格當封面：檔案總管和多數播放器看到的縮圖就是這一格（影片內容不變）",
                    "Opens the player to pick a frame as the cover, shown as the thumbnail in File Explorer and most players (the video itself is unchanged)"
                )
            };
            change.push(one(item(EntryAction::SetThumb, tr!("設為封面", "Set as cover"), Some(Icon::Image), tip)));
        }
        if screenrecorder_core::video_tags::can_tag(path) {
            change.push(one(item(
                EntryAction::Tags,
                tr!("影片資訊…", "Details…"),
                Some(Icon::Info),
                tr!(
                    "改寫在影片檔裡的標題、作者、說明、標籤（檔案總管「內容 → 詳細資料」看得到；影片內容不變）",
                    "Edit the title, author, description and tags stored in the video file (shown in File Explorer under Properties → Details; the video itself is unchanged)"
                ),
            )));
            if !audio_rec {
                let tip = tr!("直接改原檔，幾秒就好、畫質不變；轉錯再轉回來就好", "Changes the original in seconds without losing quality; just rotate back if needed");
                change.push(Node::Sub {
                    label: tr!("旋轉", "Rotate").into(),
                    icon: Icon::RotR,
                    tip: tip.into(),
                    items: vec![
                        item(EntryAction::RotateLeft, tr!("向左轉 90°", "Rotate left 90°"), Some(Icon::RotL), tip),
                        item(EntryAction::RotateRight, tr!("向右轉 90°", "Rotate right 90°"), Some(Icon::RotR), tip),
                        item(EntryAction::Rotate180, tr!("轉 180°", "Rotate 180°"), None, tip),
                        item(
                            EntryAction::Upright,
                            tr!("另存畫面轉好的影片…", "Save an upright copy…"),
                            None,
                            tr!(
                                "少數舊播放器、網頁編輯器不看旋轉的設定時用：把畫面真的轉過來另存一份（重新壓縮，比較慢）",
                                "For old players or web editors that ignore the rotation setting: saves a copy with the picture actually turned (re-encoded, slower)"
                            ),
                        ),
                    ],
                });
            }
        }
        let tip = if e.exports.is_empty() { tr!("重新命名（F2）", "Rename (F2)") } else { tr!("重新命名（加速版、GIF 一起改；F2）", "Rename (sped-up versions and GIFs too; F2)") };
        change.push(one(item(EntryAction::Rename, tr!("重新命名…", "Rename…"), Some(Icon::Rename), tip)));
    }

    let mut make = vec![];
    if !m4a && !export {
        let mut items = vec![
            gate(item(
                EntryAction::Export,
                tr!("加速版、GIF、WebP…", "Sped-up video, GIF, WebP…"),
                None,
                tr!("另存加速版、GIF 或 WebP 動圖（原檔不變）", "Save a sped-up version, GIF or WebP (the original is kept)"),
            )),
            gate(item(
                EntryAction::Compress,
                tr!("壓縮…", "Compress…"),
                None,
                tr!("縮小檔案：保留畫質，或壓縮到 LINE、Email 傳得出去的大小（原速）", "Make the file smaller: keep the quality, or fit a size you can send via LINE or email (original speed)"),
            )),
            gate(item(EntryAction::Vertical, tr!("直式影片（Shorts / Reels）…", "Vertical video (Shorts / Reels)…"), None, "")),
        ];
        if e.media.has_audio != Some(false) {
            items.push(item(EntryAction::SaveAudio, tr!("存成 M4A（只留聲音）", "Save as M4A (audio only)"), None, ""));
        }
        if !audio_rec {
            items.push(item(
                EntryAction::StepsGuide,
                tr!("步驟教學文件（附圖）…", "Step-by-step guide with pictures…"),
                None,
                tr!(
                    "錄影時點的每個按鈕、按的快速鍵各做成一步，附上那一刻的畫面（圈出點的地方），整理後存成 HTML、Word、PowerPoint 或影片",
                    "Each button you clicked and shortcut you pressed becomes a step with the frame from that moment (the click is circled). Tidy them up, then save as HTML, Word, PowerPoint or video"
                ),
            ));
        }
        if screenrecorder_core::import::is_other_video(path) {
            let mut c = item(
                EntryAction::Convert,
                tr!("轉成 MP4", "Convert to MP4"),
                None,
                tr!("MOV、MKV 等要先轉成 MP4 才能剪輯、製作", "MOV, MKV and similar files must be converted to MP4 before editing"),
            );
            c.blocked = app.blocked(EntryAction::Export);
            items.push(c);
        }
        make.push(Node::Sub {
            label: tr!("製作", "Make").into(),
            icon: Icon::Fast,
            tip: tr!(
                "另存加速版、GIF、WebP、直式影片、M4A、步驟教學，或壓縮檔案（原檔不變）",
                "Save a sped-up version, GIF, WebP, vertical video, M4A or step guide, or compress the file (the original is kept)"
            )
            .into(),
            items,
        });
    }

    let mut share = vec![one(item(
        EntryAction::CopyFile,
        tr!("複製檔案", "Copy file"),
        Some(Icon::Copy),
        tr!("複製這個檔案，可以直接貼到 LINE、Teams、信件或資料夾", "Copy this file so you can paste it into LINE, Teams, an email or a folder"),
    ))];
    let (dest, send_tip) = app.send_label();
    share.push(one(item(EntryAction::SendToLibrary, tr!("傳到媒體庫", "Send to library"), Some(Icon::Upload), &format!("{dest}\n{send_tip}"))));
    if !m4a && app.jellyfin().is_some() {
        share.push(one(item(
            EntryAction::OpenJellyfin,
            tr!("在 Jellyfin 開啟", "Open in Jellyfin"),
            Some(Icon::Video),
            tr!("用瀏覽器打開這支影片在 Jellyfin 的頁面（要先傳到 Jellyfin 的媒體庫）", "Open this video's Jellyfin page in your browser (send it to the Jellyfin library first)"),
        )));
        share.push(one(item(
            EntryAction::JellyfinLink,
            tr!("複製 Jellyfin 連結", "Copy Jellyfin link"),
            Some(Icon::Link),
            tr!(
                "複製這支影片在 Jellyfin 的網址，貼給別人就能在瀏覽器看（要先傳到 Jellyfin 的媒體庫）",
                "Copy this video's Jellyfin address so others can watch it in a browser (send it to the Jellyfin library first)"
            ),
        )));
    }
    if !e.media.chapters.is_empty() {
        share.push(one(item(EntryAction::CopyChapters, tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)"), Some(Icon::List), "")));
    }
    if !m4a && !export && !audio_rec {
        share.push(one(item(
            EntryAction::CopySteps,
            tr!("複製操作步驟（文字）", "Copy steps as text"),
            Some(Icon::Keyboard),
            tr!("整理成「1. 點「存檔」」這樣的清單，貼到文件或信件裡", "A numbered list like “1. Click “Save””, to paste into a document or email"),
        )));
    }

    let starred = app.core.pins.contains(path);
    let mut manage = vec![one(if starred {
        item(EntryAction::Star, tr!("移除星號", "Remove star"), Some(Icon::StarOn), "")
    } else {
        item(
            EntryAction::Star,
            tr!("加上星號", "Add star"),
            Some(Icon::Star),
            tr!("加上星號的不會被清理，也可以只看有星號的", "Starred files are skipped by cleanup, and you can show only starred ones"),
        )
    })];
    if place != Place::Viewer {
        manage.push(one(item(EntryAction::Reveal, tr!("在資料夾中顯示", "Show in folder"), Some(Icon::Folder), "")));
    }
    manage.push(one(item(EntryAction::External, tr!("用 Windows 開啟", "Open in Windows"), Some(Icon::Export), tr!("用 Windows 的媒體播放器開啟", "Open in the Windows media player"))));
    if place != Place::Viewer {
        let tip = if e.exports.is_empty() { "" } else { tr!("連同底下的加速版、GIF", "Including its sped-up versions and GIFs") };
        manage.push(one(item(EntryAction::Trash, tr!("移到資源回收筒", "Move to Recycle Bin"), Some(Icon::Trash), tip)));
    }

    [look, change, make, share, manage].into_iter().filter(|g| !g.is_empty()).collect()
}

/// 選單的一個項目；不能用時停用並在滑鼠提示說明原因
fn menu_item(ui: &mut egui::Ui, it: &Item) -> bool {
    let r = ui.add_enabled(it.blocked.is_none(), egui::Button::new(&it.label));
    match &it.blocked {
        Some(why) => r.on_disabled_hover_text(why).clicked(),
        None if it.tip.is_empty() => r.clicked(),
        None => r.on_hover_text(&it.tip).clicked(),
    }
}

fn sub_items(ui: &mut egui::Ui, items: &[Item]) -> Option<EntryAction> {
    let mut pick = None;
    for it in items {
        if menu_item(ui, it) {
            pick = Some(it.action);
            ui.close();
        }
    }
    pick
}

/// 「⋯」與按右鍵的選單：全部動作，一組一條分隔線
pub fn menu(ui: &mut egui::Ui, groups: &[Vec<Node>]) -> Option<EntryAction> {
    ui.set_min_width(220.0);
    let mut pick = None;
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            ui.separator();
        }
        for n in g {
            match n {
                Node::One(it) => {
                    if menu_item(ui, it) {
                        pick = Some(it.action);
                    }
                }
                Node::Sub { label, items, .. } => {
                    ui.menu_button(label, |ui| {
                        if let Some(a) = sub_items(ui, items) {
                            pick = Some(a);
                        }
                    });
                }
            }
        }
    }
    pick
}

/// 卡片、清單一列上的小圖示：直接露出的那幾個（固定的位置，這支沒有的空著，上下列才對得齊），最後是「⋯」
pub fn quick_icons(ui: &mut egui::Ui, groups: &[Vec<Node>], hot: bool, more_tip: &str) -> Option<EntryAction> {
    let mut pick = None;
    let quick: Vec<&Node> = groups.iter().flatten().filter(|n| n.quick()).collect();
    for slot in 0..5 {
        let n = quick.iter().find(|n| slot_of(n) == slot);
        // 這支沒有的：留一個和按鈕一樣大的空位（上下列的圖示才對得齊）
        let Some(n) = n else {
            ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
            continue;
        };
        let Some(icon) = n.icon() else { continue };
        let blocked = match n {
            Node::One(it) => it.blocked.clone(),
            Node::Sub { .. } => None,
        };
        let tip = blocked.clone().unwrap_or_else(|| if n.tip().is_empty() { n.label().to_string() } else { format!("{}\n{}", n.label(), n.tip()) });
        // 播放一直是深色，其他平常淡灰色，滑鼠移到上面才變深
        let r = Btn::icon_only(icon).ghost().small().quiet(!hot && slot != 0).enabled(blocked.is_none()).tooltip(tip).show(ui);
        match n {
            Node::One(it) => {
                if r.clicked() {
                    pick = Some(it.action);
                }
            }
            Node::Sub { items, .. } => {
                egui::Popup::menu(&r).show(|ui| {
                    ui.set_min_width(200.0);
                    if let Some(a) = sub_items(ui, items) {
                        pick = Some(a);
                    }
                });
            }
        }
    }
    let more = Btn::icon_only(Icon::More).ghost().small().quiet(!hot).tooltip(more_tip).show(ui);
    egui::Popup::menu(&more).show(|ui| {
        if let Some(a) = menu(ui, groups) {
            pick = Some(a);
        }
    });
    pick
}

/// 直接露出的圖示固定的位置：播放、剪輯、製作、傳到媒體庫、在資料夾中顯示
fn slot_of(n: &Node) -> usize {
    match n {
        Node::One(it) => match it.action {
            EntryAction::Play | EntryAction::PlayHere => 0,
            EntryAction::Edit => 1,
            EntryAction::SendToLibrary => 3,
            _ => 4,
        },
        Node::Sub { .. } => 2,
    }
}
