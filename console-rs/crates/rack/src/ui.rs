//! Frame-building context: skins, images, layouts, the draw list and hit regions.

use std::collections::HashMap;
use std::sync::Arc;

use houston::{Appearance, ImageCache, Insets, Layout, Rect, Resources, Rgba, Skin};
use parking_lot::Mutex;

use crate::draw::{Align, Draw, DrawList, Frame, TextStyle};
use crate::input::{ControlId, Hit, HitKind, ScrollRegion};

/// Everything loaded from the user's Console install, shared by frames.
pub struct Assets {
    pub resources: Resources,
    pub appearance: Appearance,
    pub images: Arc<ImageCache>,
    layouts: Mutex<HashMap<String, Option<Arc<Layout>>>>,
}

impl Assets {
    pub fn load(resources: Resources) -> Result<Self, houston::appearance::LoadError> {
        let appearance = Appearance::load(&resources)?;
        Ok(Self { images: Arc::new(ImageCache::new(resources.clone())), resources, appearance, layouts: Mutex::new(HashMap::new()) })
    }

    pub fn layout(&self, name: &str) -> Option<Arc<Layout>> {
        self.layouts.lock().entry(name.to_string()).or_insert_with(|| Layout::load(name, &self.resources).map(Arc::new)).clone()
    }
}

/// Console art is authored at 2×.
pub const ART_SCALE: f32 = 2.0;

pub struct Ui<'a> {
    pub assets: &'a Assets,
    pub list: DrawList,
    pub hits: Vec<Hit>,
    /// Wheel-scrollable areas (see `ViewState::wheel`).
    pub scrolls: Vec<ScrollRegion>,
    origin: (f32, f32),
    clips: Vec<Rect>,
}

impl<'a> Ui<'a> {
    pub fn new(assets: &'a Assets) -> Self {
        Self { assets, list: DrawList::default(), hits: Vec::new(), scrolls: Vec::new(), origin: (0.0, 0.0), clips: Vec::new() }
    }

    /// Runs `f` with local coordinates offset by (dx, dy).
    pub fn at<R>(&mut self, dx: f32, dy: f32, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.origin;
        self.origin = (saved.0 + dx, saved.1 + dy);
        let r = f(self);
        self.origin = saved;
        r
    }

    pub fn abs(&self, r: Rect) -> Rect {
        r.offset(self.origin.0, self.origin.1)
    }

    pub fn skin(&self, section: &str, key: &str) -> Option<&'a Skin> {
        self.assets.appearance.skin(section, key)
    }

    /// Natural size of an image in points (image px / 2).
    pub fn image_size(&self, key: &str) -> Option<(f32, f32)> {
        let img = self.assets.images.get(key)?;
        Some((img.width as f32 / ART_SCALE, img.height as f32 / ART_SCALE))
    }

    // MARK: primitives

    pub fn fill(&mut self, r: Rect, color: Rgba) {
        let rect = self.abs(r);
        self.list.push(Draw::Fill { rect, color });
    }

    pub fn image(&mut self, key: &str, dst: Rect, slice: Option<Insets>, frame: Option<Frame>, opacity: f32) {
        let dst = self.abs(dst);
        self.list.push(Draw::Image { key: key.into(), frame, dst, slice, tint: Rgba::WHITE.with_alpha(opacity) });
    }

    /// Image at its natural size, centred in `r`.
    pub fn image_centered(&mut self, key: &str, r: Rect, opacity: f32) {
        if let Some((w, h)) = self.image_size(key) {
            self.image(key, r.centered(w, h), None, None, opacity);
        }
    }

    /// houston `stretch`: corners keep their size (12 px, or less for small images).
    pub fn image_stretched(&mut self, key: &str, r: Rect, opacity: f32) {
        let Some(img) = self.assets.images.get(key) else { return };
        let c = 12f32.min(img.width as f32 / 2.0 - 1.0).min(img.height as f32 / 2.0 - 1.0).max(0.0);
        self.image(key, r, Some(Insets::uniform(c)), None, opacity);
    }

    pub fn text(&mut self, text: &str, style: TextStyle, r: Rect, align: Align) {
        if text.is_empty() {
            return;
        }
        let rect = self.abs(r);
        self.list.push(Draw::Text { text: text.to_string(), style, rect, align });
    }

    /// `Some(r)` pushes a clip (intersected with the enclosing one); `None` pops back to the
    /// enclosing clip. Calls must pair.
    pub fn clip(&mut self, r: Option<Rect>) {
        match r {
            Some(r) => {
                let mut r = self.abs(r);
                if let Some(top) = self.clips.last() {
                    r = intersect(*top, r);
                }
                self.clips.push(r);
                self.list.push(Draw::Clip(Some(r)));
            }
            None => {
                self.clips.pop();
                self.list.push(Draw::Clip(self.clips.last().copied()));
            }
        }
    }

    /// The current clip in window points, if any.
    pub fn current_clip(&self) -> Option<Rect> {
        self.clips.last().copied()
    }

    /// The rect of the most recent hit registered for `id` (popover anchors).
    pub fn hit_rect(&self, id: &ControlId) -> Option<Rect> {
        self.hits.iter().rev().find(|h| &h.id == id).map(|h| h.rect)
    }

    /// Registers a hit region, limited to the current clip (fully clipped hits are dropped).
    pub fn hit(&mut self, r: Rect, id: ControlId, kind: HitKind) {
        let mut rect = self.abs(r);
        if let Some(c) = self.clips.last() {
            rect = intersect(*c, rect);
            if rect.w <= 0.0 || rect.h <= 0.0 {
                return;
            }
        }
        self.hits.push(Hit { rect, id, kind });
    }

    pub fn scroll_region(&mut self, r: Rect, region: crate::input::ScrollTarget, max: f32) {
        let rect = self.abs(r);
        self.scrolls.push(ScrollRegion { rect, target: region, max });
    }

    // MARK: skin helpers

    /// Text style of a skin (fonts/views): family, size, weight, `color-text`.
    pub fn style(&self, section: &str, key: &str) -> TextStyle {
        let font = self.skin(section, key).map(Skin::font).unwrap_or_default();
        TextStyle {
            family: font.family.into(),
            size: font.size,
            bold: font.bold,
            italic: font.italic,
            color: font.color.unwrap_or(Rgba::WHITE),
        }
    }

    pub fn plain_style(family: &str, size: f32, bold: bool, color: Rgba) -> TextStyle {
        TextStyle { family: family.into(), size, bold, italic: false, color }
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.max_x().min(b.max_x());
    let y1 = a.max_y().min(b.max_y());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}
