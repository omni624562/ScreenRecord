//! 影片資訊（標題、作者、說明…）：讀出、改寫 MP4 裡的 metadata。只複製串流不重新壓縮，
//! 封面、章節、各音軌的名稱照樣保留，修改時間不變；檔案總管「內容 → 詳細資料」看到的就是這些。

use crate::{tr, Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// 一個欄位：FFmpeg 的名稱（MP4 裡對應的標籤）與畫面上的說明
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// 多行文字
    pub multiline: bool,
}

/// 可以改的欄位（依序顯示）。title ©nam、artist ©ART、album ©alb、genre ©gen、date ©day、comment ©cmt、copyright cprt
pub fn fields() -> [Field; 7] {
    [
        Field { key: "title", label: tr!("標題", "Title"), hint: tr!("例如：報價單怎麼填", "e.g. How to fill in a quote"), multiline: false },
        Field { key: "artist", label: tr!("作者", "Author"), hint: tr!("例如：業務部 王小明", "e.g. Sales team, Alex Wang"), multiline: false },
        Field { key: "album", label: tr!("系列", "Series"), hint: tr!("例如：新人教育訓練", "e.g. New hire training"), multiline: false },
        Field { key: "genre", label: tr!("類型", "Genre"), hint: tr!("例如：教學", "e.g. Tutorial"), multiline: false },
        Field { key: "date", label: tr!("年份", "Year"), hint: tr!("例如：2026", "e.g. 2026"), multiline: false },
        Field { key: "comment", label: tr!("說明", "Description"), hint: tr!("這支影片在講什麼（可以換行）", "What this video is about (can be several lines)"), multiline: true },
        Field { key: "copyright", label: tr!("版權", "Copyright"), hint: tr!("例如：© 2026 某某公司", "e.g. © 2026 Example Inc."), multiline: false },
    ]
}

/// 可以改影片資訊的檔案（MP4 一類，含只有聲音的 M4A）
pub fn can_tag(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".mp4", ".m4v", ".mov", ".m4a"].iter().any(|e| lower.ends_with(e))
}

/// 讀出影片目前的資訊（只有 fields() 裡的欄位；沒有的是空字串）
pub async fn read(ffmpeg: &Path, video: &str) -> Result<HashMap<String, String>> {
    let args = ["-hide_banner", "-loglevel", "error", "-i", video, "-f", "ffmetadata", "-"];
    let r = crate::process::run(ffmpeg, &args, Duration::from_secs(30)).await;
    if r.code != 0 {
        crate::info!("[影片資訊] 讀取 {video} 失敗：{}", r.stderr.trim());
        return Err(Error::other(tr!("讀不到這支影片的資訊", "Couldn't read this video's details")));
    }
    // MKV、WebM 存成大寫（ARTIST、COMMENT）：不分大小寫
    let all: HashMap<String, String> = parse_ffmetadata(&r.stdout).into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
    Ok(fields().iter().map(|f| (f.key.to_string(), all.get(f.key).cloned().unwrap_or_default())).collect())
}

/// 寫進影片：values 裡有的欄位換成新的內容，空字串 = 刪掉；其他資訊、封面、章節保留
pub async fn write(ffmpeg: Option<&Path>, video: &str, values: &HashMap<String, String>) -> Result<()> {
    let ff = ffmpeg.ok_or_else(|| Error::config(tr!("找不到 FFmpeg", "FFmpeg not found")))?;
    if !can_tag(video) {
        return Err(Error::config(tr!("只有 MP4 影片可以改影片資訊", "Only MP4 videos can have their details changed")));
    }
    crate::cover::rewrite_in_place(ff, video, |tmp| write_args(video, values, tmp)).await.map_err(Error::other)?;
    crate::info!("[影片資訊] 已寫進 {video}");
    Ok(())
}

/// 只複製串流：0:v 含封面圖（attached_pic 的設定跟著複製），聲音、字幕都留著；章節與其他資訊照搬
pub fn write_args(video: &str, values: &HashMap<String, String>, out: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error", "-i", video].iter().map(|s| s.to_string()).collect();
    a.extend(["-map", "0:v?", "-map", "0:a?", "-map", "0:s?", "-c", "copy", "-map_metadata", "0", "-map_chapters", "0"].iter().map(|s| s.to_string()));
    for f in fields() {
        if let Some(v) = values.get(f.key) {
            // 換行統一成 \n；單行的欄位不留換行
            let v = v.replace("\r\n", "\n");
            let v = if f.multiline { v.trim().to_string() } else { v.replace('\n', " ").trim().to_string() };
            a.push("-metadata".into());
            a.push(format!("{}={v}", f.key));
        }
    }
    a.extend(["-movflags", "+faststart", "-y", out].iter().map(|s| s.to_string()));
    a
}

/// FFmpeg 的 ffmetadata 格式：第一段（第一個 [區段] 之前）是整個檔案的資訊；
/// \ 後面的字照字面（\= \; \# \\），行尾的 \ 表示下一行接著是同一個值
pub fn parse_ffmetadata(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.starts_with('[') {
            break;
        }
        if line.starts_with(';') || line.starts_with('#') || line.is_empty() {
            continue;
        }
        let mut entry = line.to_string();
        // 行尾有單獨的 \：值還沒結束（要數 \ 的個數，\\ 是字面上的 \）
        while entry.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1 {
            entry.pop();
            entry.push('\n');
            match lines.next() {
                Some(next) => entry.push_str(next),
                None => break,
            }
        }
        let (mut key, mut val, mut in_val, mut esc) = (String::new(), String::new(), false, false);
        for c in entry.chars() {
            let target = if in_val { &mut val } else { &mut key };
            if esc {
                target.push(c);
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '=' && !in_val {
                in_val = true;
            } else {
                target.push(c);
            }
        }
        if in_val {
            out.insert(key, val);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escapes_and_multiline_values() {
        let text = ";FFMETADATA1\nmajor_brand=isom\ntitle=報價單 \\= 教學\\; \\#1\nartist=王小明\ncomment=第一行\\\n第二行\\\\\nencoder=Lavf60\n[CHAPTER]\nTIMEBASE=1/1000\ntitle=第一段\n";
        let m = parse_ffmetadata(text);
        assert_eq!(m["title"], "報價單 = 教學; #1");
        assert_eq!(m["artist"], "王小明");
        assert_eq!(m["comment"], "第一行\n第二行\\");
        assert_eq!(m["encoder"], "Lavf60");
        assert_eq!(m.get("TIMEBASE"), None, "章節的內容不算");
        assert_eq!(m.len(), 5);
        // MKV、WebM 的欄位是大寫
        assert_eq!(parse_ffmetadata(";FFMETADATA1\nARTIST=王小明\n")["ARTIST"], "王小明");
    }

    #[test]
    fn write_args_copy_streams_and_clear_empty_fields() {
        let values: HashMap<String, String> = [("title", " 報價單\n怎麼填 "), ("comment", "第一行\r\n第二行\n"), ("artist", "")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let a = write_args("in.mp4", &values, "out.mp4");
        let j = a.join(" ");
        assert!(j.contains("-map 0:v? -map 0:a? -map 0:s? -c copy -map_metadata 0 -map_chapters 0"), "{j}");
        assert!(a.contains(&"title=報價單 怎麼填".to_string()), "{a:?}");
        assert!(a.contains(&"comment=第一行\n第二行".to_string()), "{a:?}");
        assert!(a.contains(&"artist=".to_string()), "空的 = 刪掉");
        assert!(!j.contains("album="), "沒給的欄位不動");
        assert!(a.ends_with(&["-y".to_string(), "out.mp4".to_string()]));
    }

    #[test]
    fn only_mp4_family_can_be_tagged() {
        assert!(can_tag(r"C:\v\a.MP4") && can_tag("a.m4a") && can_tag("a.mov"));
        assert!(!can_tag("a.mkv") && !can_tag("a.gif") && !can_tag("a.png"));
    }
}
