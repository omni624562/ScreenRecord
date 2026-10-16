//! 片頭、片尾卡片（剪輯輸出時接在影片前後）。
//! 卡片用 tiny-skia 畫成和輸出影片一樣大的 PNG，FFmpeg 做成幾秒的畫面（淡入淡出）再和影片接起來。

use crate::annotate;
use serde::{Deserialize, Serialize};
use tiny_skia::{Color, GradientStop, LinearGradient, Paint, Pixmap, Point, Rect, SpreadMode, Transform};

/// 卡片的樣式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardStyle {
    /// 深色漸層、白字
    #[default]
    Dark,
    /// 淺色、深色字
    Light,
    /// 藍色漸層、白字
    Blue,
}

impl CardStyle {
    pub const ALL: [CardStyle; 3] = [CardStyle::Dark, CardStyle::Light, CardStyle::Blue];

    /// 背景漸層（上、下）與文字顏色、副標顏色
    fn colors(self) -> ([u8; 3], [u8; 3], &'static str, &'static str) {
        match self {
            CardStyle::Dark => ([34, 37, 46], [14, 15, 20], "#FFFFFF", "#B4BAC8"),
            CardStyle::Light => ([250, 250, 252], [226, 230, 238], "#16181D", "#5A6170"),
            CardStyle::Blue => ([46, 108, 230], [24, 52, 140], "#FFFFFF", "#D2E0FF"),
        }
    }
}

/// 片頭或片尾的一張卡片
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleCard {
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default)]
    pub style: CardStyle,
    /// 顯示幾秒
    pub secs: f64,
}

impl TitleCard {
    pub const MIN_SECS: f64 = 1.0;
    pub const MAX_SECS: f64 = 10.0;

    pub fn new(title: &str) -> TitleCard {
        TitleCard { title: title.into(), subtitle: String::new(), style: CardStyle::Dark, secs: 3.0 }
    }

    pub fn secs(&self) -> f64 {
        self.secs.clamp(Self::MIN_SECS, Self::MAX_SECS)
    }

    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.subtitle.trim().is_empty()
    }
}

/// 畫卡片（w × h）：漸層背景、置中的標題與副標；標題太寬時縮小
pub fn render(card: &TitleCard, w: u32, h: u32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(w.max(2), h.max(2))?;
    let (top, bottom, fg, sub) = card.style.colors();
    let c = |v: [u8; 3]| Color::from_rgba8(v[0], v[1], v[2], 255);
    let shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(w as f32 * 0.3, h as f32),
        vec![GradientStop::new(0.0, c(top)), GradientStop::new(1.0, c(bottom))],
        SpreadMode::Pad,
        Transform::identity(),
    )?;
    let paint = Paint { shader, anti_alias: true, ..Default::default() };
    pm.fill_rect(Rect::from_xywh(0.0, 0.0, w as f32, h as f32)?, &paint, Transform::identity(), None);
    let (wf, hf) = (w as f64, h as f64);
    let fit = |text: &str, size: f64, color: &str| {
        let mut a = annotate::plain_text(text, size, color);
        if a.w > wf * 0.86 {
            a = annotate::plain_text(text, (size * wf * 0.86 / a.w).max(8.0), color);
        }
        a
    };
    let title = card.title.trim();
    let subtitle = card.subtitle.trim();
    let mut lines = vec![];
    if !title.is_empty() {
        lines.push(fit(&crate::subtitles::wrap_lines(title), hf * 0.1, fg));
    }
    if !subtitle.is_empty() {
        lines.push(fit(&crate::subtitles::wrap_lines(subtitle), hf * 0.05, sub));
    }
    let gap = hf * 0.03;
    let total: f64 = lines.iter().map(|a| a.h).sum::<f64>() + gap * lines.len().saturating_sub(1) as f64;
    let mut y = (hf - total) / 2.0;
    for mut a in lines {
        a.x = ((wf - a.w) / 2.0).round();
        a.y = y.round();
        y += a.h + gap;
        annotate::draw(&mut pm, &a, Transform::identity());
    }
    Some(pm)
}

/// 畫好存成 PNG
pub fn save(card: &TitleCard, w: u32, h: u32, path: &std::path::Path) -> Result<(), String> {
    let pm = render(card, w, h).ok_or_else(|| crate::tr!("無法畫出片頭片尾", "Couldn't draw the title card").to_string())?;
    pm.save_png(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(pm: &Pixmap, x: u32, y: u32) -> [u8; 3] {
        let p = pm.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue()]
    }

    #[test]
    fn draws_background_and_centered_text() {
        let card = TitleCard { title: "報價單的填法".into(), subtitle: "業務部教學".into(), style: CardStyle::Light, secs: 3.0 };
        let pm = render(&card, 640, 360).unwrap();
        // 角落是淺色背景
        assert!(px(&pm, 2, 2).iter().all(|v| *v > 200));
        // 中間一帶有深色的字
        let dark = (100..540).flat_map(|x| (120..240).map(move |y| (x, y))).filter(|&(x, y)| px(&pm, x, y).iter().all(|v| *v < 80)).count();
        assert!(dark > 200, "{dark}");
        // 左右對稱（置中）：左右兩邊深色的量差不多
        let side = |r: std::ops::Range<u32>| r.flat_map(|x| (100..260).map(move |y| (x, y))).filter(|&(x, y)| px(&pm, x, y).iter().all(|v| *v < 80)).count() as f64;
        let (l, r) = (side(0..320), side(320..640));
        assert!((l - r).abs() / (l + r) < 0.25, "{l} {r}");
    }

    #[test]
    fn long_titles_shrink_to_fit_and_settings_round_trip() {
        let card = TitleCard::new("這是一個非常非常非常非常非常非常非常非常長的標題用來測試會不會超出畫面");
        let pm = render(&card, 320, 180).unwrap();
        // 最左、最右的一行不會被字蓋到
        assert!((0..180).all(|y| px(&pm, 0, y).iter().all(|v| *v < 60) && px(&pm, 319, y).iter().all(|v| *v < 60)));
        assert_eq!(TitleCard { secs: 99.0, ..card.clone() }.secs(), TitleCard::MAX_SECS);
        let j = serde_json::to_string(&card).unwrap();
        assert_eq!(serde_json::from_str::<TitleCard>(&j).unwrap(), card);
    }
}
