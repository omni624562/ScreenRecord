//! 拖曳文字：選好的字按住拖到輸入框放開就複製過去；從其他程式（瀏覽器、記事本、Word）拖進來的文字也一樣（Windows）。
//! egui 的輸入框本身不會：按在選取的字上會變成重新選取。不用每個輸入框各加，每一格前後看：
//! 按下去的位置在剛才選取的字上，就把選取還原、讓 egui 以為已經放開（之後移動只算滑過），
//! 真的放開時看放在哪個輸入框，在那裡點一下（拿到焦點、游標放到那裡）再貼上。

use eframe::egui::{self, text::CCursor, text::CCursorRange, text_edit::TextEditState, Context, Event, Id, LayerId, Order, OutputCommand, PointerButton, Pos2, RawInput, Rect};
use std::cell::RefCell;

enum Phase {
    Idle,
    /// 按在選取的字上、還沒拖：原本的選取、egui 放的游標（沒拖就放開時用）、按下的位置；text 是複製出來的字
    Press {
        id: Id,
        range: CCursorRange,
        click: CCursor,
        at: Pos2,
        text: Option<String>,
    },
    /// 拖著字
    Drag {
        id: Id,
        range: CCursorRange,
        text: Option<String>,
    },
    /// 放在輸入框上了：下一格在那裡點一下，再下一格貼上
    Click {
        pos: Pos2,
        text: String,
    },
    Paste {
        text: String,
    },
}

struct State {
    phase: Phase,
    /// 有焦點的輸入框上一格選取的範圍
    last: Option<(Id, CCursorRange)>,
    /// 下一格要讓 egui 以為放開了（並複製選取的字）
    release: Option<Pos2>,
    /// 真的放開的位置（沒交給 egui）
    released: Option<Pos2>,
    /// 從其他程式放下的文字（邏輯座標）
    outside: Option<(Pos2, String)>,
}

thread_local! {
    static ST: RefCell<State> = const { RefCell::new(State { phase: Phase::Idle, last: None, release: None, released: None, outside: None }) };
}

/// 送進 egui 之前：收其他程式拖進來的東西、吞掉拖曳中的放開、補上點一下和貼上
pub fn raw_input(ctx: &Context, raw: &mut RawInput) {
    let ppp = ctx.pixels_per_point();
    for d in screenrecorder_core::dropin::take() {
        match d {
            screenrecorder_core::dropin::Dropped::Files(list) => raw.dropped_files.extend(list.into_iter().map(|p| std::sync::Arc::new(LocalFile(p)) as egui::DroppedFileHandle)),
            screenrecorder_core::dropin::Dropped::Text { text, x, y } => ST.with_borrow_mut(|s| s.outside = Some((Pos2::new(x / ppp, y / ppp), text))),
        }
    }
    let hovering = screenrecorder_core::dropin::hovered_files();
    if !hovering.is_empty() {
        raw.hovered_files = hovering.into_iter().map(|path| egui::HoveredFile { path: Some(path), ..Default::default() }).collect();
    }
    let modifiers = ctx.input(|i| i.modifiers);
    ST.with_borrow_mut(|s| {
        if matches!(s.phase, Phase::Press { .. } | Phase::Drag { .. }) {
            raw.events.retain(|e| match e {
                Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, .. } => {
                    s.released = Some(*pos);
                    false
                }
                // 拖著的時候按 Esc 只是不拖了（不要關掉視窗）
                Event::Key { key: egui::Key::Escape, .. } => {
                    s.phase = Phase::Idle;
                    false
                }
                _ => true,
            });
        }
        // 先吞掉真的放開，再補上給 egui 的放開（排在最前面：同一批裡後面的移動就只算滑過，不會變成拖曳選取）
        if let Some(pos) = s.release.take() {
            raw.events.insert(0, Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers });
            raw.events.insert(1, Event::Copy);
        }
        match std::mem::replace(&mut s.phase, Phase::Idle) {
            Phase::Click { pos, text } => {
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers });
                raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers });
                s.phase = Phase::Paste { text };
            }
            Phase::Paste { text } => raw.events.push(Event::Paste(text)),
            other => s.phase = other,
        }
    });
}

/// 每一格畫完介面之後：看是不是按在選取的字上、拖到哪裡、放在哪個輸入框
pub fn after(ctx: &Context) {
    // 複製出來的字是給拖曳用的，不要放進剪貼簿
    let wants_copy = ST.with_borrow(|s| matches!(s.phase, Phase::Press { text: None, .. } | Phase::Drag { text: None, .. }));
    if wants_copy {
        let got = ctx.output_mut(|o| {
            let i = o.commands.iter().position(|c| matches!(c, OutputCommand::CopyText(_)))?;
            match o.commands.remove(i) {
                OutputCommand::CopyText(t) => Some(t),
                _ => None,
            }
        });
        if let Some(t) = got {
            ST.with_borrow_mut(|s| {
                if let Phase::Press { text, .. } | Phase::Drag { text, .. } = &mut s.phase {
                    *text = Some(t);
                }
            });
        }
    }
    let pointer = ctx.input(|i| i.pointer.latest_pos());
    let pressed = ctx.input(|i| i.pointer.primary_pressed() && i.pointer.primary_down());
    let focused = ctx.memory(|m| m.focused()).and_then(|id| Some((id, TextEditState::load(ctx, id)?)));
    ST.with_borrow_mut(|s| {
        match std::mem::replace(&mut s.phase, Phase::Idle) {
            Phase::Idle => {
                // 按在剛才選取的字上（egui 已經把游標移到按的位置）：還原選取，下一格讓 egui 以為放開了
                if let (true, Some((id, ts)), Some((lid, prev)), Some(at)) = (pressed, &focused, s.last, pointer) {
                    let now = ts.cursor.char_range();
                    if *id == lid && !prev.is_empty() && now.is_some_and(|r| r.is_empty() && inside(prev, r.primary)) {
                        let mut ts = ts.clone();
                        ts.cursor.set_char_range(Some(prev));
                        ts.store(ctx, *id);
                        s.phase = Phase::Press { id: *id, range: prev, click: now.map(|r| r.primary).unwrap_or_default(), at, text: None };
                        s.release = Some(at);
                        ctx.request_repaint();
                    }
                }
            }
            Phase::Press { id, range, click, at, text } => {
                if s.released.take().is_some() {
                    // 沒拖就放開：和平常點一下一樣，游標放在按的位置
                    if let Some(mut ts) = TextEditState::load(ctx, id) {
                        ts.cursor.set_char_range(Some(CCursorRange::one(click)));
                        ts.store(ctx, id);
                    }
                } else {
                    keep(ctx, id, range);
                    s.phase = if pointer.is_some_and(|p| p.distance(at) > 5.0) { Phase::Drag { id, range, text } } else { Phase::Press { id, range, click, at, text } };
                }
            }
            Phase::Drag { id, range, text } => match s.released.take() {
                Some(pos) => {
                    if let (Some(text), Some(_)) = (text, field_at(ctx, pos)) {
                        s.phase = Phase::Click { pos, text };
                        ctx.request_repaint();
                    }
                }
                None => {
                    keep(ctx, id, range);
                    if let Some(p) = pointer {
                        ghost(ctx, p, text.as_deref().unwrap_or(""), field_at(ctx, p));
                    }
                    s.phase = Phase::Drag { id, range, text };
                }
            },
            other => {
                ctx.request_repaint();
                s.phase = other;
            }
        }
        // 其他程式拖進來的字：放在輸入框上才貼
        if let Some((pos, text)) = s.outside.take() {
            if matches!(s.phase, Phase::Idle) && field_at(ctx, pos).is_some() {
                s.phase = Phase::Click { pos, text };
                ctx.request_repaint();
            }
        }
        if matches!(s.phase, Phase::Idle) {
            s.last = focused.as_ref().and_then(|(id, ts)| Some((*id, ts.cursor.char_range()?)));
        }
    });
    // 其他程式的字拖著經過：滑鼠下面是輸入框才能放，框起來
    if let Some((x, y)) = screenrecorder_core::dropin::text_hover() {
        let ppp = ctx.pixels_per_point();
        let target = field_at(ctx, Pos2::new(x / ppp, y / ppp));
        screenrecorder_core::dropin::set_accept_text(target.is_some());
        if let Some((_, r)) = target {
            outline(ctx, r);
        }
    }
}

/// 從檔案總管拖進來的檔案（交給 egui 的 dropped_files，和原本 winit 給的一樣）
#[derive(Debug)]
struct LocalFile(std::path::PathBuf);

impl egui::DroppedFile for LocalFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// 拖著的字跟著滑鼠；下面是輸入框時框起來
fn ghost(ctx: &Context, at: Pos2, text: &str, target: Option<(Id, Rect)>) {
    let p = super::theme::pal_ctx(ctx);
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("text-drag")));
    let short: String = text.chars().take(40).collect::<String>().replace(['\n', '\r'], " ");
    let short = if text.chars().count() > 40 { format!("{short}…") } else { short };
    let g = painter.layout_no_wrap(short, super::theme::font(12.5), p.text);
    let r = Rect::from_min_size(at + egui::vec2(14.0, 16.0), g.size() + egui::vec2(12.0, 8.0));
    painter.rect(r, 4.0, p.surface, egui::Stroke::new(1.0, p.border_strong), egui::StrokeKind::Inside);
    painter.galley(r.min + egui::vec2(6.0, 4.0), g, p.text);
    match target {
        Some((_, t)) => {
            outline(ctx, t);
            ctx.set_cursor_icon(egui::CursorIcon::Copy);
        }
        None => ctx.set_cursor_icon(egui::CursorIcon::NoDrop),
    }
}

fn outline(ctx: &Context, r: Rect) {
    let p = super::theme::pal_ctx(ctx);
    ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("text-drop"))).rect_stroke(r.expand(1.0), 4.0, egui::Stroke::new(2.0, p.accent), egui::StrokeKind::Outside);
}

/// 拖著的時候選取不變（egui 可能把同一批的移動當成拖曳選取）
fn keep(ctx: &Context, id: Id, range: CCursorRange) {
    if let Some(mut ts) = TextEditState::load(ctx, id) {
        if ts.cursor.char_range() != Some(range) {
            ts.cursor.set_char_range(Some(range));
            ts.store(ctx, id);
        }
    }
}

/// 這個位置上的輸入框（最上層、可以操作的）
fn field_at(ctx: &Context, pos: Pos2) -> Option<(Id, Rect)> {
    let layer = ctx.layer_id_at(pos)?;
    let list: Vec<(Id, Rect)> =
        ctx.viewport(|v| v.this_pass.widgets.get_layer(layer).filter(|w| w.enabled && w.sense.senses_click() && w.interact_rect.contains(pos)).map(|w| (w.id, w.interact_rect)).collect());
    list.into_iter().rev().find(|(id, _)| TextEditState::load(ctx, *id).is_some())
}

fn inside(r: CCursorRange, c: CCursor) -> bool {
    let (a, b) = if r.primary.index <= r.secondary.index { (r.primary.index, r.secondary.index) } else { (r.secondary.index, r.primary.index) };
    (a..=b).contains(&c.index)
}
