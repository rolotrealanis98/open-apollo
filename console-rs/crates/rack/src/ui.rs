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

    /// Assets from already-loaded parts (tests, tools, or an image cache with extra roots).
    pub fn from_parts(resources: Resources, appearance: Appearance, images: Arc<ImageCache>) -> Self {
        Self { resources, appearance, images, layouts: Mutex::new(HashMap::new()) }
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
    /// Separate-window placement (`ViewState::windowing`); None draws every layer in place.
    pub windowing: Option<crate::layers::Windowing>,
    /// Console's separate windows drawn this frame, in order (`crate::layers`).
    pub layers: Vec<crate::layers::Layer>,
    /// Which draws / hits / scroll regions belong to which layer.
    pub layer_marks: Vec<crate::layers::LayerMark>,
    /// Open layers: (layer number, the enclosing origin and clip stack to restore).
    layer_stack: Vec<(u16, (f32, f32), Vec<Rect>)>,
    /// The id and kind the next window chrome (`chrome_layer`) opens a layer as, and where its
    /// window goes when its opener anchors it.
    layer_hint: Option<(String, crate::layers::LayerKind, Option<(f32, f32)>)>,
}

impl<'a> Ui<'a> {
    pub fn new(assets: &'a Assets) -> Self {
        Self {
            assets,
            list: DrawList::default(),
            hits: Vec::new(),
            scrolls: Vec::new(),
            origin: (0.0, 0.0),
            clips: Vec::new(),
            windowing: None,
            layers: Vec::new(),
            layer_marks: Vec::new(),
            layer_stack: Vec::new(),
            layer_hint: None,
        }
    }

    // MARK: layers (separate windows)

    /// Menus, Settings and panels are separate OS windows (`ViewState::windowing`).
    pub fn windowed(&self) -> bool {
        self.windowing.is_some()
    }

    /// Where popups must stay: the screen when windowed, else `bounds` (the mixer window).
    pub fn screen_or(&self, bounds: Rect) -> Rect {
        self.windowing.as_ref().map(|w| w.screen).unwrap_or(bounds)
    }

    fn mark(&mut self, layer: u16) {
        let m = crate::layers::LayerMark { draw: self.list.len(), hit: self.hits.len(), scroll: self.scrolls.len(), layer };
        match self.layer_marks.last_mut() {
            Some(last) if (last.draw, last.hit, last.scroll) == (m.draw, m.hit, m.scroll) => *last = m,
            _ => self.layer_marks.push(m),
        }
    }

    /// Starts layer `id` (a window of Console's own: menu level, Settings, panel, dialog) whose
    /// window is `r` (local points). Windowed, a movable window opens where it was last left and
    /// a dialog centres on the screen: everything drawn until [`Ui::end_layer`] moves with it.
    /// Layers start unclipped. Returns the window rect actually used (absolute points).
    pub fn begin_layer(&mut self, id: impl Into<String>, kind: crate::layers::LayerKind, r: Rect, title_h: f32) -> Rect {
        self.begin_layer_at(id, kind, r, title_h, None)
    }

    /// [`Ui::begin_layer`] with the window's top-left at `at` (absolute points) when given.
    pub fn begin_layer_at(&mut self, id: impl Into<String>, kind: crate::layers::LayerKind, r: Rect, title_h: f32, at: Option<(f32, f32)>) -> Rect {
        use crate::layers::LayerKind as K;
        let id = id.into();
        let abs = self.abs(r);
        let anchored = at.map(|(x, y)| Rect::new(x, y, abs.w, abs.h)).unwrap_or(abs);
        let placed = match &self.windowing {
            Some(w) => {
                let to = match kind {
                    K::Window => w.positions.get(&id).map(|&(x, y)| Rect::new(x, y, abs.w, abs.h)).unwrap_or(anchored),
                    // JUCE `centreWithSize`: whole-point centre minus half the size, rounded down.
                    K::Dialog => {
                        let (cx, cy) = ((w.screen.x + w.screen.w / 2.0).floor(), (w.screen.y + w.screen.h / 2.0).floor());
                        Rect::new(cx - (abs.w / 2.0).floor(), cy - (abs.h / 2.0).floor(), abs.w, abs.h)
                    }
                    K::Menu | K::Popover => anchored,
                };
                if kind == K::Menu { to } else { crate::layers::constrain(to, w.screen) }
            }
            None => anchored,
        };
        let saved_origin = self.origin;
        self.origin = (saved_origin.0 + placed.x - abs.x, saved_origin.1 + placed.y - abs.y);
        let saved_clips = std::mem::take(&mut self.clips);
        if !saved_clips.is_empty() {
            self.list.push(Draw::Clip(None));
        }
        self.layers.push(crate::layers::Layer { id, kind, rect: placed, title_h });
        let n = self.layers.len() as u16;
        self.layer_stack.push((n, saved_origin, saved_clips));
        self.mark(n);
        placed
    }

    /// Ends the innermost open layer; drawing continues in the enclosing one.
    pub fn end_layer(&mut self) {
        let Some((_, origin, clips)) = self.layer_stack.pop() else { return };
        self.origin = origin;
        self.clips = clips;
        let parent = self.layer_stack.last().map(|l| l.0).unwrap_or(0);
        self.mark(parent);
        if let Some(c) = self.clips.last().copied() {
            self.list.push(Draw::Clip(Some(c)));
        }
    }

    /// Ends every open layer (after a panel whose chrome began one).
    pub fn end_layers(&mut self) {
        while !self.layer_stack.is_empty() {
            self.end_layer();
        }
    }

    /// The next window chrome drawn ([`Ui::chrome_layer`]) is layer `id` of `kind` (set by
    /// `build_window_with` before a sheet, panel or popover).
    pub fn layer_hint(&mut self, id: impl Into<String>, kind: crate::layers::LayerKind) {
        self.layer_hint = Some((id.into(), kind, None));
    }

    /// [`Ui::layer_hint`] for a window whose top-left Console puts at `at` (absolute points),
    /// whatever rect its chrome is drawn at.
    pub fn layer_hint_at(&mut self, id: impl Into<String>, kind: crate::layers::LayerKind, at: Option<(f32, f32)>) {
        self.layer_hint = Some((id.into(), kind, at));
    }

    pub fn clear_layer_hint(&mut self) {
        self.layer_hint = None;
    }

    /// Called by window chrome (title bar frames, dialog frames, callouts) with its window
    /// rect: opens the hinted layer, if any. Ended by `build_window_with` after the panel.
    pub fn chrome_layer(&mut self, r: Rect, title_h: f32) {
        if let Some((id, kind, at)) = self.layer_hint.take() {
            self.begin_layer_at(id, kind, r, title_h, at);
        }
    }

    /// The layer being drawn (0 = the mixer window).
    pub fn current_layer(&self) -> u16 {
        self.layer_stack.last().map(|l| l.0).unwrap_or(0)
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
