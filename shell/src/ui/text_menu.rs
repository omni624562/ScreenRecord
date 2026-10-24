//! 輸入框的右鍵選單：剪下、複製、貼上、全選（egui 的輸入框本身沒有）。
//! 不用每個輸入框各加：每一格最後看右鍵按在哪個輸入框上（按下去輸入框會拿到焦點），
//! 選了之後下一格一開始把焦點還給那個輸入框，再當成鍵盤事件送進去（剪下、複製、貼上；全選直接設選取範圍）

use eframe::egui::{self, text::CCursor, text::CCursorRange, text_edit::TextEditState, Context, Event, Id, PointerButton};
use screenrecorder_core::tr;

#[derive(Clone, Copy, PartialEq)]
enum Act {
    Cut,
    Copy,
    Paste,
    All,
}

#[derive(Clone, Default)]
struct State {
    /// 選單開在哪個輸入框
    target: Option<Id>,
    /// 有焦點的輸入框上一格選取的範圍：右鍵按下去 egui 會把選取改成按的位置，按在選取的字上要還原
    last: Option<(Id, CCursorRange)>,
    /// 下一格要送的動作
    pending: Option<(Id, Act)>,
}

fn key() -> Id {
    Id::new("text-menu")
}

/// 每一格畫介面之前：把選單選的動作送給輸入框
pub fn before(ctx: &Context) {
    let Some((id, act)) = ctx.data_mut(|d| d.get_temp_mut_or_default::<State>(key()).pending.take()) else { return };
    ctx.memory_mut(|m| m.request_focus(id));
    let ev = match act {
        Act::Cut => Event::Cut,
        Act::Copy => Event::Copy,
        Act::Paste => match screenrecorder_core::clipboard::read_text() {
            Some(t) => Event::Paste(t),
            None => return,
        },
        // 全選直接設選取範圍（送 Ctrl+A 的話，檔案庫的搜尋框空著時會變成「選取全部錄影」）；超過字數的位置 egui 會收回到最後一個字
        Act::All => {
            if let Some(mut ts) = TextEditState::load(ctx, id) {
                ts.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(usize::MAX))));
                ts.store(ctx, id);
            }
            return;
        }
    };
    ctx.input_mut(|i| i.events.push(ev));
}

/// 每一格畫完介面之後：右鍵按在輸入框上就開選單
pub fn after(ctx: &Context) {
    let mut st = ctx.data_mut(|d| d.get_temp::<State>(key())).unwrap_or_default();
    let focused = ctx.memory(|m| m.focused()).filter(|_| st.target.is_none());
    if let Some((id, mut ts)) = focused.and_then(|id| Some((id, TextEditState::load(ctx, id)?))) {
        let pressed = ctx.input(|i| i.pointer.button_pressed(PointerButton::Secondary));
        let range = ts.cursor.char_range();
        match (pressed, st.last) {
            // 右鍵按在剛才選取的字上：保留選取（按在別處就照 egui 把游標移過去，貼上貼在那裡）
            (true, Some((lid, prev))) if lid == id && !prev.is_empty() && range.is_some_and(|r| inside(prev, r.primary)) => {
                ts.cursor.set_char_range(Some(prev));
                ts.store(ctx, id);
            }
            _ => st.last = range.map(|r| (id, r)),
        }
        if ctx.read_response(id).is_some_and(|r| r.secondary_clicked()) {
            st.target = Some(id);
        }
    }
    if let Some(id) = st.target {
        let picked = ctx.read_response(id).and_then(|resp| {
            let sel = TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range()).is_some_and(|r| !r.is_empty());
            let shown = egui::Popup::context_menu(&resp).show(|ui| {
                ui.set_min_width(150.0);
                let mut act = None;
                for (a, label, keys, on) in [(Act::Cut, tr!("剪下", "Cut"), "Ctrl+X", sel), (Act::Copy, tr!("複製", "Copy"), "Ctrl+C", sel), (Act::Paste, tr!("貼上", "Paste"), "Ctrl+V", true)] {
                    if ui.add_enabled(on, egui::Button::new(label).shortcut_text(keys)).clicked() {
                        act = Some(a);
                    }
                }
                ui.separator();
                if ui.add(egui::Button::new(tr!("全選", "Select all")).shortcut_text("Ctrl+A")).clicked() {
                    act = Some(Act::All);
                }
                if act.is_some() {
                    ui.close();
                }
                act
            });
            match shown {
                Some(r) => Some(r.inner),
                // 選單關了
                None => {
                    st.target = None;
                    None
                }
            }
        });
        if let Some(Some(act)) = picked {
            st.pending = Some((id, act));
            st.target = None;
            ctx.request_repaint();
        } else if picked.is_none() {
            st.target = None;
        }
    }
    ctx.data_mut(|d| d.insert_temp(key(), st));
}

fn inside(r: CCursorRange, c: CCursor) -> bool {
    let (a, b) = if r.primary.index <= r.secondary.index { (r.primary.index, r.secondary.index) } else { (r.secondary.index, r.primary.index) };
    (a..=b).contains(&c.index)
}
