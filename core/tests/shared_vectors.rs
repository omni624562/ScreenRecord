//! 介面（TypeScript）與後端共用的計算：以 src/shared/vectors.json（TypeScript 算出的結果）為準，Rust 版必須算出一樣的值。
//! 資料的產生方式見 src/shared/vectors.ts。

use screenrecorder_core::edit::{cut_file_name, keep_ranges, normalize_crop, normalize_ranges, total_length, CropInput, EditSpec, Range};
use screenrecorder_core::format::{
    check_recording_name, clock, estimate_bytes, export_file_name, format_bytes, human_duration, output_size, parse_clock, parse_export_name, scaled_size, speed_for_target, speed_label, video_clock,
    EstimateInput,
};
use screenrecorder_core::types::ExportFormat;
use serde_json::{json, Value};

const VECTORS: &str = include_str!("../../src/shared/vectors.json");

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap()
}
fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}
fn ranges(v: &Value) -> Vec<Range> {
    serde_json::from_value(v.clone()).unwrap()
}
fn size((w, h): (i32, i32)) -> Value {
    json!({ "width": w, "height": h })
}
fn opt<T: Into<Value>>(v: Option<T>) -> Value {
    v.map(Into::into).unwrap_or(Value::Null)
}

/// 用 Rust 版計算一個測試案例
fn compute(name: &str, a: &[Value]) -> Value {
    match name {
        "outputSize" => size(output_size(f(&a[0]) as i32, f(&a[1]) as i32, f(&a[2]))),
        "speedLabel" => speed_label(f(&a[0])).into(),
        "exportFileName" => export_file_name(s(&a[0]), f(&a[1]), serde_json::from_value::<ExportFormat>(a[2].clone()).unwrap()).into(),
        "parseExportName" => opt(parse_export_name(s(&a[0])).map(|p| json!({ "base": p.base, "speed": p.speed, "format": p.format }))),
        "scaledSize" => size(scaled_size(f(&a[0]) as i32, f(&a[1]) as i32, f(&a[2]) as i32)),
        "estimateBytes" => {
            let o = &a[0];
            let (lo, hi) = estimate_bytes(&EstimateInput {
                format: serde_json::from_value(o["format"].clone()).unwrap(),
                src_bytes: f(&o["srcBytes"]),
                src_sec: f(&o["srcSec"]),
                src_width: f(&o["srcWidth"]),
                src_height: f(&o["srcHeight"]),
                speed: f(&o["speed"]),
                width: f(&o["width"]),
                height: f(&o["height"]),
                gif_fps: o.get("gifFps").and_then(Value::as_f64),
            });
            json!([lo, hi])
        }
        "checkRecordingName" => opt(check_recording_name(s(&a[0]))),
        "parseClock" => opt(parse_clock(s(&a[0]))),
        "speedForTarget" => speed_for_target(f(&a[0]), f(&a[1]), a[2].as_bool().unwrap()).into(),
        "humanDuration" => human_duration(f(&a[0])).into(),
        "clock" => clock(f(&a[0])).into(),
        "videoClock" => video_clock(f(&a[0])).into(),
        "formatBytes" => format_bytes(f(&a[0]) as u64).into(),
        "normalizeRanges" => json!(normalize_ranges(&ranges(&a[0]), f(&a[1]))),
        "keepRanges" => {
            let spec = EditSpec { start: f(&a[1]["start"]), end: f(&a[1]["end"]), removed: ranges(&a[1]["removed"]), crop: None, overlays: vec![] };
            json!(keep_ranges(f(&a[0]), &spec))
        }
        "totalLength" => total_length(&ranges(&a[0])).into(),
        "normalizeCrop" => {
            let crop: Option<CropInput> = serde_json::from_value(a[0].clone()).unwrap();
            opt(normalize_crop(crop, f(&a[1]) as i32, f(&a[2]) as i32).map(|r| json!({ "x": r.x, "y": r.y, "width": r.width, "height": r.height })))
        }
        "cutFileName" => cut_file_name(s(&a[0])).into(),
        other => panic!("vectors.json 有 Rust 測試還不認得的函式：{other}（請在 core/tests/shared_vectors.rs 加上對應）"),
    }
}

/// 數字以數值比較（1 與 1.0 相同，浮點數允許極小的誤差）
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            x == y || (x - y).abs() <= 1e-9 * x.abs().max(y.abs())
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

#[test]
fn rust_matches_typescript() {
    let all: serde_json::Map<String, Value> = serde_json::from_str(VECTORS).unwrap();
    let mut failures = Vec::new();
    let mut count = 0;
    for (name, cases) in &all {
        for case in cases.as_array().unwrap() {
            count += 1;
            let args = case["args"].as_array().unwrap();
            let got = compute(name, args);
            if !same(&got, &case["out"]) {
                failures.push(format!("{name}({}) → Rust {got}，TypeScript {}", Value::Array(args.clone()), case["out"]));
            }
        }
    }
    assert!(count > 100, "測試案例太少：{count}");
    assert!(failures.is_empty(), "Rust 與介面的計算結果不一致：\n{}", failures.join("\n"));
}
