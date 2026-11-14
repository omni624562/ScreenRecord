//! 相片裡的人：在這台電腦上找出相片裡的臉，把同一個人的臉分成一組（什麼都不上傳）。
//!
//! - 模型：OpenCV 的 YuNet（找臉，MIT）與 SFace（把一張臉變成 128 個數字，Apache-2.0），從 opencv_zoo 固定的版本下載；
//!   檔案大小與 SHA-256 寫在程式裡（不從網路上讀），對不上就不用，每次載入前也再算一次
//! - 用純 Rust 的 tract 執行（不用另外下載 DLL）：模型裡只有標準運算與數字
//! - 找臉：相片縮到最長 640 放進 YuNet，得到臉的位置與 5 個特徵點（兩眼、鼻尖、兩個嘴角）；
//!   用特徵點把臉轉正、縮成 112×112（和 OpenCV 的 FaceRecognizerSF::alignCrop 一樣），SFace 算出 128 個數字
//! - 臉的資料存在 photo-faces.json：每張相片的臉（位置、128 個數字壓成 128 bytes）、人物（名稱）
//! - 分組：每個人的臉平均起來當代表；新的臉和最像的那個人夠像就加進去，不然自成一組；很像的兩組（沒取名的）合在一起。
//!   使用者改過的（移到某人、不是這個人）不再自動改
//! - 閉眼、不是臉：Google MediaPipe 的臉部特徵點模型（478 個點、「是不是臉」的分數）與表情模型（52 個表情分數，用左右眼眨眼），
//!   Apache-2.0；官方是 TFLite 檔，tract 跑不了，所以由這個專案轉成 ONNX 放在 models/faces（來源、轉法、檢查結果見那裡的 README）。
//!   「是不是臉」的分數太低的不算（貓狗、雜訊）；夠大的臉才看眼睛（小臉笑起來也會被當成閉眼）

use crate::paths::{data_dir, mtime_ms};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tract_onnx::prelude::*;

/// 一個模型檔：檔名、下載網址（固定的 commit）、大小、SHA-256
pub struct ModelFile {
    pub file: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub const YUNET: ModelFile = ModelFile {
    file: "face_detection_yunet_2023mar.onnx",
    url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/48b10fa505aeb51fbd127fc60a5c660040e823b7/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
    size: 232_589,
    sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
};

pub const SFACE: ModelFile = ModelFile {
    file: "face_recognition_sface_2021dec.onnx",
    url: "https://media.githubusercontent.com/media/opencv/opencv_zoo/48b10fa505aeb51fbd127fc60a5c660040e823b7/models/face_recognition_sface/face_recognition_sface_2021dec.onnx",
    size: 38_696_353,
    sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
};

/// MediaPipe 的臉部特徵點模型（這個專案從官方的 face_landmarker.task 轉成 ONNX）
pub const LANDMARKS: ModelFile = ModelFile {
    file: "face_landmarks_detector.onnx",
    url: "https://raw.githubusercontent.com/omni624562/ScreenRecord/f9c3465d7a7d13278593fd89305edb1239074828/models/faces/face_landmarks_detector.onnx",
    size: 4_920_997,
    sha256: "cb6a76e380ff26a372957ebf69fbd523ce8a49266b882263868e1a333ed7d09a",
};

/// MediaPipe 的表情模型（同上）
pub const BLENDSHAPES: ModelFile = ModelFile {
    file: "face_blendshapes.onnx",
    url: "https://raw.githubusercontent.com/omni624562/ScreenRecord/f9c3465d7a7d13278593fd89305edb1239074828/models/faces/face_blendshapes.onnx",
    size: 1_883_548,
    sha256: "baf1761013b93e3b4f55af456a80d74aa089a2fc377f5f8044ae48d0cc6a47f2",
};

pub const MODELS: [&ModelFile; 4] = [&YUNET, &SFACE, &LANDMARKS, &BLENDSHAPES];
/// 全部加起來約幾 MB
pub const MB: u32 = 46;

/// YuNet 的輸入大小（固定 640×640）
const DET: usize = 640;
/// 找臉時相片縮到最長多少（臉轉正用這張，比 640 清楚）
pub const ANALYZE_MAX: u32 = 1280;
/// 臉要多大才拿來認人（分析用的相片裡，短邊像素）：太小的臉算出來的數字不準
const MIN_FACE: f32 = 24.0;
/// 找臉的門檻（YuNet 的分數）、重疊的框留一個。0.8 時貓狗的臉也會進來（測試的相片裡動物 0.55～0.80、真人 0.86 以上）
pub const SCORE: f32 = 0.88;
const NMS: f32 = 0.3;
/// 「是不是臉」的門檻（特徵點模型的 face flag；MediaPipe 預設 0.5）
const PRESENCE: f32 = 0.5;
/// 臉要多大才看眼睛（分析用的相片裡，寬度像素）：小臉在笑的時候眨眼分數也會到 0.5～0.8
const EYE_MIN: f32 = 80.0;
/// 特徵點模型的輸入：臉的四周各多留 25%（臉寬的 1.5 倍）、256×256
const LM: usize = 256;
const LM_SCALE: f32 = 1.5;
/// 資料的版本：2 = 擋過不是臉的、看過眼睛
pub const VERSION: u8 = 2;
/// 一張臉和某個人夠像（相似度）就算他；兩組沒取名的人夠像就合在一起。SFace 建議的「同一人」門檻是 0.363，
/// 分組時用嚴一點（不同的人被分在一起比同一個人分成兩組麻煩：家人長得像）
pub const ASSIGN: f32 = 0.42;
pub const MERGE: f32 = 0.5;

// ───────────── 模型檔 ─────────────

/// 模型可以放的資料夾：先找程式旁邊的 faces（整個資料夾帶著走也能用），再找 %LOCALAPPDATA%\ScreenRecorder\faces
pub fn model_dirs() -> Vec<PathBuf> {
    let mut v = vec![crate::paths::app_dir().join("faces"), data_dir().join("faces")];
    v.dedup();
    v
}

/// 下載、安裝時放的資料夾：程式旁邊能寫入就放那裡
pub fn install_dir() -> PathBuf {
    let dirs = model_dirs();
    for dir in &dirs {
        let probe = dir.join(format!(".write-test-{}", std::process::id()));
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return dir.clone();
        }
    }
    dirs.last().cloned().unwrap_or_default()
}

/// 已經有的模型檔（大小對的，程式旁邊優先）；沒有時是要安裝的位置
pub fn model_path(m: &ModelFile) -> PathBuf {
    model_dirs().into_iter().map(|d| d.join(m.file)).find(|p| std::fs::metadata(p).is_ok_and(|x| x.len() == m.size)).unwrap_or_else(|| install_dir().join(m.file))
}

/// 模型都在（大小對的；內容在載入前檢查）
pub fn models_ready() -> bool {
    MODELS.iter().all(|m| std::fs::metadata(model_path(m)).is_ok_and(|x| x.len() == m.size))
}

fn sha256_file(p: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 下載模型（已經有的不再下載）：先寫 .part，大小與 SHA-256 對了才改成正式檔名
pub fn download(progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    let todo: Vec<&ModelFile> = MODELS.iter().copied().filter(|m| !std::fs::metadata(model_path(m)).is_ok_and(|x| x.len() == m.size)).collect();
    download_to(&todo, &install_dir(), progress, cancel)
}

/// 把這些模型下載到 dir（測試也用：換成本機的網址）
fn download_to(todo: &[&ModelFile], dir: &Path, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    use std::io::{Read, Write};
    std::fs::create_dir_all(dir).map_err(|e| crate::trf!("無法建立資料夾：{e}", "Couldn't create the folder: {e}"))?;
    let total: u64 = todo.iter().map(|m| m.size).sum();
    let mut got = 0u64;
    for &m in todo {
        let part = dir.join(format!("{}.part", m.file));
        let (_, mut reader) = crate::http::download(m.url, std::time::Duration::from_secs(20), std::time::Duration::from_secs(60))
            .map_err(|e| crate::trf!("下載人臉模型失敗：{e}", "Couldn't download the face model: {e}"))?;
        let mut out = std::fs::File::create(&part).map_err(|e| crate::trf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut n_file = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(out);
                let _ = std::fs::remove_file(&part);
                return Err("已取消下載".into());
            }
            let n = reader.read(&mut buf).map_err(|e| crate::trf!("下載中斷：{}", "Download interrupted: {}", crate::http::describe_io(&e)))?;
            if n == 0 {
                break;
            }
            // 比官方的檔案大：不是這個檔案
            n_file += n as u64;
            if n_file > m.size {
                drop(out);
                let _ = std::fs::remove_file(&part);
                return Err(crate::tr!("下載的人臉模型和官方發佈的不一樣，請再試一次", "The downloaded face model doesn't match the official one. Please try again").into());
            }
            out.write_all(&buf[..n]).map_err(|e| crate::trf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"))?;
            h.update(&buf[..n]);
            got += n as u64;
            progress.store(((got * 1000) / total.max(1)).min(1000) as u32, Ordering::Relaxed);
        }
        drop(out);
        let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if n_file != m.size || digest != m.sha256 {
            let _ = std::fs::remove_file(&part);
            crate::warn!("[人臉] {} 檢查碼不符：{digest}（{n_file} bytes）", m.file);
            return Err(crate::tr!(
                "下載的人臉模型和官方發佈的不一樣（可能下載時損壞），請再試一次",
                "The downloaded face model doesn't match the official one (it may have been damaged). Please try again"
            )
            .into());
        }
        std::fs::rename(&part, dir.join(m.file)).map_err(|e| crate::trf!("無法儲存人臉模型：{e}", "Couldn't save the face model: {e}"))?;
        crate::info!("[人臉] 已下載 {}（SHA-256 相符）", m.file);
    }
    Ok(())
}

/// 安裝使用者自己下載的模型檔（無法自動下載時）：用 SHA-256 認出是哪一個，不是官方的檔案不用
pub fn install_file(src: &Path) -> Result<&'static str, String> {
    let size = std::fs::metadata(src).map(|m| m.len()).map_err(|e| e.to_string())?;
    let m = MODELS.iter().copied().find(|m| m.size == size && sha256_file(src).as_deref() == Some(m.sha256)).ok_or_else(|| {
        let names = MODELS.iter().map(|m| m.file).collect::<Vec<_>>().join(crate::tr!("、", ", "));
        crate::trf!("這不是官方發佈的人臉模型，請選這幾個之一：{names}", "This isn't an official face model. Choose one of: {names}")
    })?;
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| crate::error::plain(&e))?;
    let target = dir.join(m.file);
    let staging = target.with_extension("part");
    std::fs::copy(src, &staging).map_err(|e| crate::error::plain(&e))?;
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&staging, &target).map_err(|e| crate::error::plain(&e))?;
    Ok(m.file)
}

/// 移除模型檔
pub fn remove_models() -> Result<(), String> {
    for m in MODELS {
        for d in model_dirs() {
            let p = d.join(m.file);
            if p.exists() {
                std::fs::remove_file(&p).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

// ───────────── 執行模型 ─────────────

type Plan = TypedRunnableModel<TypedModel>;

/// 載入好的模型（多個執行緒共用）
pub struct Engine {
    det: Plan,
    rec: Plan,
    lm: Plan,
    bs: Plan,
}

static ENGINE: Mutex<Option<Arc<Engine>>> = Mutex::new(None);

impl Engine {
    /// 載入（第一次用到時；之後共用同一份）。載入前再算一次 SHA-256：只讀官方的檔案
    pub fn get() -> Result<Arc<Engine>, String> {
        let mut g = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.as_ref() {
            return Ok(e.clone());
        }
        let load = |m: &ModelFile, shape: &[usize]| -> Result<Plan, String> {
            let p = model_path(m);
            if sha256_file(&p).as_deref() != Some(m.sha256) {
                return Err(crate::trf!(
                    "人臉模型 {} 和官方發佈的不一樣，請到「設定 → 外部元件」重新下載",
                    "The face model {} doesn't match the official one. Download it again in Settings → Components",
                    m.file
                ));
            }
            tract_onnx::onnx()
                .model_for_path(&p)
                .and_then(|x| x.with_input_fact(0, f32::fact(shape.to_vec()).into()))
                .and_then(|x| x.into_optimized())
                .and_then(|x| x.into_runnable())
                .map_err(|e| crate::trf!("無法載入人臉模型：{e}", "Couldn't load the face model: {e}"))
        };
        let e = Arc::new(Engine { det: load(&YUNET, &[1, 3, DET, DET])?, rec: load(&SFACE, &[1, 3, 112, 112])?, lm: load(&LANDMARKS, &[1, LM, LM, 3])?, bs: load(&BLENDSHAPES, &[1, SUB.len(), 2])? });
        *g = Some(e.clone());
        Ok(e)
    }

    /// 找出臉（img：轉正好的相片；回傳的位置是 img 的像素）
    pub fn detect(&self, img: &image::RgbImage) -> Result<Vec<Face>, String> {
        let (w, h) = img.dimensions();
        // 縮到放得進 640×640（比 640 小的不放大），放在左上角，其他補黑
        let scale = (DET as f32 / w.max(h) as f32).min(1.0);
        let (nw, nh) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
        let small = if scale < 1.0 { image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle) } else { img.clone() };
        // YuNet 的輸入：BGR、0～255、NCHW
        let mut x = tract_ndarray::Array4::<f32>::zeros((1, 3, DET, DET));
        for (px, py, p) in small.enumerate_pixels() {
            let (px, py) = (px as usize, py as usize);
            x[[0, 0, py, px]] = p[2] as f32;
            x[[0, 1, py, px]] = p[1] as f32;
            x[[0, 2, py, px]] = p[0] as f32;
        }
        let out = self.det.run(tvec!(Tensor::from(x).into())).map_err(|e| e.to_string())?;
        let s = |i: usize| out.get(i).and_then(|t| t.as_slice::<f32>().ok()).unwrap_or(&[]);
        let faces = decode([s(0), s(1), s(2)], [s(3), s(4), s(5)], [s(6), s(7), s(8)], [s(9), s(10), s(11)], DET, SCORE);
        Ok(nms(faces, NMS)
            .into_iter()
            .map(|mut f| {
                for v in f.rect.iter_mut() {
                    *v /= scale;
                }
                for k in f.kps.iter_mut() {
                    k[0] /= scale;
                    k[1] /= scale;
                }
                f
            })
            .collect())
    }

    /// 一張轉正好的 112×112 臉 → 128 個數字（長度 1）
    pub fn embed(&self, face: &image::RgbImage) -> Result<[f32; 128], String> {
        // SFace 的輸入：RGB、0～255、NCHW
        let mut x = tract_ndarray::Array4::<f32>::zeros((1, 3, 112, 112));
        for (px, py, p) in face.enumerate_pixels() {
            for c in 0..3 {
                x[[0, c, py as usize, px as usize]] = p[c] as f32;
            }
        }
        let out = self.rec.run(tvec!(Tensor::from(x).into())).map_err(|e| e.to_string())?;
        let v = out.first().and_then(|t| t.as_slice::<f32>().ok()).filter(|v| v.len() == 128).ok_or("SFace 的輸出不對")?;
        let mut e = [0f32; 128];
        e.copy_from_slice(v);
        normalize(&mut e);
        Ok(e)
    }

    /// 這張臉是不是臉（0～1）、兩眼閉上的程度（本人的左眼、右眼，0 睜開～1 閉上；eyes 是 false 或不是臉時不算）。
    /// 和 MediaPipe 一樣：依兩眼轉正、臉的四周各多留 25% 的正方形縮成 256×256 → 478 個點；其中 146 個點（原圖的像素位置）→ 52 個表情分數
    pub fn check(&self, img: &image::RgbImage, f: &Face, eyes: bool) -> Result<(f32, Option<[f32; 2]>), String> {
        let (re, le) = (f.kps[0], f.kps[1]);
        let ang = (le[1] - re[1]).atan2(le[0] - re[0]);
        let (c, s) = (ang.cos(), ang.sin());
        let (cx, cy) = (f.rect[0] + f.rect[2] / 2.0, f.rect[1] + f.rect[3] / 2.0);
        let k = f.rect[2].max(f.rect[3]) * LM_SCALE / LM as f32;
        let half = LM as f32 / 2.0;
        // 256 方塊上的 (u, v) → 原圖：中心 + 轉 ang 的 (u − 128, v − 128)·k
        let (a, b) = (c * k, s * k);
        let (tx, ty) = (cx - (a - b) * half, cy - (b + a) * half);
        let to_img = |u: f32, v: f32| (a * u - b * v + tx, b * u + a * v + ty);
        let crop = warp(img, LM as u32, LM as u32, to_img);
        // 特徵點模型的輸入：RGB、0～1、NHWC
        let mut x = tract_ndarray::Array4::<f32>::zeros((1, LM, LM, 3));
        for (px, py, p) in crop.enumerate_pixels() {
            for ch in 0..3 {
                x[[0, py as usize, px as usize, ch]] = p[ch] as f32 / 255.0;
            }
        }
        let out = self.lm.run(tvec!(Tensor::from(x).into())).map_err(|e| e.to_string())?;
        let pts = out.first().and_then(|t| t.as_slice::<f32>().ok()).filter(|v| v.len() == 478 * 3).ok_or("特徵點模型的輸出不對")?;
        let flag = out.get(1).and_then(|t| t.as_slice::<f32>().ok()).and_then(|v| v.first().copied()).ok_or("特徵點模型的輸出不對")?;
        let presence = 1.0 / (1.0 + (-flag).exp());
        if !eyes || presence < PRESENCE {
            return Ok((presence, None));
        }
        let mut sub = tract_ndarray::Array3::<f32>::zeros((1, SUB.len(), 2));
        for (j, &i) in SUB.iter().enumerate() {
            let (x, y) = to_img(pts[i * 3], pts[i * 3 + 1]);
            sub[[0, j, 0]] = x;
            sub[[0, j, 1]] = y;
        }
        let out = self.bs.run(tvec!(Tensor::from(sub).into())).map_err(|e| e.to_string())?;
        let v = out.first().and_then(|t| t.as_slice::<f32>().ok()).filter(|v| v.len() == 52).ok_or("表情模型的輸出不對")?;
        // 第 9、10 個是 eyeBlinkLeft、eyeBlinkRight
        Ok((presence, Some([v[9], v[10]])))
    }
}

/// 表情模型要的 146 個點（MediaPipe face_blendshapes_graph.cc 的 kLandmarksSubsetIdxs）
const SUB: [usize; 146] = [
    0, 1, 4, 5, 6, 7, 8, 10, 13, 14, 17, 21, 33, 37, 39, 40, 46, 52, 53, 54, 55, 58, 61, 63, 65, 66, 67, 70, 78, 80, 81, 82, 84, 87, 88, 91, 93, 95, 103, 105, 107, 109, 127, 132, 133, 136, 144, 145,
    146, 148, 149, 150, 152, 153, 154, 155, 157, 158, 159, 160, 161, 162, 163, 168, 172, 173, 176, 178, 181, 185, 191, 195, 197, 234, 246, 249, 251, 263, 267, 269, 270, 276, 282, 283, 284, 285, 288,
    291, 293, 295, 296, 297, 300, 308, 310, 311, 312, 314, 317, 318, 321, 323, 324, 332, 334, 336, 338, 356, 361, 362, 365, 373, 374, 375, 377, 378, 379, 380, 381, 382, 384, 385, 386, 387, 388, 389,
    390, 397, 398, 400, 402, 405, 409, 415, 454, 466, 468, 469, 470, 471, 472, 473, 474, 475, 476, 477,
];

/// 一張臉：位置（x, y, 寬, 高）、分數、5 個特徵點（右眼、左眼、鼻尖、右嘴角、左嘴角；「右」是本人的右邊，在畫面左邊）
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub rect: [f32; 4],
    pub score: f32,
    pub kps: [[f32; 2]; 5],
}

/// YuNet 的輸出 → 臉（和 OpenCV 的 FaceDetectorYN 一樣的算法）：三種格子大小（8、16、32）的每一格都有
/// 分類分數、物件分數、框（相對格子的位移與對數大小）、5 個特徵點；分數 = √(分類 × 物件)
fn decode(cls: [&[f32]; 3], obj: [&[f32]; 3], bbox: [&[f32]; 3], kps: [&[f32]; 3], size: usize, threshold: f32) -> Vec<Face> {
    let mut out = vec![];
    for (i, stride) in [8usize, 16, 32].into_iter().enumerate() {
        let cols = size / stride;
        let n = cols * cols;
        if cls[i].len() < n || obj[i].len() < n || bbox[i].len() < n * 4 || kps[i].len() < n * 10 {
            continue;
        }
        for idx in 0..n {
            let score = (cls[i][idx].clamp(0.0, 1.0) * obj[i][idx].clamp(0.0, 1.0)).sqrt();
            if score < threshold {
                continue;
            }
            let (r, c) = ((idx / cols) as f32, (idx % cols) as f32);
            let s = stride as f32;
            let b = &bbox[i][idx * 4..idx * 4 + 4];
            let (cx, cy) = ((c + b[0]) * s, (r + b[1]) * s);
            let (w, h) = (b[2].exp() * s, b[3].exp() * s);
            let k = &kps[i][idx * 10..idx * 10 + 10];
            let mut pts = [[0f32; 2]; 5];
            for (j, p) in pts.iter_mut().enumerate() {
                *p = [(k[j * 2] + c) * s, (k[j * 2 + 1] + r) * s];
            }
            out.push(Face { rect: [cx - w / 2.0, cy - h / 2.0, w, h], score, kps: pts });
        }
    }
    out
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let (x1, y1) = (a[0].max(b[0]), a[1].max(b[1]));
    let (x2, y2) = ((a[0] + a[2]).min(b[0] + b[2]), (a[1] + a[3]).min(b[1] + b[3]));
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 重疊的框只留分數最高的
fn nms(mut faces: Vec<Face>, threshold: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = vec![];
    for f in faces {
        if keep.iter().all(|k| iou(&k.rect, &f.rect) <= threshold) {
            keep.push(f);
        }
    }
    keep
}

/// 轉正後 5 個特徵點的標準位置（112×112，ArcFace 的範本，OpenCV 的 alignCrop 也用這組）
const REF: [[f32; 2]; 5] = [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// 把特徵點對到標準位置的相似變換（旋轉 + 等比縮放 + 平移，最小平方法）：回傳 [a, b, tx, ty]，
/// 也就是 x' = a·x − b·y + tx、y' = b·x + a·y + ty
fn similarity(src: &[[f32; 2]; 5]) -> [f32; 4] {
    let mean = |p: &[[f32; 2]; 5]| [p.iter().map(|q| q[0]).sum::<f32>() / 5.0, p.iter().map(|q| q[1]).sum::<f32>() / 5.0];
    let (ms, md) = (mean(src), mean(&REF));
    let (mut num_a, mut num_b, mut den) = (0f32, 0f32, 0f32);
    for i in 0..5 {
        let (sx, sy) = (src[i][0] - ms[0], src[i][1] - ms[1]);
        let (dx, dy) = (REF[i][0] - md[0], REF[i][1] - md[1]);
        num_a += sx * dx + sy * dy;
        num_b += sx * dy - sy * dx;
        den += sx * sx + sy * sy;
    }
    if den <= f32::EPSILON {
        return [1.0, 0.0, md[0] - ms[0], md[1] - ms[1]];
    }
    let (a, b) = (num_a / den, num_b / den);
    [a, b, md[0] - (a * ms[0] - b * ms[1]), md[1] - (b * ms[0] + a * ms[1])]
}

/// 把臉轉正、縮成 112×112（雙線性，超出相片的地方是黑色），和 OpenCV 的 warpAffine 一樣
pub fn align(img: &image::RgbImage, kps: &[[f32; 2]; 5]) -> image::RgbImage {
    let [a, b, tx, ty] = similarity(kps);
    // 反過來：112×112 的每一點在原圖的哪裡
    let det = a * a + b * b;
    warp(img, 112, 112, |u, v| {
        let (u, v) = (u - tx, v - ty);
        ((a * u + b * v) / det, (-b * u + a * v) / det)
    })
}

/// 做一張 w×h 的圖：每一點 (u, v) 取原圖 at(u, v) 的顏色（雙線性，超出相片的地方是黑色）
fn warp(img: &image::RgbImage, w: u32, h: u32, at: impl Fn(f32, f32) -> (f32, f32)) -> image::RgbImage {
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let px = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= iw || y >= ih {
            return [0.0; 3];
        }
        let p = img.get_pixel(x as u32, y as u32);
        [p[0] as f32, p[1] as f32, p[2] as f32]
    };
    image::RgbImage::from_fn(w, h, |u, v| {
        let (x, y) = at(u as f32, v as f32);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (p00, p10, p01, p11) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
        let mut out = [0u8; 3];
        for c in 0..3 {
            let top = p00[c] * (1.0 - fx) + p10[c] * fx;
            let bot = p01[c] * (1.0 - fx) + p11[c] * fx;
            out[c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
        }
        image::Rgb(out)
    })
}

fn normalize(e: &mut [f32; 128]) {
    let n = e.iter().map(|v| v * v).sum::<f32>().sqrt();
    if n > 0.0 {
        for v in e.iter_mut() {
            *v /= n;
        }
    }
}

pub fn dot(a: &[f32; 128], b: &[f32; 128]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ───────────── 128 個數字的存法 ─────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 長度 1 的 128 個數字 → 每個乘 127 存成一個 byte（相似度只差約 1%）→ base64
pub fn pack(e: &[f32; 128]) -> String {
    let bytes: Vec<u8> = e.iter().map(|v| ((v * 127.0).round().clamp(-127.0, 127.0) as i8) as u8).collect();
    let mut s = String::with_capacity(172);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn unpack(s: &str) -> Option<[f32; 128]> {
    let val = |c: u8| B64.iter().position(|b| *b == c).map(|v| v as u32);
    let mut bytes = Vec::with_capacity(128);
    for chunk in s.as_bytes().chunks(4) {
        let mut n = 0u32;
        let mut k = 0usize;
        for &c in chunk {
            if c == b'=' {
                break;
            }
            n |= val(c)? << (18 - 6 * k);
            k += 1;
        }
        for i in 0..k.saturating_sub(1) {
            bytes.push((n >> (16 - 8 * i) & 255) as u8);
        }
    }
    if bytes.len() != 128 {
        return None;
    }
    let mut e = [0f32; 128];
    for (o, b) in e.iter_mut().zip(bytes) {
        *o = (b as i8) as f32 / 127.0;
    }
    normalize(&mut e);
    Some(e)
}

// ───────────── 分析一張相片 ─────────────

/// 存起來的一張臉
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceRec {
    /// 位置（x, y, 寬, 高，佔轉正後相片的比例 0～1）
    #[serde(rename = "b")]
    pub rect: [f32; 4],
    #[serde(rename = "s")]
    pub score: f32,
    /// 128 個數字（pack）
    #[serde(rename = "e")]
    pub emb: String,
    /// 是誰（人物編號；0 = 還沒分）
    #[serde(rename = "p", default)]
    pub person: u32,
    /// 使用者指定的（自動分組不再改）
    #[serde(rename = "u", default)]
    pub user: bool,
    /// 使用者說不是這些人
    #[serde(rename = "n", default, skip_serializing_if = "Vec::is_empty")]
    pub not: Vec<u32>,
    /// 兩眼閉上的程度（本人的左眼、右眼，0～100）；臉太小、還沒看過時沒有
    #[serde(rename = "k", default, skip_serializing_if = "Option::is_none")]
    pub eyes: Option<[u8; 2]>,
}

impl FaceRec {
    /// 兩眼都閉上的程度（比較張開的那隻眼睛，0～1）
    pub fn blink(&self) -> Option<f32> {
        self.eyes.map(|[l, r]| l.min(r) as f32 / 100.0)
    }
}

/// 找出一張相片裡的臉、算出每張臉的 128 個數字。打不開檔案（NAS 斷線）時 Err（之後再試）；
/// 讀得到但解不出圖（壞掉的檔案）時當作沒有臉
pub fn analyze(engine: &Engine, ffmpeg: Option<&Path>, path: &str) -> Result<Vec<FaceRec>, String> {
    std::fs::File::open(path).map_err(|e| e.to_string())?;
    let Some((img, _)) = crate::photos::load_full(ffmpeg, path, ANALYZE_MAX) else { return Ok(vec![]) };
    let img = image::DynamicImage::ImageRgba8(img).to_rgb8();
    let (w, h) = (img.width() as f32, img.height() as f32);
    let mut out = vec![];
    for f in engine.detect(&img)? {
        if f.rect[2].min(f.rect[3]) < MIN_FACE {
            continue;
        }
        // 不是臉（貓狗、雜訊）的不算；夠大的臉才看眼睛
        let (presence, eyes) = engine.check(&img, &f, f.rect[2] >= EYE_MIN)?;
        if presence < PRESENCE {
            continue;
        }
        let e = engine.embed(&align(&img, &f.kps))?;
        let eyes = eyes.map(|v| v.map(|x| (x.clamp(0.0, 1.0) * 100.0).round() as u8));
        out.push(FaceRec { rect: [f.rect[0] / w, f.rect[1] / h, f.rect[2] / w, f.rect[3] / h], score: f.score, emb: pack(&e), person: 0, user: false, not: vec![], eyes });
    }
    Ok(out)
}

/// 要分析的一張相片
#[derive(Debug, Clone)]
pub struct Job {
    pub path: String,
    pub size: u64,
    pub mtime: f64,
}

/// 同時分析幾張（每張都要從 NAS 讀檔、解碼、跑模型）
const THREADS: usize = 3;

/// 一張相片找臉的結果：（相片, 找到的臉；打不開時是原因）
pub type JobResult = (Job, Result<Vec<FaceRec>, String>);

/// 分析一批相片（同時 THREADS 張）；stop 時停在做到的那裡。回傳每張的結果（沒做到的不在裡面）
pub fn analyze_many(jobs: Vec<Job>, ffmpeg: Option<PathBuf>, stop: &AtomicBool) -> Result<Vec<JobResult>, String> {
    let engine = Engine::get()?;
    let queue = Mutex::new(jobs);
    let out = Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Some(job) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop() else { break };
                let r = analyze(&engine, ffmpeg.as_deref(), &job.path);
                out.lock().unwrap_or_else(|e| e.into_inner()).push((job, r));
            });
        }
    });
    Ok(out.into_inner().unwrap_or_else(|e| e.into_inner()))
}

// ───────────── 臉的資料與人物 ─────────────

/// 一張相片的臉（大小、修改時間對得上才算數：檔案換了就重新找）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoFaces {
    #[serde(rename = "s")]
    pub size: u64,
    #[serde(rename = "m")]
    pub mtime: f64,
    #[serde(rename = "f", default)]
    pub faces: Vec<FaceRec>,
    /// 用哪一版找的（VERSION；舊的要再看一次眼睛）
    #[serde(rename = "v", default)]
    pub ver: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Person {
    pub id: u32,
    /// 名稱（空的 = 還沒取名）
    #[serde(default)]
    pub name: String,
    /// 不要列出來（路人、不想看到的）
    #[serde(default)]
    pub hidden: bool,
}

/// 存在 photo-faces.json 的全部臉的資料
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FaceIndex {
    #[serde(default)]
    pub photos: BTreeMap<String, PhotoFaces>,
    #[serde(default)]
    pub people: Vec<Person>,
    #[serde(default)]
    pub next_id: u32,
}

pub fn index_file() -> PathBuf {
    data_dir().join("photo-faces.json")
}

pub fn load_index(file: &Path) -> FaceIndex {
    std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_index(file: &Path, idx: &FaceIndex) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = crate::paths::write_atomic(file, &serde_json::to_vec(idx).unwrap_or_default()) {
        crate::warn!("[人臉] 存不了臉的資料：{e}");
    }
}

/// 刪除全部臉的資料（模型留著）
pub fn delete_all_data() -> Result<(), String> {
    for p in [index_file(), face_thumbs_dir()] {
        if p.is_dir() {
            std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
        } else if p.exists() {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 一個人的整理結果：（人物, 幾張相片, 代表的臉（相片, 第幾張臉））
#[derive(Debug, Clone)]
pub struct PersonSummary {
    pub person: Person,
    pub photos: usize,
    pub cover: Option<(String, usize)>,
}

impl FaceIndex {
    /// 還沒找過（或檔案換了、用舊版找的）的相片
    pub fn needs(&self, path: &str, size: u64, mtime: f64) -> bool {
        !self.photos.get(path).is_some_and(|p| p.size == size && crate::paths::same_mtime(p.mtime, mtime) && p.ver >= VERSION)
    }

    /// 讀進舊版的資料後：拿掉分數不夠的臉（門檻提高了；使用者指定過的留著），沒有臉的相片不用再找。回傳有沒有改
    pub fn upgrade(&mut self) -> bool {
        let mut changed = false;
        for p in self.photos.values_mut().filter(|p| p.ver < VERSION) {
            let n = p.faces.len();
            p.faces.retain(|f| f.score >= SCORE || f.user);
            changed |= p.faces.len() != n;
            if p.faces.is_empty() {
                p.ver = VERSION;
                changed = true;
            }
        }
        if changed {
            self.drop_empty();
        }
        changed
    }

    /// 存一張相片找到的臉。同一個檔案用舊版找過時：對到的臉（位置重疊）留著原本分好的人、加上眼睛；
    /// 這次沒找到的拿掉（不是臉；使用者指定過的留著），新找到的加進去等分組
    pub fn put(&mut self, path: String, size: u64, mtime: f64, faces: Vec<FaceRec>) {
        let old = self.photos.remove(&path).filter(|p| p.size == size && crate::paths::same_mtime(p.mtime, mtime));
        let faces = match old {
            None => faces,
            Some(old) => {
                let mut new: Vec<Option<FaceRec>> = faces.into_iter().map(Some).collect();
                let mut out = vec![];
                for mut f in old.faces {
                    let best = new.iter().enumerate().filter_map(|(j, n)| n.as_ref().map(|n| (j, iou(&f.rect, &n.rect)))).max_by(|a, b| a.1.total_cmp(&b.1));
                    match best.filter(|(_, o)| *o >= 0.5) {
                        Some((j, _)) => {
                            let n = new[j].take().unwrap_or_else(|| f.clone());
                            (f.score, f.eyes) = (n.score, n.eyes);
                            out.push(f);
                        }
                        None if f.user => out.push(f),
                        None => {}
                    }
                }
                out.extend(new.into_iter().flatten());
                out
            }
        };
        // 沒有臉了的人在分組（cluster）時拿掉
        self.photos.insert(path, PhotoFaces { size, mtime, faces, ver: VERSION });
    }

    /// 有人閉眼的相片：兩眼閉上的程度都到 threshold 的臉（隱藏的人不算：卡通、雕像、路人）
    pub fn closed_eyes(&self, threshold: f32) -> HashSet<String> {
        let hidden: HashSet<u32> = self.people.iter().filter(|p| p.hidden).map(|p| p.id).collect();
        self.photos.iter().filter(|(_, p)| p.faces.iter().any(|f| !hidden.contains(&f.person) && f.blink().is_some_and(|b| b >= threshold))).map(|(k, _)| k.clone()).collect()
    }

    /// 還沒用新版看過眼睛的相片有幾張
    pub fn old_photos(&self) -> usize {
        self.photos.values().filter(|p| p.ver < VERSION).count()
    }

    /// 只留還在的相片（相片清單裡沒有的拿掉）
    pub fn retain_photos(&mut self, keep: &HashSet<&str>) {
        self.photos.retain(|p, _| keep.contains(p.as_str()));
    }

    fn new_person(&mut self) -> u32 {
        self.next_id = self.next_id.max(self.people.iter().map(|p| p.id).max().unwrap_or(0)) + 1;
        self.people.push(Person { id: self.next_id, name: String::new(), hidden: false });
        self.next_id
    }

    pub fn person(&self, id: u32) -> Option<&Person> {
        self.people.iter().find(|p| p.id == id)
    }

    /// 分組：還沒分的臉加進最像的人（夠像的話），不然自成一組；之後這次有動到的組和其他沒取名的組很像就合在一起
    pub fn cluster(&mut self) {
        // 每個人的臉加起來（代表）
        let mut sum: HashMap<u32, ([f32; 128], usize)> = HashMap::new();
        let mut todo: Vec<(String, usize, [f32; 128])> = vec![];
        for (path, pf) in &self.photos {
            for (i, f) in pf.faces.iter().enumerate() {
                let Some(e) = unpack(&f.emb) else { continue };
                if f.person == 0 {
                    todo.push((path.clone(), i, e));
                } else {
                    let s = sum.entry(f.person).or_insert(([0.0; 128], 0));
                    for (a, b) in s.0.iter_mut().zip(e) {
                        *a += b;
                    }
                    s.1 += 1;
                }
            }
        }
        let unit = |s: &[f32; 128]| {
            let mut v = *s;
            normalize(&mut v);
            v
        };
        let mut centers: HashMap<u32, [f32; 128]> = sum.iter().map(|(k, (s, _))| (*k, unit(s))).collect();
        let mut changed: HashSet<u32> = HashSet::new();
        for (path, i, e) in todo {
            let not = self.photos[&path].faces[i].not.clone();
            let best = centers.iter().filter(|(id, _)| !not.contains(id)).map(|(id, c)| (*id, dot(&e, c))).max_by(|a, b| a.1.total_cmp(&b.1));
            let id = match best {
                Some((id, s)) if s >= ASSIGN => id,
                _ => self.new_person(),
            };
            if let Some(f) = self.photos.get_mut(&path).and_then(|p| p.faces.get_mut(i)) {
                f.person = id;
            }
            let s = sum.entry(id).or_insert(([0.0; 128], 0));
            for (a, b) in s.0.iter_mut().zip(e) {
                *a += b;
            }
            s.1 += 1;
            centers.insert(id, unit(&s.0));
            changed.insert(id);
        }
        // 合併：這次有動到的組和其他組很像，而且至少一邊沒取名（兩個取了名的人不會自動合在一起）
        let named: HashSet<u32> = self.people.iter().filter(|p| !p.name.is_empty()).map(|p| p.id).collect();
        let mut changed: Vec<u32> = changed.into_iter().collect();
        changed.sort_unstable();
        for id in changed {
            let Some(c) = centers.get(&id).copied() else { continue };
            let best = centers.iter().filter(|(o, _)| **o != id && !(named.contains(o) && named.contains(&id))).map(|(o, oc)| (*o, dot(&c, oc))).max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((other, s)) = best.filter(|(_, s)| *s >= MERGE) {
                let _ = s;
                // 合進臉比較多的那個（或取了名的那個）
                let (from, into) = if named.contains(&id) || (!named.contains(&other) && sum[&id].1 > sum[&other].1) { (other, id) } else { (id, other) };
                if self.blocked(from, into) {
                    continue;
                }
                self.merge(from, into);
                let (sf, nf) = sum.remove(&from).unwrap_or(([0.0; 128], 0));
                let s = sum.entry(into).or_insert(([0.0; 128], 0));
                for (a, b) in s.0.iter_mut().zip(sf) {
                    *a += b;
                }
                s.1 += nf;
                centers.remove(&from);
                centers.insert(into, unit(&s.0));
            }
        }
        self.drop_empty();
    }

    /// 有使用者說「不是這個人」的臉時，兩組不能合
    fn blocked(&self, a: u32, b: u32) -> bool {
        self.photos.values().flat_map(|p| p.faces.iter()).any(|f| (f.person == a && f.not.contains(&b)) || (f.person == b && f.not.contains(&a)))
    }

    /// 沒有臉、也沒取名的人拿掉
    fn drop_empty(&mut self) {
        let used: HashSet<u32> = self.photos.values().flat_map(|p| p.faces.iter().map(|f| f.person)).collect();
        self.people.retain(|p| used.contains(&p.id) || !p.name.is_empty());
    }

    /// 把 from 的臉都改成 into
    pub fn merge(&mut self, from: u32, into: u32) {
        if from == into {
            return;
        }
        for f in self.photos.values_mut().flat_map(|p| p.faces.iter_mut()) {
            if f.person == from {
                f.person = into;
            }
            for n in f.not.iter_mut() {
                if *n == from {
                    *n = into;
                }
            }
        }
        // 取了名的合進沒取名的：名稱帶過去
        let from_name = self.person(from).map(|p| p.name.clone()).unwrap_or_default();
        if let Some(p) = self.people.iter_mut().find(|p| p.id == into) {
            if p.name.is_empty() {
                p.name = from_name;
            }
        }
        self.people.retain(|p| p.id != from);
    }

    /// 取名；和另一個人同名（不分大小寫、前後空白）時合在一起。回傳最後是哪個人
    pub fn rename(&mut self, id: u32, name: &str) -> u32 {
        let name = name.trim();
        if let Some(other) = self.people.iter().find(|p| p.id != id && !name.is_empty() && p.name.trim().to_lowercase() == name.to_lowercase()).map(|p| p.id) {
            self.merge(id, other);
            return other;
        }
        if let Some(p) = self.people.iter_mut().find(|p| p.id == id) {
            p.name = name.to_string();
        }
        id
    }

    pub fn set_hidden(&mut self, id: u32, hidden: bool) {
        if let Some(p) = self.people.iter_mut().find(|p| p.id == id) {
            p.hidden = hidden;
        }
    }

    /// 這幾張臉不是這個人：拿出來重新分（不會再分回這個人）
    pub fn not_this(&mut self, faces: &[(String, usize)], id: u32) {
        for (path, i) in faces {
            if let Some(f) = self.photos.get_mut(path).and_then(|p| p.faces.get_mut(*i)).filter(|f| f.person == id) {
                f.person = 0;
                f.user = false;
                if !f.not.contains(&id) {
                    f.not.push(id);
                }
            }
        }
        self.cluster();
    }

    /// 這幾張臉是這個人（使用者指定，之後不再自動改）
    pub fn assign(&mut self, faces: &[(String, usize)], id: u32) {
        for (path, i) in faces {
            if let Some(f) = self.photos.get_mut(path).and_then(|p| p.faces.get_mut(*i)) {
                f.person = id;
                f.user = true;
                f.not.retain(|n| *n != id);
            }
        }
        self.drop_empty();
    }

    /// 新增一個有名稱的人（「移到…」選新的人時）
    pub fn add_person(&mut self, name: &str) -> u32 {
        let id = self.new_person();
        self.rename(id, name)
    }

    /// 每個人的整理結果：有取名的在前（依相片張數），沒取名的依張數；代表的臉是分數最高、最大的那張
    pub fn summary(&self) -> Vec<PersonSummary> {
        let mut photos: HashMap<u32, HashSet<&str>> = HashMap::new();
        let mut cover: HashMap<u32, (f32, String, usize)> = HashMap::new();
        for (path, pf) in &self.photos {
            for (i, f) in pf.faces.iter().enumerate().filter(|(_, f)| f.person != 0) {
                photos.entry(f.person).or_default().insert(path);
                let q = f.score * (f.rect[2] * f.rect[3]).sqrt();
                if cover.get(&f.person).is_none_or(|c| q > c.0) {
                    cover.insert(f.person, (q, path.clone(), i));
                }
            }
        }
        let mut list: Vec<PersonSummary> =
            self.people.iter().map(|p| PersonSummary { person: p.clone(), photos: photos.get(&p.id).map_or(0, |s| s.len()), cover: cover.get(&p.id).map(|c| (c.1.clone(), c.2)) }).collect();
        list.sort_by(|a, b| a.person.name.is_empty().cmp(&b.person.name.is_empty()).then(b.photos.cmp(&a.photos)).then(a.person.id.cmp(&b.person.id)));
        list
    }

    /// 有這個人的相片
    pub fn photos_of(&self, id: u32) -> HashSet<String> {
        self.photos.iter().filter(|(_, p)| p.faces.iter().any(|f| f.person == id)).map(|(k, _)| k.clone()).collect()
    }

    /// 這個人的每一張臉（相片, 第幾張）
    pub fn faces_of(&self, id: u32) -> Vec<(String, usize)> {
        self.photos.iter().flat_map(|(k, p)| p.faces.iter().enumerate().filter(|(_, f)| f.person == id).map(move |(i, _)| (k.clone(), i))).collect()
    }

    /// 這張相片裡有取名的人
    pub fn names_in(&self, path: &str) -> Vec<String> {
        let Some(p) = self.photos.get(path) else { return vec![] };
        let mut v: Vec<String> = vec![];
        for f in &p.faces {
            if let Some(n) = self.person(f.person).filter(|x| !x.name.is_empty() && !x.hidden).map(|x| x.name.clone()) {
                if !v.contains(&n) {
                    v.push(n);
                }
            }
        }
        v
    }
}

// ───────────── 臉的小圖 ─────────────

fn face_thumbs_dir() -> PathBuf {
    data_dir().join("face-thumbs")
}

/// 一張臉的小圖（正方形，臉的四周多留一點；存在 face-thumbs，以相片的路徑、大小、修改時間、臉的位置為鍵）
pub fn face_thumb(ffmpeg: Option<&Path>, path: &str, rect: [f32; 4], size: u32) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let key: String = {
        let mut h = Sha256::new();
        h.update(format!("{}|{}|{}|{:?}|{size}", path.to_lowercase(), meta.len(), mtime_ms(&meta) as i64, rect).as_bytes());
        h.finalize().iter().take(20).map(|b| format!("{b:02x}")).collect()
    };
    let dir = face_thumbs_dir();
    let out = dir.join(format!("{key}.jpg"));
    if out.is_file() {
        return Some(out);
    }
    // 相片分頁已經做好縮圖（最長 480，在這台電腦上）而且臉夠大：從縮圖裁，不用再從 NAS 讀整張相片
    let small = crate::photos::cached_thumb(path, meta.len(), mtime_ms(&meta))
        .and_then(|t| image::open(t).ok())
        .map(|i| i.to_rgba8())
        .filter(|i| (rect[2] * i.width() as f32).max(rect[3] * i.height() as f32) >= 48.0);
    let img = match small {
        Some(i) => i,
        None => crate::photos::load_full(ffmpeg, path, ANALYZE_MAX)?.0,
    };
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (cx, cy) = ((rect[0] + rect[2] / 2.0) * w, (rect[1] + rect[3] / 2.0) * h);
    let side = (rect[2] * w).max(rect[3] * h) * 1.5;
    let x0 = (cx - side / 2.0).clamp(0.0, (w - 1.0).max(0.0));
    let y0 = (cy - side / 2.0).clamp(0.0, (h - 1.0).max(0.0));
    let s = side.min(w - x0).min(h - y0).max(1.0);
    let crop = image::imageops::crop_imm(&img, x0 as u32, y0 as u32, s as u32, s as u32).to_image();
    let small = image::imageops::resize(&crop, size, size, image::imageops::FilterType::Triangle);
    let rgb = image::DynamicImage::ImageRgba8(small).to_rgb8();
    let mut bytes = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85).encode_image(&rgb).ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    let tmp = out.with_extension("tmp");
    std::fs::write(&tmp, bytes).ok()?;
    std::fs::rename(&tmp, &out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emb(seed: u32, noise: f32) -> [f32; 128] {
        // 每個 seed 一個方向，加一點不同的雜訊（同一個人的不同相片）
        let mut e = [0f32; 128];
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(12345);
        for (i, v) in e.iter_mut().enumerate() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *v = (x % 1000) as f32 / 500.0 - 1.0 + noise * ((i as f32 * 1.7 + seed as f32 + noise * 31.0).sin());
        }
        normalize(&mut e);
        e
    }

    fn face(e: &[f32; 128]) -> FaceRec {
        FaceRec { rect: [0.1, 0.1, 0.2, 0.2], score: 0.9, emb: pack(e), person: 0, user: false, not: vec![], eyes: None }
    }

    #[test]
    fn pack_round_trip() {
        let e = emb(7, 0.0);
        let back = unpack(&pack(&e)).unwrap();
        assert!(dot(&e, &back) > 0.999);
        assert_eq!(pack(&e).len(), 172);
        assert!(unpack("abc").is_none() && unpack("!!!!").is_none());
    }

    #[test]
    fn similarity_maps_reference_points() {
        // 特徵點就是標準位置平移、放大兩倍、轉 30 度：轉回來要對到標準位置
        let (s, t) = (30f32.to_radians(), [100.0, 50.0]);
        let src: [[f32; 2]; 5] = REF.map(|p| [2.0 * (p[0] * s.cos() - p[1] * s.sin()) + t[0], 2.0 * (p[0] * s.sin() + p[1] * s.cos()) + t[1]]);
        let [a, b, tx, ty] = similarity(&src);
        for (p, r) in src.iter().zip(REF) {
            let (x, y) = (a * p[0] - b * p[1] + tx, b * p[0] + a * p[1] + ty);
            assert!((x - r[0]).abs() < 1e-2 && (y - r[1]).abs() < 1e-2, "{x},{y} vs {r:?}");
        }
        // 轉正的圖：特徵點周圍的顏色搬到標準位置
        let mut img = image::RgbImage::new(400, 400);
        let nose = src[2];
        img.put_pixel(nose[0] as u32, nose[1] as u32, image::Rgb([255, 255, 255]));
        let al = align(&img, &src);
        assert_eq!(al.dimensions(), (112, 112));
        let bright = al.enumerate_pixels().max_by_key(|(_, _, p)| p[0]).map(|(x, y, _)| (x as f32, y as f32)).unwrap();
        assert!((bright.0 - REF[2][0]).abs() <= 1.5 && (bright.1 - REF[2][1]).abs() <= 1.5, "{bright:?}");
    }

    #[test]
    fn decode_and_nms() {
        // 640 的格子：stride 32 的第 (r=2, c=3) 格有一張臉，旁邊一格也有（重疊，分數低一點）
        let n = |s: usize| (640 / s) * (640 / s);
        let mk = |s: usize, k: usize| vec![0f32; n(s) * k];
        let (mut cls, mut obj, mut bbox, mut kps) = ([mk(8, 1), mk(16, 1), mk(32, 1)], [mk(8, 1), mk(16, 1), mk(32, 1)], [mk(8, 4), mk(16, 4), mk(32, 4)], [mk(8, 10), mk(16, 10), mk(32, 10)]);
        let cols = 640 / 32;
        for (idx, score) in [(2 * cols + 3, 0.95f32), (2 * cols + 4, 0.9)] {
            cls[2][idx] = score;
            obj[2][idx] = score;
            bbox[2][idx * 4..idx * 4 + 4].copy_from_slice(&[0.5, 0.5, 2f32.ln() * 1.0, 2f32.ln()]);
            kps[2][idx * 10..idx * 10 + 2].copy_from_slice(&[0.25, 0.25]);
        }
        fn s(v: &[Vec<f32>; 3]) -> [&[f32]; 3] {
            [v[0].as_slice(), v[1].as_slice(), v[2].as_slice()]
        }
        let faces = decode(s(&cls), s(&obj), s(&bbox), s(&kps), 640, 0.8);
        assert_eq!(faces.len(), 2);
        let f = &faces.iter().max_by(|a, b| a.score.total_cmp(&b.score)).unwrap();
        // 中心 ((3 + 0.5) × 32, (2 + 0.5) × 32) = (112, 80)，大小 2 × 32 = 64
        assert!((f.rect[0] - 80.0).abs() < 1e-3 && (f.rect[1] - 48.0).abs() < 1e-3 && (f.rect[2] - 64.0).abs() < 1e-3);
        assert!((f.kps[0][0] - 104.0).abs() < 1e-3 && (f.kps[0][1] - 72.0).abs() < 1e-3);
        // 兩個框重疊一半以上：只留分數高的
        let kept = nms(faces, 0.3);
        assert_eq!(kept.len(), 1);
        assert!((kept[0].score - 0.95).abs() < 1e-6);
    }

    #[test]
    fn clustering_and_people() {
        let mut idx = FaceIndex::default();
        // 兩個人各 4 張相片，另外一個路人 1 張
        for i in 0..4 {
            idx.photos.insert(format!("/video/family/a{i}.jpg"), PhotoFaces { size: 1, mtime: 0.0, ver: VERSION, faces: vec![face(&emb(1, 0.15 + i as f32 * 0.02))] });
            idx.photos.insert(format!("/video/family/b{i}.jpg"), PhotoFaces { size: 1, mtime: 0.0, ver: VERSION, faces: vec![face(&emb(2, 0.15 + i as f32 * 0.02))] });
        }
        idx.photos.insert("/video/family/c.jpg".into(), PhotoFaces { size: 1, mtime: 0.0, ver: VERSION, faces: vec![face(&emb(3, 0.0)), face(&emb(1, 0.3))] });
        idx.cluster();
        let s = idx.summary();
        let mut counts: Vec<usize> = s.iter().map(|p| p.photos).collect();
        counts.sort_unstable();
        assert_eq!(counts, [1, 4, 5], "{s:?}");
        let a = idx.photos["/video/family/a0.jpg"].faces[0].person;
        assert_eq!(idx.photos["/video/family/c.jpg"].faces[1].person, a);
        // 取名、同名的合在一起
        idx.rename(a, "小明");
        let b = idx.photos["/video/family/b0.jpg"].faces[0].person;
        assert_eq!(idx.rename(b, " 小明 "), a);
        assert_eq!(idx.photos_of(a).len(), 9);
        // 不是這個人：拿出來，不會再分回去
        idx.not_this(&[("/video/family/b1.jpg".into(), 0)], a);
        let p = idx.photos["/video/family/b1.jpg"].faces[0].person;
        assert!(p != a && p != 0);
        // 指定給某人：之後不再自動改
        let c = idx.add_person("小美");
        idx.assign(&[("/video/family/c.jpg".into(), 0)], c);
        idx.cluster();
        assert_eq!(idx.photos["/video/family/c.jpg"].faces[0].person, c);
        assert_eq!(idx.names_in("/video/family/c.jpg"), ["小美", "小明"]);
        // 有取名的排在前面
        assert!(!idx.summary()[0].person.name.is_empty());
        // 新的臉：自動加進最像的人（像 b1 的跟著 b1：使用者說過 b1 不是小明）
        idx.photos.insert("/video/family/new.jpg".into(), PhotoFaces { size: 1, mtime: 0.0, ver: VERSION, faces: vec![face(&emb(1, 0.2)), face(&emb(2, 0.2))] });
        idx.cluster();
        assert_eq!(idx.photos["/video/family/new.jpg"].faces[0].person, a);
        assert_eq!(idx.photos["/video/family/new.jpg"].faces[1].person, p);
        // 存回去再讀
        let file = std::env::temp_dir().join(format!("sr-faces-{}.json", std::process::id()));
        save_index(&file, &idx);
        let back = load_index(&file);
        assert_eq!(back.photos, idx.photos);
        assert_eq!(back.people, idx.people);
        std::fs::remove_file(&file).ok();
        assert!(idx.needs("/video/family/x.jpg", 1, 0.0) && !idx.needs("/video/family/a0.jpg", 1, 0.0) && idx.needs("/video/family/a0.jpg", 2, 0.0));
    }

    #[test]
    fn upgrade_put_and_closed_eyes() {
        let at = |x: f32, eyes: Option<[u8; 2]>, score: f32| FaceRec { rect: [x, 0.1, 0.2, 0.2], score, eyes, ..face(&emb(1, 0.1)) };
        let mut idx = FaceIndex::default();
        // 舊版的資料：分數不夠的臉拿掉（使用者指定的留著）、沒有臉的相片不用再找
        let mut kept = at(0.5, None, 0.82);
        kept.user = true;
        idx.photos.insert("/video/family/old.jpg".into(), PhotoFaces { size: 1, mtime: 0.0, ver: 0, faces: vec![at(0.1, None, 0.95), at(0.3, None, 0.81), kept] });
        idx.photos.insert("/video/family/none.jpg".into(), PhotoFaces { size: 1, mtime: 0.0, ver: 0, faces: vec![] });
        idx.photos.insert("/video/family/cat.jpg".into(), PhotoFaces { size: 1, mtime: 0.0, ver: 0, faces: vec![at(0.1, None, 0.8)] });
        assert!(idx.upgrade());
        idx.cluster();
        assert_eq!(idx.photos["/video/family/old.jpg"].faces.len(), 2);
        assert!(!idx.needs("/video/family/none.jpg", 1, 0.0) && !idx.needs("/video/family/cat.jpg", 1, 0.0));
        assert!(idx.needs("/video/family/old.jpg", 1, 0.0));
        assert_eq!(idx.old_photos(), 1);
        // 再找一次（同一個檔案）：對到的臉留著原本的人、加上眼睛；沒對到的拿掉（使用者指定的留著）；新的加進去
        let a = idx.photos["/video/family/old.jpg"].faces[0].person;
        assert_ne!(a, 0);
        idx.put("/video/family/old.jpg".into(), 1, 0.0, vec![at(0.11, Some([90, 80]), 0.96), at(0.7, Some([5, 3]), 0.9)]);
        let p = &idx.photos["/video/family/old.jpg"];
        assert_eq!(p.ver, VERSION);
        assert_eq!(p.faces.len(), 3);
        assert_eq!((p.faces[0].person, p.faces[0].eyes, p.faces[0].rect[0]), (a, Some([90, 80]), 0.1));
        assert!(p.faces[1].user && p.faces[2].person == 0);
        assert!(!idx.needs("/video/family/old.jpg", 1, 0.0));
        // 檔案換了：整個換掉
        idx.put("/video/family/old.jpg".into(), 2, 0.0, vec![at(0.4, None, 0.9)]);
        assert_eq!(idx.photos["/video/family/old.jpg"].faces.len(), 1);
        // 有人閉眼：兩眼都要到門檻；隱藏的人不算
        idx.put("/video/family/blink.jpg".into(), 1, 0.0, vec![at(0.1, Some([90, 75]), 0.95)]);
        idx.put("/video/family/wink.jpg".into(), 1, 0.0, vec![at(0.1, Some([95, 10]), 0.95)]);
        idx.cluster();
        assert_eq!(idx.closed_eyes(0.7), HashSet::from(["/video/family/blink.jpg".to_string()]));
        assert!(idx.closed_eyes(0.8).is_empty());
        let who = idx.photos["/video/family/blink.jpg"].faces[0].person;
        idx.set_hidden(who, true);
        assert!(idx.closed_eyes(0.7).is_empty());
    }

    #[test]
    fn install_rejects_other_files() {
        let f = std::env::temp_dir().join(format!("sr-faces-model-{}.onnx", std::process::id()));
        std::fs::write(&f, b"not a model").unwrap();
        assert!(install_file(&f).is_err());
        std::fs::remove_file(&f).ok();
    }

    /// 在本機開一個只回一個檔案的網頁伺服器（每次連線都回同一份）
    fn serve(body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut buf = [0u8; 2048];
                let _ = c.read(&mut buf);
                let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = c.write_all(&body);
            }
        });
        format!("http://{addr}/m.onnx")
    }

    #[test]
    fn download_checks_size_and_sha256() {
        let body = b"pretend this is an onnx model".to_vec();
        let sha: String = Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let dir = std::env::temp_dir().join(format!("sr-faces-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (progress, cancel) = (AtomicU32::new(0), AtomicBool::new(false));
        // 正確的檔案：下載好、改成正式檔名、沒有留下 .part
        let good = ModelFile { file: "good.onnx", url: leak(serve(body.clone())), size: body.len() as u64, sha256: leak(sha.clone()) };
        download_to(&[&good], &dir, &progress, &cancel).unwrap();
        assert_eq!(std::fs::read(dir.join("good.onnx")).unwrap(), body);
        assert!(!dir.join("good.onnx.part").exists());
        assert_eq!(progress.load(Ordering::Relaxed), 1000);
        // 內容被換掉（大小一樣、SHA-256 不同）：不用、不留檔案
        let mut evil = body.clone();
        evil[0] ^= 1;
        let bad = ModelFile { file: "bad.onnx", url: leak(serve(evil)), size: body.len() as u64, sha256: leak(sha.clone()) };
        assert!(download_to(&[&bad], &dir, &progress, &cancel).is_err());
        assert!(!dir.join("bad.onnx").exists() && !dir.join("bad.onnx.part").exists());
        // 比官方的大：讀到超過就停
        let mut big = body.clone();
        big.extend([0u8; 100]);
        let large = ModelFile { file: "large.onnx", url: leak(serve(big)), size: body.len() as u64, sha256: leak(sha.clone()) };
        assert!(download_to(&[&large], &dir, &progress, &cancel).is_err());
        assert!(!dir.join("large.onnx").exists() && !dir.join("large.onnx.part").exists());
        // 取消：不留檔案
        cancel.store(true, Ordering::Relaxed);
        let c = ModelFile { file: "c.onnx", url: leak(serve(body.clone())), size: body.len() as u64, sha256: leak(sha) };
        assert!(download_to(&[&c], &dir, &progress, &cancel).is_err());
        assert!(!dir.join("c.onnx").exists() && !dir.join("c.onnx.part").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 有模型時（SCREENRECORDER_TEST_FACE_MODELS 指到放兩個模型的資料夾）：載入、找臉、認人跑得起來
    #[test]
    fn engine_with_real_models() {
        let Some(dir) = std::env::var_os("SCREENRECORDER_TEST_FACE_MODELS").map(PathBuf::from) else {
            eprintln!("略過：沒有設定 SCREENRECORDER_TEST_FACE_MODELS");
            return;
        };
        for m in MODELS {
            assert_eq!(sha256_file(&dir.join(m.file)).as_deref(), Some(m.sha256), "{}", m.file);
        }
        let load = |m: &ModelFile, shape: &[usize]| {
            tract_onnx::onnx().model_for_path(dir.join(m.file)).unwrap().with_input_fact(0, f32::fact(shape.to_vec()).into()).unwrap().into_optimized().unwrap().into_runnable().unwrap()
        };
        let engine = Engine { det: load(&YUNET, &[1, 3, DET, DET]), rec: load(&SFACE, &[1, 3, 112, 112]), lm: load(&LANDMARKS, &[1, LM, LM, 3]), bs: load(&BLENDSHAPES, &[1, SUB.len(), 2]) };
        // 沒有臉的圖：找不到臉
        let blank = image::RgbImage::from_pixel(800, 600, image::Rgb([90, 120, 160]));
        assert!(engine.detect(&blank).unwrap().is_empty());
        let e = engine.embed(&image::RgbImage::from_pixel(112, 112, image::Rgb([120, 100, 90]))).unwrap();
        assert!((dot(&e, &e) - 1.0).abs() < 1e-4);
        // 有測試相片時（SCREENRECORDER_TEST_FACE_IMAGE，可以用 | 隔開好幾張）：印出找到的臉、是不是臉、眼睛，方便和 OpenCV、MediaPipe 比對
        if let Ok(list) = std::env::var("SCREENRECORDER_TEST_FACE_IMAGE") {
            for file in list.split('|') {
                let img = image::open(file).unwrap().to_rgb8();
                for f in engine.detect(&img).unwrap() {
                    let e = engine.embed(&align(&img, &f.kps)).unwrap();
                    let (presence, eyes) = engine.check(&img, &f, true).unwrap();
                    println!("FACE {file} {:?} {:.4} {:?} PRESENCE {presence:.4} EYES {eyes:?} EMB {:?}", f.rect, f.score, f.kps, e.to_vec());
                }
            }
        }
    }
}
