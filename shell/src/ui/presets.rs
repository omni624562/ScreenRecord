//! 錄影設定組合（主畫面下方「設定」左邊的按鈕）：切換、把目前的設定存成組合、重新命名、刪除。
//! 組合存在 settings.json 的 presets（見 core 的 presets.rs），系統匣選單也能切換。

use super::dialogs::Ask;
use super::settings::UiSettings;
use super::theme::{Btn, Icon};
use super::UiApp;
use eframe::egui::{self, Ui};
use screenrecorder_core::presets::{self, Preset};
use screenrecorder_core::settings::SettingsPatch;
use screenrecorder_core::{tr, trf};

/// 目前的設定是哪一組
pub fn active(app: &UiApp) -> Option<usize> {
    if app.presets.is_empty() {
        return None;
    }
    presets::active(&app.presets, &app.s.to_value())
}

fn label(app: &UiApp, current: Option<usize>) -> String {
    match current {
        Some(i) => app.presets[i].name.clone(),
        None => tr!("設定組合", "Presets").to_string(),
    }
}

/// 按鈕的寬度（設定列先扣掉，其他摘要依剩下的寬度取捨）
pub fn width(app: &UiApp, ui: &Ui) -> f32 {
    Btn::new(label(app, active(app))).icon(Icon::List).ghost().small().width(ui)
}

/// 下方設定列的按鈕
pub fn button(app: &mut UiApp, ui: &mut Ui) {
    let locked = app.locked();
    let current = active(app);
    let label = label(app, current);
    let tip = match current {
        Some(i) => trf!("目前是「{}」設定組合。點一下切換或管理組合", "Using the “{}” preset. Click to switch or manage presets", app.presets[i].name),
        None => tr!(
            "把範圍、畫面、聲音、儲存位置等錄影設定存成組合（例如「教學影片」「線上會議」），之後一次切換；系統匣選單也能切換",
            "Save the recording settings (area, video, audio, save location…) as a preset such as “Tutorial” or “Meeting” and switch in one click. You can also switch from the tray menu."
        )
        .to_string(),
    };
    let resp = Btn::new(&label).icon(Icon::List).ghost().small().enabled(!locked).tooltip(tip).show(ui);
    let mut apply = None;
    let mut save = false;
    let mut rename = None;
    let mut delete = None;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(220.0);
        for (i, p) in app.presets.iter().enumerate() {
            if ui.selectable_label(current == Some(i), &p.name).clicked() && current != Some(i) {
                apply = Some(i);
            }
        }
        if !app.presets.is_empty() {
            ui.separator();
        }
        let full = app.presets.len() >= presets::MAX;
        if ui
            .add_enabled(!full, egui::Button::new(tr!("把目前的設定存成組合…", "Save current settings as a preset…")))
            .on_hover_text(tr!("用已經有的名稱儲存，就是用目前的設定更新那一組", "Saving with an existing name updates that preset with the current settings"))
            .on_disabled_hover_text(trf!("最多 {} 組，請先刪除用不到的", "Up to {} presets. Delete one you don't need first.", presets::MAX))
            .clicked()
        {
            save = true;
        }
        if !app.presets.is_empty() {
            ui.menu_button(tr!("重新命名", "Rename"), |ui| {
                for (i, p) in app.presets.iter().enumerate() {
                    if ui.button(&p.name).clicked() {
                        rename = Some(i);
                    }
                }
            });
            ui.menu_button(tr!("刪除", "Delete"), |ui| {
                for (i, p) in app.presets.iter().enumerate() {
                    if ui.button(&p.name).clicked() {
                        delete = Some(i);
                    }
                }
            });
        }
    });
    if let Some(i) = apply {
        apply_preset(app, i);
    }
    if save {
        save_as(app);
    }
    if let Some(i) = rename {
        rename_preset(app, i);
    }
    if let Some(i) = delete {
        delete_preset(app, i);
    }
}

/// 存檔並記下新的 rev（不會被當成別處改的設定再讀一次）
fn store(app: &mut UiApp, list: Vec<Preset>) {
    app.presets = list.clone();
    app.settings_rev = app.core.settings.save(SettingsPatch { presets: Some(list), ..Default::default() }).rev;
}

pub fn apply_preset(app: &mut UiApp, i: usize) {
    let Some(p) = app.presets.get(i).cloned() else { return };
    let mut v = app.s.to_value();
    p.apply_to(&mut v);
    app.s = UiSettings::from_saved(Some(&v), &app.env);
    app.save_settings();
    app.toast(trf!("已切換到「{}」", "Switched to “{}”", p.name), false);
}

/// 建議的名稱：錄影範圍（重複時加編號）
fn suggest(app: &UiApp) -> String {
    let base = app.s.source_label(&app.env);
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base} {n}") }).find(|name| presets::find(&app.presets, name).is_none()).unwrap_or(base)
}

fn save_as(app: &mut UiApp) {
    let name = suggest(app);
    app.ask = Some(Ask::input(
        tr!("儲存設定組合", "Save preset"),
        tr!("名稱（例如「教學影片」「線上會議」）", "Name (e.g. “Tutorial” or “Meeting”)"),
        name,
        tr!("儲存", "Save"),
        |app, value| {
            let Some(name) = presets::clean_name(&value) else {
                save_as(app);
                if let Some(a) = &mut app.ask {
                    a.error = Some(tr!("請輸入名稱", "Enter a name").into());
                }
                return;
            };
            match presets::find(&app.presets, &name) {
                // 已經有同名的：確認後用目前的設定更新
                Some(i) => {
                    let old = app.presets[i].name.clone();
                    app.ask = Some(Ask::confirm(
                        tr!("更新設定組合", "Update preset"),
                        trf!("已經有「{old}」，要用目前的設定取代它嗎？", "“{old}” already exists. Replace it with the current settings?"),
                        tr!("取代", "Replace"),
                        move |app, _| save_now(app, &name),
                    ));
                }
                None => save_now(app, &name),
            }
        },
    ));
}

fn save_now(app: &mut UiApp, name: &str) {
    let p = Preset::capture(name, &app.s.to_value(), &app.s.record_config(&app.env));
    let mut list = app.presets.clone();
    if !presets::upsert(&mut list, p) {
        app.toast(trf!("最多 {} 組，請先刪除用不到的", "Up to {} presets. Delete one you don't need first.", presets::MAX), true);
        return;
    }
    store(app, list);
    app.toast(trf!("已儲存設定組合「{name}」", "Saved preset “{name}”"), false);
}

fn rename_preset(app: &mut UiApp, i: usize) {
    let Some(old) = app.presets.get(i).map(|p| p.name.clone()) else { return };
    app.ask = Some(Ask::input(tr!("重新命名設定組合", "Rename preset"), tr!("新名稱", "New name"), old.clone(), tr!("改名", "Rename"), move |app, value| {
        let Some(name) = presets::clean_name(&value) else {
            rename_preset(app, i);
            if let Some(a) = &mut app.ask {
                a.error = Some(tr!("請輸入名稱", "Enter a name").into());
            }
            return;
        };
        if presets::find(&app.presets, &name).is_some_and(|j| j != i) {
            rename_preset(app, i);
            if let Some(a) = &mut app.ask {
                a.error = Some(trf!("已經有「{name}」了", "“{name}” already exists"));
            }
            return;
        }
        let mut list = app.presets.clone();
        if let Some(p) = list.get_mut(i) {
            p.name = name;
        }
        store(app, list);
    }));
}

fn delete_preset(app: &mut UiApp, i: usize) {
    let Some(name) = app.presets.get(i).map(|p| p.name.clone()) else { return };
    let mut ask = Ask::confirm(
        tr!("刪除設定組合", "Delete preset"),
        trf!("要刪除「{name}」嗎？目前的設定不會改變。", "Delete “{name}”? Your current settings won't change."),
        tr!("刪除", "Delete"),
        move |app, _| {
            let mut list = app.presets.clone();
            if i < list.len() {
                list.remove(i);
            }
            store(app, list);
        },
    );
    ask.danger = true;
    app.ask = Some(ask);
}
