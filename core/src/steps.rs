//! 步驟截圖：開始後每點一下滑鼠就截一張（標出點的位置），完成後先在操作視窗整理（刪掉、調整順序），
//! 再做成一份教學文件（HTML）：每個步驟一段說明與一張圖，圖片都包在同一個檔案裡（單獨傳給別人也看得到）。
//! 說明可以直接在瀏覽器裡修改，再列印成 PDF 或複製到 Word。

use crate::{tr, trf};
use std::path::{Path, PathBuf};

/// 一個步驟
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 點的時間（時:分:秒）
    pub time: String,
    /// 點到的視窗標題
    pub window: String,
    /// 圖檔（相對於儲存資料夾的路徑：資料夾/step_NN.png）
    pub image: String,
}

/// 截好、還沒做成文件的步驟（在操作視窗整理）
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    /// 儲存資料夾（文件放這裡）
    pub dir: PathBuf,
    /// 文件名稱（教學_日期_時間）；截圖暫時放在同名的資料夾
    pub name: String,
    /// 開始的時間（文件標題與日期用）
    pub date: String,
    pub steps: Vec<Step>,
}

impl Draft {
    /// 放截圖的資料夾
    pub fn folder(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    pub fn image_path(&self, s: &Step) -> PathBuf {
        self.dir.join(&s.image)
    }

    /// 預設的文件標題
    pub fn title(&self) -> String {
        trf!("操作步驟（{}）", "Steps ({})", self.date)
    }

    /// 刪掉截圖（取消、或已經做成文件）
    pub fn discard(&self) {
        if !self.name.is_empty() {
            let _ = std::fs::remove_dir_all(self.folder());
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 步驟的說明：「在「視窗」點一下」
pub fn caption(window: &str) -> String {
    let w = window.trim();
    if w.is_empty() {
        tr!("點一下", "Click").into()
    } else {
        trf!("在「{w}」點一下", "Click in “{w}”")
    }
}

/// 教學文件（HTML）；srcs = 每一步的圖（data: 網址或路徑）
pub fn html(title: &str, date: &str, steps: &[Step], srcs: &[String]) -> String {
    let mut items = String::new();
    for (i, (s, src)) in steps.iter().zip(srcs).enumerate() {
        items.push_str(&trf!(
            "<li><p class=\"cap\" contenteditable=\"true\"><b>步驟 {}</b>　{}</p><p class=\"time\">{}</p><img src=\"{}\" alt=\"步驟 {}\"></li>\n",
            "<li><p class=\"cap\" contenteditable=\"true\"><b>Step {}</b> {}</p><p class=\"time\">{}</p><img src=\"{}\" alt=\"Step {}\"></li>\n",
            i + 1,
            esc(&caption(&s.window)),
            esc(&s.time),
            esc(src),
            i + 1
        ));
    }
    let (d, n) = (esc(date), steps.len());
    let meta = if crate::i18n::is_en() { format!("{d} · {n} step{}", if n == 1 { "" } else { "s" }) } else { format!("{d}・共 {n} 個步驟") };
    let tip = tr!(
        "文字可以直接點一下修改（例如補上要輸入什麼）。在瀏覽器裡改的文字不會自動存檔：改好後用瀏覽器的「列印」存成 PDF，或全選複製貼到 Word。圖片都包在這個檔案裡，單獨傳給別人也看得到。",
        "Click any text to edit it (e.g. add what to type). Changes made in the browser aren't saved automatically: when you're done, use the browser's “Print” to save it as a PDF, or select all and paste it into Word. The pictures are inside this file, so you can send it on its own."
    );
    format!(
        r#"<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{t}</title>
<style>
body {{ font-family: "Microsoft JhengHei UI", "Noto Sans TC", sans-serif; max-width: 1100px; margin: 32px auto; padding: 0 20px; color: #1f2328; background: #fff; }}
h1 {{ font-size: 26px; margin: 0 0 6px; }}
.meta {{ color: #6b7280; margin: 0 0 24px; }}
.tip {{ background: #eef6ff; border: 1px solid #c7defa; border-radius: 8px; padding: 10px 14px; color: #1d4f91; }}
ol {{ list-style: none; padding: 0; }}
li {{ margin: 0 0 36px; page-break-inside: avoid; }}
.cap {{ font-size: 18px; margin: 0 0 4px; outline: none; }}
.cap:focus {{ background: #fff8d6; }}
.time {{ color: #9aa1ab; font-size: 13px; margin: 0 0 8px; }}
img {{ max-width: 100%; border: 1px solid #d0d4da; border-radius: 6px; }}
@media print {{ .tip {{ display: none; }} body {{ margin: 0; }} }}
</style>
</head>
<body>
<h1 contenteditable="true">{t}</h1>
<p class="meta">{meta}</p>
<p class="tip">{tip}</p>
<ol>
{items}</ol>
</body>
</html>
"#,
        lang = tr!("zh-Hant-TW", "en"),
        t = esc(title),
    )
}

/// 截圖時浮在畫面下方的小工具列：按鈕編號
pub const BAR_FINISH: u32 = 1;
pub const BAR_CANCEL: u32 = 2;
pub const BAR_DISCARD: u32 = 3;
pub const BAR_KEEP: u32 = 4;

/// 小工具列的內容：「步驟截圖中・已截 N 步 [完成] [取消]」；按了取消（已經有步驟）時先問要不要放棄
pub fn bar(count: u32, confirm: bool) -> crate::bubble::Content {
    let en = crate::i18n::is_en();
    if confirm {
        let q = if en { format!("Discard the {count} captured step{}?", if count == 1 { "" } else { "s" }) } else { format!("要放棄已截的 {count} 步嗎？") };
        return crate::bubble::Content {
            big: None,
            lines: vec![q, tr!("截好的圖會刪掉，不會做成文件", "The screenshots will be deleted").into()],
            buttons: vec![(BAR_KEEP, tr!("繼續截圖", "Keep capturing").into(), true), (BAR_DISCARD, tr!("放棄", "Discard").into(), false)],
        };
    }
    let head = if en { format!("Capturing steps · {count} captured") } else { format!("步驟截圖中・已截 {count} 步") };
    crate::bubble::Content {
        big: None,
        lines: vec![head, tr!("每點一下滑鼠就截一張", "Each mouse click takes a screenshot").into()],
        buttons: vec![(BAR_FINISH, tr!("完成", "Finish").into(), true), (BAR_CANCEL, tr!("取消", "Cancel").into(), false)],
    }
}

/// 圖片太寬（4K 螢幕）時縮到這個寬度再放進文件：檔案小一點，瀏覽器裡看起來一樣
const DOC_MAX_W: u32 = 1920;

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// 一步的圖放進文件：data: 網址（PNG）
fn data_uri(path: &Path) -> Result<String, String> {
    Ok(format!("data:image/png;base64,{}", base64(&doc_png(path)?)))
}

/// 放進文件的 PNG：太寬時縮小
fn doc_png(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let wide = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()).is_some_and(|(w, _)| w > DOC_MAX_W);
    Ok(if wide {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let img = img.resize(DOC_MAX_W, u32::MAX, image::imageops::FilterType::Triangle);
        let mut out = std::io::Cursor::new(vec![]);
        img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        out.into_inner()
    } else {
        bytes
    })
}

/// 教學文件的格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocFormat {
    /// 一個 HTML 檔（圖片包在裡面，用瀏覽器開啟、可以直接改文字）
    #[default]
    Html,
    /// Word（.docx）
    Word,
    /// PowerPoint（.pptx）：一步一張投影片
    PowerPoint,
}

impl DocFormat {
    fn ext(self) -> &'static str {
        match self {
            DocFormat::Html => "html",
            DocFormat::Word => "docx",
            DocFormat::PowerPoint => "pptx",
        }
    }
}

/// 做成 Word 或 PowerPoint 的內容
fn office(draft: &Draft, title: &str, steps: &[Step], format: DocFormat) -> Result<Vec<u8>, String> {
    let mut items = Vec::with_capacity(steps.len());
    for s in steps {
        let path = draft.image_path(s);
        let png = std::fs::read(&path).map_err(|e| trf!("無法讀取「{}」：{e}", "Couldn't read “{}”: {e}", s.image))?;
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()).unwrap_or((1280, 720));
        items.push(crate::ooxml::Item { caption: caption(&s.window), png, width, height });
    }
    Ok(match format {
        DocFormat::PowerPoint => crate::ooxml::pptx(title, &draft.date, &items),
        _ => crate::ooxml::docx(title, &draft.date, &items),
    })
}

/// 做成教學文件：steps 是整理後（刪掉、調整順序）的步驟；寫成 儲存資料夾/文件名稱.html（已經有同名檔案時加上編號），
/// 成功後刪掉放截圖的資料夾。回傳文件的路徑。
pub fn build(draft: &Draft, title: &str, steps: &[Step]) -> Result<PathBuf, String> {
    build_as(draft, title, steps, DocFormat::Html)
}

/// 同上，指定格式（HTML、Word、PowerPoint）
pub fn build_as(draft: &Draft, title: &str, steps: &[Step], format: DocFormat) -> Result<PathBuf, String> {
    let title = if title.trim().is_empty() { draft.title() } else { title.trim().to_string() };
    let doc: Vec<u8> = match format {
        DocFormat::Html => {
            let srcs = steps.iter().map(|s| data_uri(&draft.image_path(s)).map_err(|e| trf!("無法讀取「{}」：{e}", "Couldn't read “{}”: {e}", s.image))).collect::<Result<Vec<_>, _>>()?;
            html(&title, &draft.date, steps, &srcs).into_bytes()
        }
        _ => office(draft, &title, steps, format)?,
    };
    let ext = format.ext();
    let mut path = draft.dir.join(format!("{}.{ext}", draft.name));
    let mut n = 2;
    while path.exists() {
        path = draft.dir.join(format!("{} ({n}).{ext}", draft.name));
        n += 1;
    }
    let tmp = path.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, doc).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        trf!("無法儲存教學文件：{e}", "Couldn't save the step-by-step guide: {e}")
    })?;
    draft.discard();
    Ok(path)
}

/// 整理步驟時的小圖（未預乘 RGBA）：長邊縮到 max 以內
pub fn thumb(path: &Path, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?.thumbnail(max, max).to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_document() {
        let steps = vec![
            Step { time: "14:30:05".into(), window: "設定 <進階>".into(), image: "教學_1/step_01.png".into() },
            Step { time: "14:30:09".into(), window: " ".into(), image: "教學_1/step_02.png".into() },
        ];
        let h = html("登入流程", "2026/10/10", &steps, &["data:image/png;base64,AAAA".into(), "b.png".into()]);
        assert!(h.contains("<title>登入流程</title>"));
        assert!(h.contains("共 2 個步驟"));
        assert!(h.contains("<b>步驟 1</b>　在「設定 &lt;進階&gt;」點一下"));
        assert!(h.contains("<b>步驟 2</b>　點一下"));
        assert!(h.contains("src=\"data:image/png;base64,AAAA\""));
        assert!(h.contains("contenteditable=\"true\""));
        assert!(h.contains("在瀏覽器裡改的文字不會自動存檔"));
    }

    #[test]
    fn bar_asks_before_discarding() {
        let b = bar(3, false);
        assert_eq!(b.lines[0], "步驟截圖中・已截 3 步");
        assert_eq!(b.buttons.iter().map(|b| b.0).collect::<Vec<_>>(), [BAR_FINISH, BAR_CANCEL]);
        let b = bar(3, true);
        assert_eq!(b.lines[0], "要放棄已截的 3 步嗎？");
        assert_eq!(b.buttons.iter().map(|b| b.0).collect::<Vec<_>>(), [BAR_KEEP, BAR_DISCARD]);
        assert!(crate::bubble::render(&b, 1.0, None).is_some());
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn builds_one_file_and_removes_the_pictures() {
        let dir = std::env::temp_dir().join(format!("steps-test-{}", std::process::id()));
        let draft = Draft { dir: dir.clone(), name: "教學_測試".into(), date: "2026/10/13 09:00".into(), steps: vec![] };
        std::fs::create_dir_all(draft.folder()).unwrap();
        let mut steps = vec![];
        for (i, w) in [(1, 40), (2, 2400)] {
            let img = image::RgbaImage::from_pixel(w, 30, image::Rgba([200, 30, 30, 255]));
            img.save(draft.folder().join(format!("step_0{i}.png"))).unwrap();
            steps.push(Step { time: format!("09:00:0{i}"), window: "記事本".into(), image: format!("教學_測試/step_0{i}.png") });
        }
        let t = thumb(&draft.image_path(&steps[1]), 240).unwrap();
        assert_eq!((t.0, t.1, t.2.len()), (240, 3, 240 * 3 * 4));
        // 太寬的圖縮到 1920 再放進文件
        let png = doc_png(&draft.image_path(&steps[1])).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().width(), 1920);
        assert_eq!(doc_png(&draft.image_path(&steps[0])).unwrap(), std::fs::read(draft.image_path(&steps[0])).unwrap());
        // 只留第 2 步（順序、刪除由介面決定）
        let path = build(&draft, " ", &steps[1..]).unwrap();
        assert_eq!(path, dir.join("教學_測試.html"));
        let h = std::fs::read_to_string(&path).unwrap();
        assert!(h.contains("<title>操作步驟（2026/10/13 09:00）</title>") && h.contains("共 1 個步驟"));
        assert_eq!(h.matches("src=\"data:image/png;base64,").count(), 1);
        assert!(!draft.folder().exists());
        // 同名的文件已經有了：加上編號
        std::fs::create_dir_all(draft.folder()).unwrap();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255])).save(draft.folder().join("step_01.png")).unwrap();
        assert_eq!(build(&draft, "標題", &steps[..1]).unwrap(), dir.join("教學_測試 (2).html"));
        // 圖不見了：說明哪一張
        let err = build(&draft, "標題", &steps[..1]).unwrap_err();
        assert!(err.contains("step_01.png"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
