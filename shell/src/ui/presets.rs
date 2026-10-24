//! 錄影設定組合（主畫面下方「設定」左邊的按鈕）：切換、把目前的設定存成組合、重新命名、刪除。
//! 組合存在 settings.json 的 presets（見 core 的 presets.rs），系統匣選單也能切換。

use super::dialogs::Ask;
use super::settings::UiSettings;
use super::theme::{self, Btn, Icon};
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

/// 上次切換或儲存的組合（目前的設定和它不一樣時，寫「已修改」並可以更新它）
fn last_used(app: &UiApp) -> Option<usize> {
    (!app.s.preset_name.is_empty()).then(|| presets::find(&app.presets, &app.s.preset_name)).flatten()
}

fn label(app: &UiApp, current: Option<usize>) -> String {
    match (current, last_used(app)) {
        (Some(i), _) => trf!("組合：{}", "Preset: {}", app.presets[i].name),
        (None, Some(i)) => trf!("組合：{}（已修改）", "Preset: {} (modified)", app.presets[i].name),
        (None, None) => tr!("設定組合", "Presets").to_string(),
    }
}

/// 按鈕的寬度（預覽上方那一列先扣掉）
pub fn width(app: &UiApp, ui: &Ui) -> f32 {
    Btn::new(label(app, active(app))).icon(Icon::List).ghost().small().width(ui)
}

/// 預覽上方的按鈕（有建立組合時）
pub fn button(app: &mut UiApp, ui: &mut Ui) {
    let locked = app.locked();
    let current = active(app);
    let label = label(app, current);
    let modified = last_used(app).filter(|_| current.is_none());
    let tip = match (current, modified) {
        (Some(i), _) => trf!("目前是「{}」設定組合。按一下切換或管理組合", "Using the “{}” preset. Click to switch or manage presets", app.presets[i].name),
        (None, Some(i)) => trf!(
            "目前的設定和「{}」組合不一樣了。按一下可以更新這個組合、切換或管理組合",
            "The current settings differ from the “{}” preset. Click to update it, switch or manage presets",
            app.presets[i].name
        ),
        (None, None) => tr!(
            "把範圍、畫面、聲音、儲存位置等錄影設定存成組合（例如「教學影片」「線上會議」），之後一次切換；系統匣選單也能切換",
            "Save the recording settings (area, video, audio, save location…) as a preset such as “Tutorial” or “Meeting” and switch in one click. You can also switch from the tray menu."
        )
        .to_string(),
    };
    let resp = Btn::new(&label).icon(Icon::List).ghost().small().enabled(!locked).tooltip(tip).show(ui);
    egui::Popup::menu(&resp).show(|ui| menu_body(app, ui));
}

/// 組合清單與管理（預覽上方的按鈕、「⋯」總選單的子選單都用這個）
pub fn menu_body(app: &mut UiApp, ui: &mut Ui) {
    let current = active(app);
    let modified = last_used(app).filter(|_| current.is_none());
    let mut apply = None;
    let mut save = false;
    let mut update = None;
    let mut rename = None;
    let mut delete = None;
    {
        ui.set_min_width(240.0);
        // 組合裡存了什麼
        ui.label(theme::muted(ui, tr!("包含範圍、畫面、聲音、儲存位置", "Includes the area, video, audio and save location")));
        for (i, p) in app.presets.iter().enumerate() {
            let text = if modified == Some(i) { trf!("{}（已修改）", "{} (modified)", p.name) } else { p.name.clone() };
            if ui.selectable_label(current == Some(i), text).clicked() && current != Some(i) {
                apply = Some(i);
            }
        }
        if !app.presets.is_empty() {
            ui.separator();
        }
        if let Some(i) = modified {
            if ui.button(trf!("更新「{}」組合", "Update the “{}” preset", app.presets[i].name)).on_hover_text(tr!("用目前的設定更新這個組合", "Update this preset with the current settings")).clicked()
            {
                update = Some(i);
            }
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
    }
    if let Some(i) = apply {
        apply_preset(app, i);
    }
    if save {
        save_as(app);
    }
    if let Some(i) = update {
        let name = app.presets[i].name.clone();
        save_now(app, &name);
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
    app.s.preset_name = p.name.clone();
    app.save_settings();
    app.toast(trf!("已切換到「{}」", "Switched to “{}”", p.name), false);
}

/// 建議的名稱：錄影範圍（重複時加編號）
fn suggest(app: &UiApp) -> String {
    let base = app.s.source_label(&app.env);
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base} {n}") }).find(|name| presets::find(&app.presets, name).is_none()).unwrap_or(base)
}

fn save_as(app: &mut UiApp) {
    // 名稱一開始是空的（灰色提示寫例子），不用先刪掉建議的名稱
    let mut ask = Ask::input(
        tr!("儲存設定組合", "Save preset"),
        tr!("名稱（範圍、畫面、聲音、儲存位置會存在一起）", "Name (the area, video, audio and save location are saved together)"),
        "",
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
    );
    ask.hint = trf!("例如：教學影片（或「{}」）", "e.g. Tutorial (or “{}”)", suggest(app));
    app.ask = Some(ask);
}

fn save_now(app: &mut UiApp, name: &str) {
    let p = Preset::capture(name, &app.s.to_value(), &app.s.record_config(&app.env));
    let mut list = app.presets.clone();
    if !presets::upsert(&mut list, p) {
        app.toast(trf!("最多 {} 組，請先刪除用不到的", "Up to {} presets. Delete one you don't need first.", presets::MAX), true);
        return;
    }
    store(app, list);
    app.s.preset_name = name.to_string();
    app.save_settings();
    app.toast(trf!("已儲存設定組合「{name}」", "Saved preset “{name}”"), false);
}

fn rename_preset(app: &mut UiApp, i: usize) {
    let Some(old) = app.presets.get(i).map(|p| p.name.clone()) else { return };
    app.ask = Some(Ask::input(tr!("重新命名設定組合", "Rename preset"), tr!("新名稱", "New name"), old.clone(), tr!("重新命名", "Rename"), move |app, value| {
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
            // 改的是上次用的組合：跟著重新命名
            if p.name == app.s.preset_name {
                app.s.preset_name = name.clone();
                app.save_settings();
            }
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
