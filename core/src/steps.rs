//! 步驟截圖：開始後每點一下滑鼠就截一張（標出點的位置），完成時做成一份教學文件（HTML），
//! 每個步驟一段說明與一張圖；說明可以直接在瀏覽器裡修改，再列印成 PDF 或複製到 Word。

use crate::{tr, trf};

/// 一個步驟
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 點的時間（時:分:秒）
    pub time: String,
    /// 點到的視窗標題
    pub window: String,
    /// 圖檔（相對於文件的路徑）
    pub image: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 圖片路徑：每一段分開編碼（空白、中文、# 等）
fn url(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
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

/// 教學文件（HTML，圖片放在旁邊的資料夾）
pub fn html(title: &str, date: &str, steps: &[Step]) -> String {
    let mut items = String::new();
    for (i, s) in steps.iter().enumerate() {
        items.push_str(&trf!(
            "<li><p class=\"cap\" contenteditable=\"true\"><b>步驟 {}</b>　{}</p><p class=\"time\">{}</p><img src=\"{}\" alt=\"步驟 {}\"></li>\n",
            "<li><p class=\"cap\" contenteditable=\"true\"><b>Step {}</b> {}</p><p class=\"time\">{}</p><img src=\"{}\" alt=\"Step {}\"></li>\n",
            i + 1,
            esc(&caption(&s.window)),
            esc(&s.time),
            esc(&url(&s.image)),
            i + 1
        ));
    }
    let (d, n) = (esc(date), steps.len());
    let meta = if crate::i18n::is_en() { format!("{d} · {n} step{}", if n == 1 { "" } else { "s" }) } else { format!("{d}・共 {n} 個步驟") };
    let tip = tr!(
        "文字可以直接點一下修改（例如補上要輸入什麼）；改好後用瀏覽器的「列印」存成 PDF，或全選複製貼到 Word。",
        "Click any text to edit it (e.g. add what to type). When you're done, use the browser's “Print” to save it as a PDF, or select all and paste it into Word."
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_document() {
        let steps = vec![
            Step { time: "14:30:05".into(), window: "設定 <進階>".into(), image: "教學_1/step_01.png".into() },
            Step { time: "14:30:09".into(), window: " ".into(), image: "教學_1/step_02.png".into() },
        ];
        let h = html("登入流程", "2026/10/10", &steps);
        assert!(h.contains("<title>登入流程</title>"));
        assert!(h.contains("共 2 個步驟"));
        assert!(h.contains("<b>步驟 1</b>　在「設定 &lt;進階&gt;」點一下"));
        assert!(h.contains("<b>步驟 2</b>　點一下"));
        // 中文路徑編碼
        assert!(h.contains("src=\"%E6%95%99%E5%AD%B8_1/step_01.png\""));
        assert!(h.contains("contenteditable=\"true\""));
    }
}
