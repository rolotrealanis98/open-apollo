//! `<png>.draws.json`: what a headless render drew, for the alignment tools
//! (`console-rs/tools/align/placement_diff.py`). Images and text in draw order, in window points,
//! with the clip that applied. Contains UA image paths and labels only, never pixels.

use std::fmt::Write;

use houston::{Rect, Rgba};
use rack::{Align, Draw, DrawList};

fn rect(r: Rect) -> String {
    format!("[{},{},{},{}]", num(r.x), num(r.y), num(r.w), num(r.h))
}

fn num(v: f32) -> String {
    let r = (v as f64 * 1000.0).round() / 1000.0;
    if r == r.trunc() { format!("{}", r as i64) } else { format!("{r}") }
}

fn color(c: Rgba) -> String {
    format!("[{},{},{},{}]", num(c.r), num(c.g), num(c.b), num(c.a))
}

fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON description of `list` (format `gfx-draws/1`).
pub fn draws_json(list: &DrawList, size_pt: (f32, f32), scale: f32) -> String {
    let mut images = Vec::new();
    let mut texts = Vec::new();
    let mut clip: Option<Rect> = None;
    let clip_s = |c: Option<Rect>| c.map(rect).unwrap_or_else(|| "null".into());
    for (i, item) in list.items.iter().enumerate() {
        match item {
            Draw::Clip(r) => clip = *r,
            Draw::Fill { .. } => {}
            Draw::Image { key, frame, dst, slice, tint } => {
                let frame = frame.map(|f| format!("[{},{}]", f.index, f.height)).unwrap_or_else(|| "null".into());
                let slice = slice
                    .map(|s| format!("[{},{},{},{}]", num(s.l), num(s.t), num(s.r), num(s.b)))
                    .unwrap_or_else(|| "null".into());
                images.push(format!(
                    "{{\"i\":{i},\"key\":{},\"frame\":{frame},\"dst\":{},\"slice\":{slice},\"tint\":{},\"clip\":{}}}",
                    string(key),
                    rect(*dst),
                    color(*tint),
                    clip_s(clip)
                ));
            }
            Draw::Text { text, style, rect: r, align } => {
                let align = match align {
                    Align::Left => "left",
                    Align::Center => "center",
                    Align::Right => "right",
                };
                texts.push(format!(
                    "{{\"i\":{i},\"text\":{},\"family\":{},\"size\":{},\"bold\":{},\"italic\":{},\"rect\":{},\"align\":\"{align}\",\"color\":{},\"clip\":{}}}",
                    string(text),
                    string(&style.family),
                    num(style.size),
                    style.bold,
                    style.italic,
                    rect(*r),
                    color(style.color),
                    clip_s(clip)
                ));
            }
        }
    }
    format!(
        "{{\"format\":\"gfx-draws/1\",\"units\":\"pt\",\"scale\":{},\"size\":[{},{}],\n\"images\":[\n{}\n],\n\"texts\":[\n{}\n]}}\n",
        num(scale),
        num(size_pt.0),
        num(size_pt.1),
        images.join(",\n"),
        texts.join(",\n")
    )
}

/// `out.png` → `out.draws.json`.
pub fn sidecar_path(png: &std::path::Path) -> std::path::PathBuf {
    png.with_extension("draws.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use houston::Insets;
    use rack::{Frame, TextStyle};
    use std::sync::Arc;

    #[test]
    fn json_shape() {
        let mut l = DrawList::default();
        l.push(Draw::Clip(Some(Rect::new(0.0, 0.0, 10.0, 10.0))));
        l.push(Draw::Image {
            key: Arc::from("a/b.png"),
            frame: Some(Frame { height: 40, index: 3 }),
            dst: Rect::new(1.5, 2.0, 20.0, 20.0),
            slice: Some(Insets::uniform(4.0)),
            tint: Rgba::WHITE,
        });
        l.push(Draw::Text {
            text: "a\"b\nc".into(),
            style: TextStyle { family: Arc::from("Helvetica"), size: 9.0, bold: true, italic: false, color: Rgba::BLACK },
            rect: Rect::new(0.0, 0.0, 50.0, 12.0),
            align: Align::Center,
        });
        let j = draws_json(&l, (100.0, 50.0), 2.0);
        assert!(j.contains("\"key\":\"a/b.png\",\"frame\":[3,40],\"dst\":[1.5,2,20,20],\"slice\":[4,4,4,4]"));
        assert!(j.contains("\"text\":\"a\\\"b\\nc\""));
        assert!(j.contains("\"clip\":[0,0,10,10]"));
        assert_eq!(sidecar_path(std::path::Path::new("/x/out.png")), std::path::Path::new("/x/out.draws.json"));
    }
}
