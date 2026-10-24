//! Word（.docx）與 PowerPoint（.pptx）：步驟教學另存成 Office 檔案。
//! 兩種都是 ZIP 包著 XML；這裡只寫需要的最少內容（標題、每一步的說明與圖），圖片原樣放進去（PNG 本來就壓縮過，ZIP 不再壓縮）。

/// 一頁 / 一段：說明與 PNG 圖（寬、高是像素）
pub struct Item {
    pub caption: String,
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

// ───────────── ZIP（不壓縮） ─────────────

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
        let mut t = [0u32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *v = c;
        }
        t
    });
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c = TABLE[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// 把檔案（路徑, 內容）包成 ZIP（stored）
pub fn zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    // 2026-01-01 00:00（DOS 日期時間）
    let (time, date) = (0u16, ((2026 - 1980) << 9 | 1 << 5 | 1) as u16);
    let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let n = name.as_bytes();
        le32(&mut out, 0x0403_4b50);
        le16(&mut out, 20);
        le16(&mut out, 0x0800);
        le16(&mut out, 0);
        le16(&mut out, time);
        le16(&mut out, date);
        le32(&mut out, crc);
        le32(&mut out, data.len() as u32);
        le32(&mut out, data.len() as u32);
        le16(&mut out, n.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(n);
        out.extend_from_slice(data);
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, 20);
        le16(&mut central, 20);
        le16(&mut central, 0x0800);
        le16(&mut central, 0);
        le16(&mut central, time);
        le16(&mut central, date);
        le32(&mut central, crc);
        le32(&mut central, data.len() as u32);
        le32(&mut central, data.len() as u32);
        le16(&mut central, n.len() as u16);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, 0);
        le32(&mut central, offset);
        central.extend_from_slice(n);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    le32(&mut out, 0x0605_4b50);
    le16(&mut out, 0);
    le16(&mut out, 0);
    le16(&mut out, files.len() as u16);
    le16(&mut out, files.len() as u16);
    le32(&mut out, cd_size);
    le32(&mut out, cd_offset);
    le16(&mut out, 0);
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 圖放進 max_w × max_h（EMU）以內、維持比例
fn fit(w: u32, h: u32, max_w: u64, max_h: u64) -> (u64, u64) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let s = (max_w as f64 / w).min(max_h as f64 / h);
    ((w * s) as u64, (h * s) as u64)
}

const XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#;
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

// ───────────── Word ─────────────

fn w_text(text: &str, size_half_pt: u32, bold: bool, color: &str) -> String {
    let b = if bold { "<w:b/>" } else { "" };
    format!(
        r#"<w:p><w:pPr><w:spacing w:before="240" w:after="120"/></w:pPr><w:r><w:rPr><w:rFonts w:ascii="Segoe UI" w:hAnsi="Segoe UI" w:eastAsia="Microsoft JhengHei"/>{b}<w:color w:val="{color}"/><w:sz w:val="{size_half_pt}"/><w:szCs w:val="{size_half_pt}"/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
        esc(text)
    )
}

fn w_image(id: usize, rid: &str, cx: u64, cy: u64) -> String {
    format!(
        r#"<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="{id}" name="Picture {id}"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="{id}" name="image{id}.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    )
}

/// Word 文件：標題、日期，每一步一段說明（步驟 N：…）加一張圖（寬度放滿版面）
pub fn docx(title: &str, subtitle: &str, items: &[Item]) -> Vec<u8> {
    let mut body = String::new();
    body.push_str(&w_text(title, 40, true, "1F2328"));
    if !subtitle.is_empty() {
        body.push_str(&w_text(subtitle, 22, false, "6E7781"));
    }
    let mut rels = String::new();
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    for (i, it) in items.iter().enumerate() {
        let n = i + 1;
        body.push_str(&w_text(&crate::trf!("步驟 {n}：{}", "Step {n}: {}", it.caption), 26, true, "1F2328"));
        // 版面寬 6.3 英吋（A4 預設邊界），高最多 8 英吋
        let (cx, cy) = fit(it.width, it.height, 5_760_720, 7_315_200);
        body.push_str(&w_image(n, &format!("rIdImg{n}"), cx, cy));
        rels.push_str(&format!(r#"<Relationship Id="rIdImg{n}" Type="{REL_IMAGE}" Target="media/image{n}.png"/>"#));
        files.push((format!("word/media/image{n}.png"), it.png.clone()));
    }
    let document = format!(
        r#"{XML}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="{NS_R}" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let mut all = vec![
        (
            "[Content_Types].xml".to_string(),
            format!(r#"{XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).into_bytes(),
        ),
        (
            "_rels/.rels".to_string(),
            format!(r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).into_bytes(),
        ),
        ("word/document.xml".to_string(), document.into_bytes()),
        ("word/_rels/document.xml.rels".to_string(), format!(r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#).into_bytes()),
    ];
    all.extend(files);
    zip(&all)
}

/// 只有文字的 Word 文件（逐字稿）：標題、副標，每段前面可以有灰色的時間標記
pub fn docx_text(title: &str, subtitle: &str, paras: &[(Option<String>, String)]) -> Vec<u8> {
    let mut body = String::new();
    body.push_str(&w_text(title, 40, true, "1F2328"));
    if !subtitle.is_empty() {
        body.push_str(&w_text(subtitle, 22, false, "6E7781"));
    }
    let run = |text: &str, color: &str| {
        format!(
            r#"<w:r><w:rPr><w:rFonts w:ascii="Segoe UI" w:hAnsi="Segoe UI" w:eastAsia="Microsoft JhengHei"/><w:color w:val="{color}"/><w:sz w:val="24"/><w:szCs w:val="24"/></w:rPr><w:t xml:space="preserve">{}</w:t></w:r>"#,
            esc(text)
        )
    };
    for (time, text) in paras {
        let t = time.as_ref().map(|t| run(&format!("{t} "), "6E7781")).unwrap_or_default();
        body.push_str(&format!(r#"<w:p><w:pPr><w:spacing w:before="120" w:after="120" w:line="360" w:lineRule="auto"/></w:pPr>{t}{}</w:p>"#, run(text, "1F2328")));
    }
    let document = format!(
        r#"{XML}<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="{NS_R}"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    zip(&[
        (
            "[Content_Types].xml".to_string(),
            format!(r#"{XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).into_bytes(),
        ),
        (
            "_rels/.rels".to_string(),
            format!(r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).into_bytes(),
        ),
        ("word/document.xml".to_string(), document.into_bytes()),
    ])
}

// ───────────── PowerPoint ─────────────

const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
/// 16:9
const SLIDE_W: u64 = 12_192_000;
const SLIDE_H: u64 = 6_858_000;

#[allow(clippy::too_many_arguments)]
fn p_textbox(id: usize, x: u64, y: u64, cx: u64, cy: u64, text: &str, size_100pt: u32, bold: bool, color: &str, center: bool) -> String {
    let algn = if center { r#" algn="ctr""# } else { "" };
    let b = if bold { r#" b="1""# } else { "" };
    let anchor = if center { "ctr" } else { "t" };
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Text {id}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap="square" anchor="{anchor}"><a:normAutofit/></a:bodyPr><a:lstStyle/><a:p><a:pPr{algn}/><a:r><a:rPr lang="zh-TW" sz="{size_100pt}"{b} dirty="0"><a:solidFill><a:srgbClr val="{color}"/></a:solidFill><a:latin typeface="Segoe UI"/><a:ea typeface="Microsoft JhengHei"/></a:rPr><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp>"#,
        esc(text)
    )
}

fn p_picture(id: usize, rid: &str, x: u64, y: u64, cx: u64, cy: u64) -> String {
    format!(
        r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:ln w="9525"><a:solidFill><a:srgbClr val="D0D7DE"/></a:solidFill></a:ln></p:spPr></p:pic>"#
    )
}

fn p_slide(shapes: &str) -> String {
    format!(
        r#"{XML}<p:sld xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>{shapes}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#
    )
}

const THEME: &str = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="Segoe UI"/><a:ea typeface="Microsoft JhengHei"/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Segoe UI"/><a:ea typeface="Microsoft JhengHei"/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#;

const EMPTY_TREE: &str = r#"<p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree>"#;

/// PowerPoint 簡報（16:9）：第一張是標題，之後每一步一張（上面是說明，下面是圖）
pub fn pptx(title: &str, subtitle: &str, items: &[Item]) -> Vec<u8> {
    let rel = |id: &str, ty: &str, target: &str| format!(r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{ty}" Target="{target}"/>"#);
    let rels = |inner: String| format!(r#"{XML}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{inner}</Relationships>"#).into_bytes();
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    let mut slides: Vec<String> = vec![];
    // 標題頁
    let mut shapes = p_textbox(2, 685_800, 2_286_000, SLIDE_W - 1_371_600, 1_371_600, title, 4000, true, "1F2328", true);
    if !subtitle.is_empty() {
        shapes.push_str(&p_textbox(3, 685_800, 3_700_000, SLIDE_W - 1_371_600, 700_000, subtitle, 2000, false, "6E7781", true));
    }
    slides.push(p_slide(&shapes));
    let mut slide_rels: Vec<String> = vec![String::new()];
    for (i, it) in items.iter().enumerate() {
        let n = i + 1;
        let caption = crate::trf!("步驟 {n}：{}", "Step {n}: {}", it.caption);
        let mut s = p_textbox(2, 457_200, 228_600, SLIDE_W - 914_400, 762_000, &caption, 2400, true, "1F2328", false);
        let (max_w, max_h) = (SLIDE_W - 914_400, SLIDE_H - 1_143_000 - 228_600);
        let (cx, cy) = fit(it.width, it.height, max_w, max_h);
        let x = (SLIDE_W - cx) / 2;
        s.push_str(&p_picture(3, "rId2", x, 1_143_000, cx, cy));
        slides.push(p_slide(&s));
        slide_rels.push(rel("rId2", "image", &format!("../media/image{n}.png")));
        files.push((format!("ppt/media/image{n}.png"), it.png.clone()));
    }
    let mut overrides = String::new();
    let mut pres_rels = rel("rId1", "slideMaster", "slideMasters/slideMaster1.xml") + &rel("rId2", "theme", "theme/theme1.xml");
    let mut sld_ids = String::new();
    for (i, (s, r)) in slides.iter().zip(&slide_rels).enumerate() {
        let n = i + 1;
        files.push((format!("ppt/slides/slide{n}.xml"), s.clone().into_bytes()));
        files.push((format!("ppt/slides/_rels/slide{n}.xml.rels"), rels(rel("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml") + r)));
        overrides.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#));
        pres_rels.push_str(&rel(&format!("rIdS{n}"), "slide", &format!("slides/slide{n}.xml")));
        sld_ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rIdS{n}"/>"#, 255 + n));
    }
    let presentation = format!(
        r#"{XML}<p:presentation xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{sld_ids}</p:sldIdLst><p:sldSz cx="{SLIDE_W}" cy="{SLIDE_H}"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    );
    let master = format!(
        r#"{XML}<p:sldMaster xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}"><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:bgPr></p:bg>{EMPTY_TREE}</p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst></p:sldMaster>"#
    );
    let layout = format!(
        r#"{XML}<p:sldLayout xmlns:a="{NS_A}" xmlns:r="{NS_R}" xmlns:p="{NS_P}" type="blank" preserve="1"><p:cSld name="Blank">{EMPTY_TREE}</p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#
    );
    let mut all: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".into(),
            format!(
                r#"{XML}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>{overrides}</Types>"#
            )
            .into_bytes(),
        ),
        ("_rels/.rels".into(), rels(rel("rId1", "officeDocument", "ppt/presentation.xml"))),
        ("ppt/presentation.xml".into(), presentation.into_bytes()),
        ("ppt/_rels/presentation.xml.rels".into(), rels(pres_rels)),
        ("ppt/slideMasters/slideMaster1.xml".into(), master.into_bytes()),
        ("ppt/slideMasters/_rels/slideMaster1.xml.rels".into(), rels(rel("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml") + &rel("rId2", "theme", "../theme/theme1.xml"))),
        ("ppt/slideLayouts/slideLayout1.xml".into(), layout.into_bytes()),
        ("ppt/slideLayouts/_rels/slideLayout1.xml.rels".into(), rels(rel("rId1", "slideMaster", "../slideMasters/slideMaster1.xml"))),
        ("ppt/theme/theme1.xml".into(), format!("{XML}{THEME}").into_bytes()),
    ];
    all.extend(files);
    zip(&all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn zip_has_entries_and_end_record() {
        let z = zip(&[("a.txt".into(), b"hello".to_vec()), ("b/c.xml".into(), b"<x/>".to_vec())]);
        assert_eq!(&z[..4], &[0x50, 0x4b, 0x03, 0x04]);
        let end = &z[z.len() - 22..];
        assert_eq!(&end[..4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([end[10], end[11]]), 2);
    }

    #[test]
    fn fit_keeps_the_ratio() {
        assert_eq!(fit(1920, 1080, 1920, 1920), (1920, 1080));
        assert_eq!(fit(1000, 2000, 500, 500), (250, 500));
    }

    /// 有 Python（python-docx、python-pptx）時：寫出檔案，用它們開啟檢查
    #[test]
    fn office_files_open_in_python() {
        let ok = std::process::Command::new("python3").args(["-c", "import docx, pptx"]).output().is_ok_and(|o| o.status.success());
        if !ok {
            return;
        }
        let mut pm = tiny_skia::Pixmap::new(320, 200).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(30, 120, 200, 255));
        let png = pm.encode_png().unwrap();
        let items = vec![Item { caption: "在「設定」按一下".into(), png: png.clone(), width: 320, height: 200 }, Item { caption: "Click <OK> & done".into(), png, width: 320, height: 200 }];
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("a.docx");
        let p = dir.path().join("a.pptx");
        std::fs::write(&d, docx("操作步驟", "2026/10/15", &items)).unwrap();
        std::fs::write(&p, pptx("操作步驟", "2026/10/15", &items)).unwrap();
        let script = format!(
            "import docx, pptx\nd = docx.Document(r'{}')\nt = [x.text for x in d.paragraphs]\nassert t[0] == '操作步驟', t\nassert any('在「設定」按一下' in x for x in t), t\nassert len(d.inline_shapes) == 2\np = pptx.Presentation(r'{}')\nassert len(p.slides) == 3\nassert any('Click <OK> & done' in s.text_frame.text for s in p.slides[2].shapes if s.has_text_frame)\nprint('ok')",
            d.display(),
            p.display()
        );
        // 開發時看看長怎樣：OOXML_OUT=資料夾 會留一份
        if let Some(o) = std::env::var_os("OOXML_OUT") {
            let _ = std::fs::copy(&d, std::path::Path::new(&o).join("a.docx"));
            let _ = std::fs::copy(&p, std::path::Path::new(&o).join("a.pptx"));
        }
        let out = std::process::Command::new("python3").args(["-c", &script]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
}
