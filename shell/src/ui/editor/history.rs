//! 復原清單：比對前後兩個快照（snapshot() 的 JSON），寫出這一步改了什麼（「刪除 0:03–0:05」「加上標註」）。

use screenrecorder_core::edit::normalize_ranges;
use screenrecorder_core::format::video_clock;
use screenrecorder_core::{tr, trf};
use serde_json::Value;

/// 這一步改了什麼（prev → now）；看不出來時寫「修改」
pub(super) fn describe(prev: &str, now: &str) -> String {
    let (Ok(a), Ok(b)) = (serde_json::from_str::<Value>(prev), serde_json::from_str::<Value>(now)) else {
        return tr!("修改", "Change").into();
    };
    let (da, db) = (&a[0], &b[0]);
    // 有變的欄位，依重要程度找第一個說得出來的
    let changed = |k: &str| da.get(k) != db.get(k);
    let spec = |v: &Value, k: &str| v["spec"][k].as_f64().unwrap_or(0.0);
    if da["spec"]["removed"] != db["spec"]["removed"] {
        let dur = db["duration"].as_f64().unwrap_or(f64::MAX);
        let ra = ranges(&da["spec"]["removed"], dur);
        let rb = ranges(&db["spec"]["removed"], dur);
        let len = |l: &[(f64, f64)]| l.iter().map(|(x, y)| y - x).sum::<f64>();
        return if len(&rb) >= len(&ra) {
            match first_new(&rb, &ra) {
                Some((x, y)) => trf!("刪除 {}–{}", "Remove {}–{}", video_clock(x), video_clock(y)),
                None => tr!("刪除片段", "Remove parts").into(),
            }
        } else {
            match first_new(&ra, &rb) {
                Some((x, y)) => trf!("還原 {}–{}", "Restore {}–{}", video_clock(x), video_clock(y)),
                None => tr!("還原片段", "Restore parts").into(),
            }
        };
    }
    if spec(da, "start") != spec(db, "start") {
        let (s0, s1) = (spec(da, "start"), spec(db, "start"));
        return if s1 > s0 { trf!("剪掉開頭（到 {}）", "Trim the start (to {})", video_clock(s1)) } else { trf!("開頭改成 {}", "Start moved to {}", video_clock(s1)) };
    }
    if spec(da, "end") != spec(db, "end") {
        let (e0, e1) = (spec(da, "end"), spec(db, "end"));
        return if e1 < e0 { trf!("剪掉結尾（從 {}）", "Trim the end (from {})", video_clock(e1)) } else { trf!("結尾改成 {}", "End moved to {}", video_clock(e1)) };
    }
    if changed("splits") {
        let (na, nb) = (count(da, "splits"), count(db, "splits"));
        return if nb > na { tr!("分割", "Split").into() } else { tr!("移除分割點", "Remove a split").into() };
    }
    if changed("anns") {
        let (na, nb) = (count(da, "anns"), count(db, "anns"));
        let subs = changed("subs");
        return match (nb.cmp(&na), subs) {
            (std::cmp::Ordering::Greater, true) => tr!("加上字幕", "Add subtitles").into(),
            (std::cmp::Ordering::Less, true) => tr!("刪除字幕", "Delete subtitles").into(),
            (_, true) => tr!("修改字幕", "Edit subtitles").into(),
            (std::cmp::Ordering::Greater, _) => tr!("加上標註", "Add an annotation").into(),
            (std::cmp::Ordering::Less, _) => tr!("刪除標註", "Delete an annotation").into(),
            _ => tr!("修改標註", "Edit an annotation").into(),
        };
    }
    // 其他欄位：名稱對照（第一個有變的）
    let names: [(&str, &str, &str); 24] = [
        ("fast", "改速度", "Change speed"),
        ("freezes", "定格", "Freeze frame"),
        ("speed", "整支加速", "Speed up the whole video"),
        ("speed_audio", "整支加速的聲音", "Sound when sped up"),
        ("markers", "標記", "Markers"),
        ("order", "調整片段順序", "Reorder sections"),
        ("crop_on", "裁切", "Crop"),
        ("audio", "聲音", "Sound"),
        ("narration", "旁白", "Narration"),
        ("zoom", "跟著點擊放大", "Zoom on clicks"),
        ("zoom_skip", "跟著點擊放大", "Zoom on clicks"),
        ("zoom_all_clicks", "跟著點擊放大", "Zoom on clicks"),
        ("zoom_regions", "手動放大", "Zoom in"),
        ("cursor", "游標", "Cursor"),
        ("cursor_hide", "游標", "Cursor"),
        ("frame", "背景與圓角", "Background and corners"),
        ("vertical", "直式影片", "Vertical video"),
        ("intro", "片頭", "Intro"),
        ("outro", "片尾", "Outro"),
        ("fade_cuts", "接縫淡入淡出", "Fade at cuts"),
        ("sub_style", "字幕樣式", "Subtitle style"),
        ("look", "調色", "Color"),
        ("stamp", "日期時間浮水印", "Date stamp"),
        ("subs", "字幕", "Subtitles"),
    ];
    if da["spec"]["crop"] != db["spec"]["crop"] {
        return tr!("裁切", "Crop").into();
    }
    if let Some((_, zh, en)) = names.iter().find(|(k, _, _)| changed(k)) {
        return tr!(*zh, *en).into();
    }
    // 截圖的外觀與輸出（快照的第二項）
    if a[1] != b[1] {
        return tr!("外觀與輸出", "Look and output").into();
    }
    tr!("修改", "Change").into()
}

fn count(v: &Value, k: &str) -> usize {
    v.get(k).and_then(Value::as_array).map_or(0, Vec::len)
}

fn ranges(v: &Value, dur: f64) -> Vec<(f64, f64)> {
    let list: Vec<(f64, f64)> = v.as_array().map(|a| a.iter().filter_map(|r| Some((r[0].as_f64()?, r[1].as_f64()?))).collect()).unwrap_or_default();
    normalize_ranges(&list, dur)
}

/// more 裡第一段不在 less 裡的（合併過的片段比對不到時取有重疊的那段）
fn first_new(more: &[(f64, f64)], less: &[(f64, f64)]) -> Option<(f64, f64)> {
    let same = |a: &(f64, f64), b: &(f64, f64)| (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01;
    more.iter().find(|r| !less.iter().any(|l| same(r, l))).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(start: f64, end: f64, removed: &str, anns: usize) -> String {
        let anns: Vec<Value> = (0..anns).map(|i| serde_json::json!({ "id": i })).collect();
        format!(r#"[{{"duration":12.0,"spec":{{"start":{start},"end":{end},"removed":{removed}}},"anns":{}}},null]"#, Value::Array(anns))
    }

    #[test]
    fn says_what_changed() {
        let base = snap(0.0, 12.0, "[]", 0);
        assert!(describe(&base, &snap(0.0, 12.0, "[[3.0,5.0]]", 0)).contains("0:03"));
        assert!(describe(&snap(0.0, 12.0, "[[3.0,5.0]]", 0), &base).contains("0:05"));
        assert!(describe(&base, &snap(2.0, 12.0, "[]", 0)).contains("0:02"));
        assert!(describe(&base, &snap(0.0, 10.0, "[]", 0)).contains("0:10"));
        assert_ne!(describe(&base, &snap(0.0, 12.0, "[]", 1)), describe(&snap(0.0, 12.0, "[]", 1), &base));
        assert_eq!(describe("x", "y"), describe(&base, &base));
    }
}
