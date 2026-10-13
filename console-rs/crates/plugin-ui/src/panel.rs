//! A plug-in's own panel, drawn from its [`PluginSpec`] with the spec folder's sprites.
//!
//! Frame, visibility and action semantics follow ConsoleLab's `SpecPluginModel` (itself a port of
//! ConsoleLink's `SpecPluginViewModel`). Elements are drawn in spec order (the original GUI's
//! z-order); hit regions are registered in the same order, so the topmost element wins.

use std::collections::HashMap;
use std::sync::Arc;

use houston::{Rect, Rgba};
use rack::{Action, Align, Control, ControlId, Frame, HitKind, TextStyle, Ui};

use crate::library::PluginSpecLibrary;
use crate::params::{ParamSource, clamp01, step_index, step_value};
use crate::spec::{self, ActionKind, Element, ElementKind, FrameSourceKind, PluginSpec, Row};

/// Points of drag for the full 0…1 range (ConsoleLab: 0.005 per point).
pub const DRAG_SPAN: f32 = 200.0;

/// Backdrop behind the panel (shows where sprites are missing).
const BACKDROP: Rgba = Rgba::rgb8(34, 36, 38);

pub struct PluginPanel {
    pub spec: Arc<PluginSpec>,
    target: Arc<str>,
    sprites: HashMap<String, Arc<str>>,
    missing: Vec<String>,
    /// Values for parameters the source does not publish (spec defaults until changed).
    local: HashMap<u16, f64>,
    /// Displayed needle/LED position per LUNA meter id, 0…1 (see [`crate::meters`]).
    pub meters: HashMap<String, f64>,
}

impl PluginPanel {
    /// A panel for `spec` whose hits use `target` as the [`ControlId::strip`] (the slot path).
    /// Sprites resolve through `library` (call [`PluginSpecLibrary::attach`] on the image cache
    /// `gfx` draws with).
    pub fn new(spec: Arc<PluginSpec>, library: &PluginSpecLibrary, target: impl Into<Arc<str>>) -> Self {
        Self::with_sprites(spec, target, |name| library.sprite_key(name))
    }

    /// As [`new`](Self::new), with a custom sprite-name → image-key resolver.
    pub fn with_sprites(spec: Arc<PluginSpec>, target: impl Into<Arc<str>>, resolve: impl Fn(&str) -> Option<Arc<str>>) -> Self {
        let mut sprites = HashMap::new();
        let mut missing = Vec::new();
        for name in spec.sprite_names() {
            match resolve(name) {
                Some(key) => {
                    sprites.insert(name.to_string(), key);
                }
                None => missing.push(name.to_string()),
            }
        }
        let local = spec.parameters.iter().map(|p| (p.index, p.default_value.unwrap_or(0.0))).collect();
        Self { spec, target: target.into(), sprites, missing, local, meters: HashMap::new() }
    }

    /// Natural size in points.
    pub fn size(&self) -> (f32, f32) {
        (self.spec.size.w, self.spec.size.h)
    }

    pub fn target(&self) -> &Arc<str> {
        &self.target
    }

    /// Sprite names with no image file in the spec folder.
    pub fn missing_sprites(&self) -> &[String] {
        &self.missing
    }

    fn id(&self, control: Control) -> ControlId {
        ControlId { strip: self.target.clone(), control }
    }

    // MARK: values

    pub fn is_bound(&self, k: u16, params: &dyn ParamSource) -> bool {
        params.normalized(k).is_some()
    }

    pub fn value(&self, k: u16, params: &dyn ParamSource) -> f64 {
        params
            .normalized(k)
            .or_else(|| self.local.get(&k).copied())
            .or_else(|| self.spec.parameter(k).and_then(|p| p.default_value))
            .unwrap_or(0.0)
    }

    /// Step labels: the spec's, else the source's (empty when continuous).
    pub fn step_labels(&self, k: u16, params: &dyn ParamSource) -> Vec<String> {
        if let Some(s) = self.spec.step_labels(k) {
            return s.to_vec();
        }
        let s = params.steps(k);
        if s.len() > 1 { s } else { vec![] }
    }

    /// Number of discrete steps (0 when continuous).
    pub fn steps(&self, k: u16, params: &dyn ParamSource) -> usize {
        self.step_labels(k, params).len()
    }

    /// Readout: the source's display text, else the step label, else a percentage.
    pub fn display(&self, k: u16, params: &dyn ParamSource) -> String {
        if self.is_bound(k, params) {
            if let Some(t) = params.display(k).filter(|t| !t.is_empty()) {
                return t;
            }
        }
        let labels = self.step_labels(k, params);
        if labels.len() > 1 {
            return labels[step_index(self.value(k, params), labels.len())].clone();
        }
        format!("{}", (self.value(k, params) * 100.0).round() as i64)
    }

    fn key(&self, ks: &[u16], params: &dyn ParamSource) -> Vec<f64> {
        ks.iter().map(|&k| self.value(k, params)).collect()
    }

    pub fn opacity(&self, el: &Element, params: &dyn ParamSource) -> f64 {
        if let Some(vis) = &el.visibility {
            if let Some(o) = Row::nearest(&vis.table, &self.key(&vis.params, params)).and_then(|r| r.opacity) {
                return o;
            }
        }
        el.opacity.unwrap_or(1.0)
    }

    /// Frame index. Knobs and switches without a frame source show their own parameter; other
    /// elements follow `frameSource` (param, table or meter) and `reverse`.
    pub fn frame(&self, el: &Element, params: &dyn ParamSource) -> u32 {
        let frames = el.frame_count() as i64;
        let scaled = |v: f64| (v * (frames - 1) as f64).round() as i64;
        let mut index = 0i64;
        let own = spec::FrameSource { kind: FrameSourceKind::Param, param: el.param, params: None, table: None, meter: None };
        let source = match &el.frame_source {
            Some(s) => Some(s),
            None if matches!(el.kind, ElementKind::Knob | ElementKind::Switch) && el.param.is_some() => Some(&own),
            None => None,
        };
        if let Some(s) = source {
            match s.kind {
                FrameSourceKind::Param => {
                    if let Some(p) = s.param {
                        index = scaled(self.value(p, params));
                    }
                }
                FrameSourceKind::Table => {
                    if let (Some(ps), Some(table)) = (&s.params, &s.table) {
                        if let Some(row) = Row::nearest(table, &self.key(ps, params)) {
                            if let Some(f) = row.frame {
                                index = f as i64;
                            } else if let Some(n) = row.frame_normalized {
                                index = scaled(n);
                            }
                        }
                    }
                }
                FrameSourceKind::Meter => {
                    if let Some(id) = &s.meter {
                        index = scaled(self.meters.get(id).copied().unwrap_or(0.0));
                    }
                }
                FrameSourceKind::Other => {}
            }
        }
        let index = index.clamp(0, frames - 1) as u32;
        if el.reverse == Some(true) { frames as u32 - 1 - index } else { index }
    }

    // MARK: drawing

    /// Draws the panel scaled uniformly to fit `origin` (top-left anchored; pass the natural
    /// [`size`](Self::size) for 1:1) and registers its hit regions. Leaves clipping off.
    pub fn build(&self, ui: &mut Ui, origin: Rect, params: &dyn ParamSource) {
        let (sw, sh) = self.size();
        let s = if origin.w > 0.0 && origin.h > 0.0 && sw > 0.0 && sh > 0.0 { (origin.w / sw).min(origin.h / sh) } else { 1.0 };
        let panel = Rect::new(origin.x, origin.y, sw * s, sh * s);
        let map = |r: Rect| Rect::new(origin.x + r.x * s, origin.y + r.y * s, r.w * s, r.h * s);

        ui.clip(Some(panel));
        ui.fill(panel, BACKDROP);
        if let Some(key) = self.spec.background.as_ref().and_then(|b| self.sprites.get(b)) {
            ui.image(key, panel, None, None, 1.0);
        }
        for (i, el) in self.spec.elements.iter().enumerate() {
            let opacity = self.opacity(el, params);
            if opacity <= 0.01 {
                continue;
            }
            let r = map(el.rect());
            let ei = i.min(u16::MAX as usize) as u16;
            let sprite = el.sprite.as_ref().and_then(|n| self.sprites.get(n));
            match el.kind {
                ElementKind::Knob => {
                    let Some(k) = el.param.filter(|_| el.sprite.is_some()) else { continue };
                    self.sprite_frame(ui, sprite, el, r, opacity as f32, params);
                    self.drag_hit(ui, r, k, el.drag.as_deref() == Some("horizontal"), s, params);
                }
                ElementKind::Switch => {
                    let Some(k) = el.param.filter(|_| el.sprite.is_some()) else { continue };
                    self.sprite_frame(ui, sprite, el, r, opacity as f32, params);
                    let n = self.steps(k, params);
                    if el.action.is_none() && el.is_horizontal() && n > 2 && r.w > 0.0 {
                        let w = r.w / n as f32;
                        for seg in 0..n {
                            ui.hit(Rect::new(r.x + seg as f32 * w, r.y, w, r.h), self.id(Control::PluginStep(k, seg as u16)), HitKind::Tap);
                        }
                    } else {
                        ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                    }
                }
                ElementKind::Filmstrip => self.sprite_frame(ui, sprite, el, r, opacity as f32, params),
                ElementKind::Image => {
                    let Some(key) = sprite else { continue };
                    let Some(img) = ui.assets.images.get(key) else { continue };
                    let (w, h) = (img.width as f32 / 2.0 * s, img.height as f32 / 2.0 * s);
                    ui.clip(Some(intersect(r, panel)));
                    ui.image(key, Rect::new(r.x, r.y, w, h), None, None, opacity as f32);
                    ui.clip(None);
                }
                ElementKind::Button => {
                    if el.sprite.is_none() || el.action.is_none() {
                        continue;
                    }
                    if let Some(key) = sprite {
                        let frame = film_frame(ui, key, el.frame_count(), 0);
                        ui.image(key, r, None, frame, opacity as f32);
                    }
                    ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                }
                ElementKind::Zone => {
                    if el.gesture.as_deref() == Some("drag") {
                        if let Some(k) = el.param {
                            self.drag_hit(ui, r, k, false, s, params);
                        }
                    } else if el.action.is_some() {
                        ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                    }
                }
                ElementKind::Text => {
                    if let Some(k) = el.param {
                        let (style, align) = text_style(el.text.as_ref(), s, opacity as f32);
                        ui.text(&self.display(k, params), style, r, align);
                    }
                }
                ElementKind::Menu => {
                    let Some(k) = el.param.filter(|&k| self.steps(k, params) > 1) else { continue };
                    if el.text.is_some() {
                        let (style, align) = text_style(el.text.as_ref(), s, opacity as f32);
                        ui.text(&self.display(k, params), style, r, align);
                    }
                    ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                }
                ElementKind::Other => {}
            }
        }
        ui.clip(None);
    }

    fn sprite_frame(&self, ui: &mut Ui, key: Option<&Arc<str>>, el: &Element, r: Rect, opacity: f32, params: &dyn ParamSource) {
        let Some(key) = key else { return };
        let frame = film_frame(ui, key, el.frame_count(), self.frame(el, params));
        ui.image(key, r, None, frame, opacity);
    }

    /// Knob/zone drag: [`DRAG_SPAN`] points (scaled) for the full range. rack's drag reads
    /// vertical motion plus a quarter of horizontal motion, so horizontal faders get a quarter
    /// of the span to cover the range with sideways motion.
    fn drag_hit(&self, ui: &mut Ui, r: Rect, k: u16, horizontal: bool, scale: f32, params: &dyn ParamSource) {
        let span = DRAG_SPAN * scale * if horizontal { 0.25 } else { 1.0 };
        let reset = self.spec.parameter(k).and_then(|p| p.default_value);
        ui.hit(r, self.id(Control::PluginParam(k)), HitKind::Drag { value: self.value(k, params), span, reset });
    }

    // MARK: input

    /// Applies a rack [`Action`] aimed at this panel. Returns false when it is not for this
    /// panel (another strip, or a non plug-in control).
    pub fn apply(&mut self, action: &Action, params: &mut dyn ParamSource) -> bool {
        let id = action_id(action);
        if id.strip != self.target {
            return false;
        }
        match (action, &id.control) {
            (Action::GestureBegin(_), Control::PluginParam(_)) => params.begin_gesture(),
            (Action::GestureEnd(_), Control::PluginParam(_)) => params.end_gesture(),
            (Action::Set(_, v) | Action::Reset(_, v), Control::PluginParam(k)) => {
                let n = self.steps(*k, params);
                let v = if n > 1 { step_value(step_index(*v, n), n) } else { clamp01(*v) };
                if v != self.value(*k, params) {
                    self.write(*k, v, params);
                }
            }
            (Action::Tap(_) | Action::Toggle(_), Control::PluginElement(i)) => self.tap(*i as usize, params),
            (Action::Tap(_) | Action::Toggle(_), Control::PluginStep(k, step)) => {
                let n = self.steps(*k, params);
                self.commit(*k, step_value(*step as usize, n), params);
            }
            _ => return false,
        }
        true
    }

    /// A tap on element `i`: its action, else (menus) the next step, else toggle/cycle.
    pub fn tap(&mut self, i: usize, params: &mut dyn ParamSource) {
        let Some(el) = self.spec.elements.get(i) else { return };
        let (action, kind, param) = (el.action.clone(), el.kind, el.param);
        if let Some(a) = action {
            self.perform(&a, params, false);
        } else if let Some(k) = param {
            if matches!(kind, ElementKind::Switch | ElementKind::Menu) {
                self.perform(&spec::Action::new(ActionKind::Toggle, k), params, false);
            }
        }
    }

    /// Runs a spec action. `multi` is one gesture; its parts are plain sets inside it.
    pub fn perform(&mut self, action: &spec::Action, params: &mut dyn ParamSource, in_multi: bool) {
        if action.kind == ActionKind::Multi {
            params.begin_gesture();
            for sub in action.actions.iter().flatten() {
                self.perform(sub, params, true);
            }
            params.end_gesture();
            return;
        }
        let Some(k) = action.param else { return };
        let current = self.value(k, params);
        let target = match action.kind {
            ActionKind::Set => action.value,
            ActionKind::Nudge => Some(current + action.delta.unwrap_or(0.0)),
            ActionKind::Toggle => {
                let n = self.steps(k, params);
                Some(if n > 2 { step_value((step_index(current, n) + 1) % n, n) } else if current > 0.5 { 0.0 } else { 1.0 })
            }
            ActionKind::Default => Some(self.spec.parameter(k).and_then(|p| p.default_value).unwrap_or(0.0)),
            ActionKind::Table => action
                .table
                .iter()
                .flatten()
                .filter_map(|row| {
                    let distance = if let Some(keys) = &action.keys {
                        let key = row.key.as_ref().filter(|key| key.len() == keys.len())?;
                        keys.iter().zip(key).map(|(k, v)| (self.value(*k, params) - v).abs()).sum::<f64>()
                    } else {
                        (row.current? - current).abs()
                    };
                    Some((row, distance))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(r, _)| r.value),
            ActionKind::Multi | ActionKind::Other => None,
        };
        if let Some(v) = target {
            if in_multi {
                self.write(k, clamp01(v), params);
            } else {
                self.commit(k, v, params);
            }
        }
    }

    /// A value inside a gesture: to the source when bound, else local.
    fn write(&mut self, k: u16, v: f64, params: &mut dyn ParamSource) {
        if self.is_bound(k, params) {
            params.set(k, v);
        } else {
            self.local.insert(k, v);
        }
    }

    /// A discrete change: one undo step (skipped when unchanged).
    pub fn commit(&mut self, k: u16, v: f64, params: &mut dyn ParamSource) {
        let v = clamp01(v);
        if self.value(k, params) == v {
            return;
        }
        if self.is_bound(k, params) {
            params.commit(k, v);
        } else {
            self.local.insert(k, v);
        }
    }

    /// Sets every local (unbound) value, e.g. the all-zero state LUNA references show.
    pub fn set_all_local(&mut self, v: f64) {
        for p in &self.spec.parameters {
            self.local.insert(p.index, clamp01(v));
        }
    }
}

pub(crate) fn action_id(action: &Action) -> &ControlId {
    match action {
        Action::GestureBegin(id) | Action::Set(id, _) | Action::Reset(id, _) | Action::GestureEnd(id) | Action::Toggle(id) | Action::Tap(id) => id,
    }
}

/// `Frame` for a vertical strip of `frames` (None when it is a single image or not loaded).
fn film_frame(ui: &Ui, key: &str, frames: u32, index: u32) -> Option<Frame> {
    if frames <= 1 {
        return None;
    }
    let img = ui.assets.images.get(key)?;
    let height = (img.height / frames).max(1);
    Some(Frame { height, index: index.min(frames - 1) })
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Rect::new(x, y, (a.max_x().min(b.max_x()) - x).max(0.0), (a.max_y().min(b.max_y()) - y).max(0.0))
}

/// `#RRGGBB` / `#RRGGBBAA`.
pub fn parse_hex(hex: Option<&str>) -> Option<Rgba> {
    let h = hex?.trim().strip_prefix('#')?;
    if h.len() != 6 && h.len() != 8 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let (rgb, a) = if h.len() == 8 { (v >> 8, (v & 0xff) as f32 / 255.0) } else { (v, 1.0) };
    Some(Rgba::rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8).with_alpha(a))
}

fn text_style(t: Option<&spec::TextStyle>, scale: f32, opacity: f32) -> (TextStyle, Align) {
    let size = t.and_then(|t| t.size).unwrap_or(10.0) as f32 * scale;
    let mut color = parse_hex(t.and_then(|t| t.color.as_deref())).unwrap_or(Rgba::WHITE);
    color.a *= opacity;
    let bold = t.and_then(|t| t.weight.as_deref()).is_some_and(|w| w == "bold" || w == "medium");
    let family = if t.and_then(|t| t.monospaced) == Some(true) { "Menlo" } else { "Helvetica" };
    let align = match t.and_then(|t| t.align.as_deref()) {
        Some("left") => Align::Left,
        Some("right") => Align::Right,
        _ => Align::Center,
    };
    (TextStyle { family: family.into(), size, bold, italic: false, color }, align)
}
