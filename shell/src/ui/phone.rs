//! 傳到手機：開一個暫時的下載網頁（core::phone_share），顯示 QR 碼讓手機掃描；關掉視窗就停止分享。

use super::dialogs;
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, Color32, ColorImage, Id, Layout, RichText, TextureHandle, TextureOptions};
use screenrecorder_core::phone_share::{self, Share};
use screenrecorder_core::{tr, trf};

/// QR 碼顯示的大小
const QR_SIZE: f32 = 220.0;

pub struct Phone {
    name: String,
    /// None = 正在準備
    share: Option<Share>,
    qr: Option<TextureHandle>,
}

pub fn open(app: &mut UiApp, path: String) {
    app.phone = Some(Phone { name: dialogs::file_name(&path), share: None, qr: None });
    app.spawn(async move { phone_share::start(std::path::Path::new(&path)).await }, |app, r| match r {
        Ok(share) => {
            let Some(p) = &mut app.phone else { return };
            p.qr = qr_texture(&app.ctx, &share.url);
            p.share = Some(share);
        }
        Err(e) => {
            app.phone = None;
            app.toast(e.message().to_string(), true);
        }
    });
}

/// QR 碼做成圖（每格 1 像素、四周留 4 格白邊；顯示時用 NEAREST 放大才清楚）
fn qr_texture(ctx: &egui::Context, url: &str) -> Option<TextureHandle> {
    let (n, cells) = screenrecorder_core::qr::encode(url)?;
    let side = n + 8;
    let mut img = ColorImage::new([side, side], vec![Color32::WHITE; side * side]);
    for (i, _) in cells.iter().enumerate().filter(|(_, &d)| d) {
        let (x, y) = (i % n + 4, i / n + 4);
        img.pixels[y * side + x] = Color32::BLACK;
    }
    Some(ctx.load_texture("phone-qr", img, TextureOptions::NEAREST))
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(p) = app.phone.take() else { return };
    let mut close = false;
    let modal = egui::Modal::new(Id::new("phone")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(440.0);
        let pal = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("傳到手機", "Send to phone")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉並停止分享（Esc）", "Close and stop sharing (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&p.name).color(pal.muted)).truncate());
        ui.add_space(10.0);
        let Some(share) = &p.share else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr!("正在準備…", "Getting ready…"));
            });
            return;
        };
        if !share.is_running() {
            ui.label(tr!("分享時間到了，已經停止。要再傳一次請關掉這個視窗後重新開始。", "Sharing has timed out and stopped. Close this window and start again to send it once more."));
        } else {
            ui.vertical_centered(|ui| {
                if let Some(t) = &p.qr {
                    ui.add(egui::Image::new((t.id(), vec2(QR_SIZE, QR_SIZE))).corner_radius(6));
                }
            });
            ui.add_space(10.0);
            ui.label(tr!(
                "用手機的相機掃描 QR 碼，就能在手機上看、下載這個檔案。手機要和這台電腦連在同一個 Wi-Fi（或同一個網路）。",
                "Scan the QR code with your phone's camera to view and download this file. Your phone must be on the same Wi-Fi (or network) as this computer."
            ));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&share.url).font(theme::font(12.0)).color(pal.muted)).truncate());
                if Btn::icon_only(Icon::Copy).ghost().small().tooltip(tr!("複製網址", "Copy link")).show(ui).clicked() {
                    ui.ctx().copy_text(share.url.clone());
                }
            });
            ui.add_space(6.0);
            let mins = share.remaining().as_secs().div_ceil(60);
            let n = share.downloads();
            let status = if n == 0 {
                trf!("{mins} 分鐘後自動停止。關掉這個視窗也會停止分享。", "Stops automatically in {mins} min. Closing this window also stops sharing.")
            } else {
                trf!("手機已下載 {n} 次。{mins} 分鐘後自動停止。", "Downloaded {n} time(s). Stops automatically in {mins} min.")
            };
            ui.label(RichText::new(status).font(theme::font(12.0)).color(pal.muted));
            ui.label(
                RichText::new(tr!(
                    "掃不到或打不開：Windows 問要不要讓本程式存取網路時請選「允許」；公司、飯店的訪客 Wi-Fi 可能不讓裝置互連。",
                    "Can't open it? If Windows asks whether to allow network access, choose Allow. Guest Wi-Fi at offices or hotels may block devices from reaching each other."
                ))
                .font(theme::font(12.0))
                .color(pal.muted),
            );
            // 每秒更新剩下的時間、下載次數
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if Btn::new(tr!("停止分享", "Stop sharing")).primary().show(ui).clicked() {
                close = true;
            }
        });
    });
    if close || (modal.should_close() && app.ask.is_none()) {
        // Share 丟掉就停止
        return;
    }
    app.phone = Some(p);
}
