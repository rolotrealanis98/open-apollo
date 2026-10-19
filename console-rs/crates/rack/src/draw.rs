use std::sync::Arc;

use houston::{Insets, Rect, Rgba};

/// A vertical filmstrip frame: rows `index*height .. (index+1)*height` of the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Frame {
    pub height: u32,
    pub index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: Arc<str>,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: Rgba,
}

/// One drawing command, in window points. Console art is 2× (image px = 2 × pt).
#[derive(Clone, Debug)]
pub enum Draw {
    /// `key` is an `images/`-relative path or `plugin:<EffectName>`. `slice` = 9-slice insets in
    /// image px (corners keep their size). `tint` multiplies colour and alpha.
    Image { key: Arc<str>, frame: Option<Frame>, dst: Rect, slice: Option<Insets>, tint: Rgba },
    Fill { rect: Rect, color: Rgba },
    /// Text laid out inside `rect` (vertically centred; `\n` breaks lines).
    Text { text: String, style: TextStyle, rect: Rect, align: Align },
    /// Clip subsequent draws to a rect (None = no clip).
    Clip(Option<Rect>),
}

#[derive(Clone, Debug, Default)]
pub struct DrawList {
    pub items: Vec<Draw>,
}

impl DrawList {
    pub fn push(&mut self, d: Draw) {
        self.items.push(d);
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    /// The topmost text drawn inside `r`, for scripted drivers.
    pub fn text_in(&self, r: Rect) -> Option<String> {
        self.items.iter().rev().find_map(|d| match d {
            Draw::Text { text, rect, .. } if r.contains(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0) => Some(text.clone()),
            _ => None,
        })
    }
}
