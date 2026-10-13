//! A plug-in's own panel, drawn from its [`PluginSpec`] with the spec folder's sprites.
//!
//! A port of the Swift runtime (`SpecPluginViewModel` + `SpecPluginView`): parameter values (the
//! source's, else local values seeded from the spec defaults), local UI states (`states`,
//! `touchState` with timeouts and holds, `setState`/`cycleState`, `channels` and `monoSize`),
//! every action type (set, nudge, toggle, default, table, multi, setState, cycleState, copy,
//! random, snapshot, resend), frames (param, table keyed on params + states + meters, meter with
//! threshold, blink, element blink, held looks, reverse), visibility (tables, `visibleWhen`),
//! readouts (Console strings, sprite fonts, selected parameters, computed values, names, meter
//! readouts in dB with floors, static and multi-line text), modifiers (`move`, `reveal`,
//! `rotate`, `spin`), graph elements ([`crate::graph`], drawn by [`crate::raster`]), and the
//! "more" panel for parameters no element reaches.
//!
//! Elements are drawn in spec order (the original GUI's z-order); hits are searched topmost
//! first, and hidden elements (opacity ≤ 0.01) take no touches.
//!
//! **Input.** Two routes reach the panel:
//! - [`PluginPanel::pointer_down`] / [`pointer_move`](PluginPanel::pointer_move) /
//!   [`pointer_up`](PluginPanel::pointer_up) / [`tick`](PluginPanel::tick): the panel's own
//!   gestures, as SwiftUI recognises them — knob drags (vertical or horizontal, 0.005 per
//!   point) and knob taps (`tapAction`), drag zones, swipes, switch taps on segments with held
//!   looks, momentary buttons with `repeat`, long presses (≥ 0.45 s), menu pop-ups, absolute graph
//!   handle drags through the laws' inverses, and the "more" panel.
//! - `rack` hits ([`PluginPanel::build`] registers `PluginParam`/`PluginElement`/`PluginStep`)
//!   applied with [`PluginPanel::apply`]: the shell's generic route (no long presses, repeats or
//!   handle drags).
//!
//! Beyond Swift: a knob's double tap resets it to its default (Console's behaviour); stepped
//! parameters are quantised as Console does; `nudge` honours `wrap`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use houston::{Rect, Rgba};
use parking_lot::Mutex;
use rack::{Action, Align, Control, ControlId, Frame, HitKind, TextStyle, Ui};

use crate::graph::{Axes, Lookup, Scene, SceneInput};
use crate::library::PluginSpecLibrary;
use crate::meters::MeterFeed;
use crate::params::{ParamSource, clamp01, step_index, step_value};
use crate::raster::{self, Region};
use crate::readout;
use crate::spec::{self, ActionKind, Element, ElementKind, FrameSourceKind, GraphValue, PluginSpec, Row, TouchState};

/// Points of drag for the full 0…1 range (SpriteKnob: 0.005 per point).
pub const DRAG_SPAN: f32 = 200.0;
/// Long-press threshold (s).
pub const LONG_PRESS: f64 = 0.45;
/// Movement (points) that turns a tap into a drag.
pub const TAP_SLOP: f32 = 10.0;
/// Delay before a `repeat` button repeats (s).
pub const REPEAT_DELAY: f64 = 0.4;
/// `rack` element index of the "more" button.
pub const MORE_BUTTON: u16 = u16::MAX;
/// `rack` element index of an open pop-up's backdrop (tap outside closes it).
pub const POPUP_BACKDROP: u16 = u16::MAX - 1;
/// `rack` element index of the "more" list's background (absorbs taps between its rows).
pub const MORE_LIST: u16 = u16::MAX - 2;

/// Backdrop behind the panel (shows where sprites are missing).
const BACKDROP: Rgba = Rgba::rgb8(34, 36, 38);
const POPUP_BG: Rgba = Rgba { r: 0.13, g: 0.13, b: 0.14, a: 0.97 };
const POPUP_LINE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.12 };
const ACCENT: Rgba = Rgba { r: 0.04, g: 0.52, b: 1.0, a: 1.0 };
const MORE_D: f32 = 21.0;
const MORE_W: f32 = 260.0;
const MORE_ROW: f32 = 46.0;
const MENU_ROW: f32 = 20.0;

/// A pending `touchState` fallback.
#[derive(Clone, Debug)]
struct TouchTimer {
    state: String,
    generation: u64,
    due: f64,
    fallback: Option<f64>,
    hold: Option<String>,
}

#[derive(Clone, Debug)]
struct HandleHit {
    el: usize,
    x_value: Option<GraphValue>,
    y_value: Option<GraphValue>,
    axes: Axes,
    /// The graph's top-left in window points and the panel scale.
    origin: (f32, f32),
    scale: f32,
    touch: Option<TouchState>,
}

#[derive(Clone, Debug)]
enum Target {
    Knob { el: usize, param: u16, horizontal: bool },
    DragZone { el: usize, param: u16 },
    Swipe { el: usize, param: u16 },
    Switch { el: usize, param: u16 },
    Button { el: usize },
    Zone { el: usize },
    Menu { el: usize },
    Handle(Box<HandleHit>),
    MoreButton,
    MoreSlider { param: u16, track: Rect },
    MoreStep { param: u16, step: usize },
    MoreList,
    MenuRow { el: usize, param: u16, step: usize },
    Dismiss,
}

impl Target {
    fn element(&self) -> Option<usize> {
        match self {
            Target::Knob { el, .. } | Target::DragZone { el, .. } | Target::Swipe { el, .. } | Target::Switch { el, .. } | Target::Button { el } | Target::Zone { el } | Target::Menu { el } => Some(*el),
            Target::Handle(h) => Some(h.el),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct PanelHit {
    rect: Rect,
    /// A round hit area (graph handles): centre and radius.
    circle: Option<(f32, f32, f32)>,
    target: Target,
}

impl PanelHit {
    fn contains(&self, x: f32, y: f32) -> bool {
        match self.circle {
            Some((cx, cy, r)) => (x - cx).powi(2) + (y - cy).powi(2) <= r * r,
            None => self.rect.contains(x, y),
        }
    }
}

#[derive(Clone, Debug)]
struct Gesture {
    target: Target,
    /// The hit rect (element frame after `move`) in window points.
    rect: Rect,
    start: (f32, f32),
    last: (f32, f32),
    start_value: f64,
    start_time: f64,
    moved: bool,
    long_fired: bool,
    next_repeat: Option<f64>,
    /// A parameter gesture (undo bracket) is open.
    began: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Popup {
    /// A menu element's step list.
    Menu { el: usize },
    /// The "more" panel, scrolled by `scroll` points.
    More { scroll: f32 },
}

/// Where the last build put the panel: its top-left (window points) and scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub w: f32,
    pub h: f32,
}

impl Placement {
    fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w * self.scale, self.h * self.scale)
    }
    fn map(&self, r: Rect) -> Rect {
        Rect::new(self.x + r.x * self.scale, self.y + r.y * self.scale, r.w * self.scale, r.h * self.scale)
    }
}

pub struct PluginPanel {
    pub spec: Arc<PluginSpec>,
    target: Arc<str>,
    sprites: HashMap<String, Arc<str>>,
    missing: Vec<String>,
    /// Values for parameters the source does not publish (spec defaults until changed).
    local: HashMap<u16, f64>,
    /// Local UI state (pages, tabs, band selection, `channels`); never sent to Console.
    pub ui_state: BTreeMap<String, f64>,
    /// Meter targets, needles, raw values and the analyser block (see [`crate::meters`]).
    pub meters: MeterFeed,
    unbound: Vec<u16>,
    snapshots: HashMap<String, BTreeMap<u16, f64>>,
    touch_generation: HashMap<String, u64>,
    timers: Vec<TouchTimer>,
    epoch: Instant,
    /// Seconds on the panel's clock as of the last pointer event or tick (blink, spin, timers).
    clock: f64,
    last_tick: Option<f64>,
    rng: u64,
    gesture: Option<Gesture>,
    popup: Option<Popup>,
    placement: Mutex<Option<Placement>>,
    hits: Mutex<Vec<PanelHit>>,
    /// Prefix of this panel's code-drawn images (`runtime:plugin/<slot>/<effect>/…`): stable per
    /// slot and plug-in, so a window opened again reuses its images (and the renderer's textures).
    raster_prefix: String,
    /// The image cache the panel drew into, to release its images when it closes.
    cache: Mutex<Option<std::sync::Weak<houston::ImageCache>>>,
}

static PANELS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Parameter values for graph values, readouts and visibility: the panel's view of a source.
struct Ctx<'a> {
    panel: &'a PluginPanel,
    params: &'a dyn ParamSource,
}

impl Lookup for Ctx<'_> {
    fn value(&self, k: u16) -> f64 {
        self.panel.value(k, self.params)
    }
    fn meter(&self, id: &str) -> Option<f64> {
        self.panel.meters.raw(id)
    }
}

impl SceneInput for Ctx<'_> {
    fn block(&self, id: i64) -> Option<f64> {
        self.panel.meters.block(id)
    }
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
        let ui_state = spec.states.clone().unwrap_or_default();
        let unbound = spec.unbound_parameters();
        let n = PANELS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let target: Arc<str> = target.into();
        let raster_prefix = format!("plugin/{}/{}/", target, spec.effect_name);
        Self {
            spec,
            target,
            sprites,
            missing,
            local,
            ui_state,
            meters: MeterFeed::new(),
            unbound,
            snapshots: HashMap::new(),
            touch_generation: HashMap::new(),
            timers: Vec::new(),
            epoch: Instant::now(),
            clock: 0.0,
            last_tick: None,
            rng: 0x9E37_79B9_7F4A_7C15 ^ n,
            gesture: None,
            popup: None,
            placement: Mutex::new(None),
            hits: Mutex::new(Vec::new()),
            raster_prefix,
            cache: Mutex::new(None),
        }
    }

    /// The editor size in points: `monoSize` on a mono insert, else `size`.
    pub fn size(&self) -> (f32, f32) {
        let s = self.spec.display_size(self.ui_state.get("channels").copied());
        (s.w, s.h)
    }

    pub fn target(&self) -> &Arc<str> {
        &self.target
    }

    /// Sprite names with no image file in the spec folder.
    pub fn missing_sprites(&self) -> &[String] {
        &self.missing
    }

    /// Parameters no element reaches (offered in the "more" panel).
    pub fn unbound_parameters(&self) -> &[u16] {
        &self.unbound
    }

    /// Insert channel layout: state `channels` = 1 (mono) or 2 (stereo), for specs that declare it.
    pub fn set_channel_layout(&mut self, stereo: bool) {
        if self.spec.states.as_ref().is_some_and(|s| s.contains_key("channels")) {
            self.ui_state.insert("channels".into(), if stereo { 2.0 } else { 1.0 });
        }
    }

    /// Where the last [`build`](Self::build) placed the panel.
    pub fn placement(&self) -> Option<Placement> {
        *self.placement.lock()
    }

    /// Seconds on the panel clock (monotonic, from creation).
    pub fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// The clock blink and spin are drawn at (set by [`tick`](Self::tick) and pointer events).
    pub fn clock(&self) -> f64 {
        self.clock
    }

    /// Seeds the `random` action (tests and reproducible renders).
    pub fn seed(&mut self, seed: u64) {
        self.rng = seed.max(1);
    }

    fn id(&self, control: Control) -> ControlId {
        ControlId { strip: self.target.clone(), control }
    }

    fn ctx<'a>(&'a self, params: &'a dyn ParamSource) -> Ctx<'a> {
        Ctx { panel: self, params }
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

    /// Readout: the source's display text (Console `StringValue`), else the step label, else a
    /// percentage (Swift's `displayString`).
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

    /// The parameter a readout shows: the one its `select` state picks, else its own.
    pub fn readout_param(&self, el: &Element) -> Option<u16> {
        if let Some(sel) = el.text.as_ref().and_then(|t| t.select.as_ref()) {
            let i = self.ui_state.get(&sel.state).copied().unwrap_or(-1.0).round();
            if i >= 0.0 && (i as usize) < sel.params.len() {
                return Some(sel.params[i as usize]);
            }
        }
        el.param
    }

    /// A readout's text (Swift's `readoutText`): static text, a name picked by a value, a meter
    /// readout, a computed value, else the shown parameter's Console string.
    pub fn readout_text(&self, el: &Element, params: &dyn ParamSource) -> String {
        let ctx = self.ctx(params);
        let style = el.text.as_ref();
        if let Some(fixed) = style.and_then(|t| t.string.as_ref()) {
            return fixed.clone();
        }
        if let (Some(names), Some(v)) = (style.and_then(|t| t.names.as_ref()).filter(|n| !n.is_empty()), style.and_then(|t| t.value.as_ref())) {
            let i = v.resolve(&ctx).round();
            return if i >= 0.0 && (i as usize) < names.len() { names[i as usize].clone() } else { String::new() };
        }
        if let (Some(id), Some(t)) = (style.and_then(|t| t.meter.as_ref()), style) {
            let raw = self.meters.raw(id).unwrap_or(0.0);
            let level = t.db.map(|db| db * raw.max(1e-12).log10()).unwrap_or(raw);
            let mut v = level * t.scale.unwrap_or(1.0) + t.offset.unwrap_or(0.0);
            if let Some(f) = t.floor {
                if v < f {
                    return t.floor_text.clone().unwrap_or_default();
                }
            }
            if let Some(lo) = t.min {
                v = v.max(lo);
            }
            if let Some(hi) = t.max {
                v = v.min(hi);
            }
            return readout::format_number(t.format.as_deref().unwrap_or("%.0f"), v);
        }
        let Some(param) = self.readout_param(el) else { return String::new() };
        if let Some(v) = style.and_then(|t| t.value.as_ref()) {
            return readout::format_number(style.and_then(|t| t.format.as_deref()).unwrap_or("%.0f"), v.resolve(&ctx));
        }
        self.display(param, params)
    }

    /// A lookup key: the params' values, then the states (absent: 0), then the meter needles.
    fn key(&self, ps: &[u16], states: Option<&[String]>, meters: Option<&[String]>, params: &dyn ParamSource) -> Vec<f64> {
        let mut key: Vec<f64> = ps.iter().map(|&k| self.value(k, params)).collect();
        key.extend(states.unwrap_or(&[]).iter().map(|s| self.ui_state.get(s).copied().unwrap_or(0.0)));
        key.extend(meters.unwrap_or(&[]).iter().map(|m| self.meters.needle(m)));
        key
    }

    /// Element opacity: 0 while `visibleWhen` is off, else the nearest visibility row's
    /// opacity, else the element's own (default 1).
    pub fn opacity(&self, el: &Element, params: &dyn ParamSource) -> f64 {
        if let Some(when) = &el.visible_when {
            if !GraphValue::is_on(Some(when), &self.ctx(params)) {
                return 0.0;
            }
        }
        if let Some(vis) = &el.visibility {
            let key = self.key(&vis.params, vis.states.as_deref(), vis.meters.as_deref(), params);
            if let Some(o) = Row::nearest(&vis.table, &key).and_then(|r| r.opacity) {
                return o;
            }
        }
        el.opacity.unwrap_or(1.0)
    }

    /// Frame index (Swift's `frame(of:)`): from `frameSource` (param, table, meter with
    /// optional threshold, blink), then element blink, then `reverse`. Elements without a frame
    /// source show frame 0 (knobs and switches: see [`display_frame`](Self::display_frame)).
    pub fn frame(&self, el: &Element, params: &dyn ParamSource) -> u32 {
        self.frame_of(el, el.frame_source.as_ref(), el.frame_count(), params)
    }

    fn frame_of(&self, el: &Element, source: Option<&spec::FrameSource>, frames: u32, params: &dyn ParamSource) -> u32 {
        let frames = frames.max(1) as i64;
        let scaled = |v: f64| (v * (frames - 1) as f64).round() as i64;
        let mut index = 0i64;
        if let Some(s) = source {
            match s.kind {
                FrameSourceKind::Param => {
                    if let Some(p) = s.param {
                        index = scaled(self.value(p, params));
                    }
                }
                FrameSourceKind::Table => {
                    if let Some(ps) = &s.params {
                        let key = self.key(ps, s.states.as_deref(), s.meters.as_deref(), params);
                        if let Some(row) = Row::nearest(s.table.as_deref().unwrap_or(&[]), &key) {
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
                        let m = self.meters.needle(id);
                        index = match s.threshold {
                            Some(t) => {
                                if m > t {
                                    frames - 1
                                } else {
                                    0
                                }
                            }
                            None => scaled(m),
                        };
                    }
                }
                FrameSourceKind::Blink => {
                    if let Some(p) = s.param {
                        if self.value(p, params) > 0.5 {
                            let period = s.period.unwrap_or(1.0).max(0.05);
                            index = if self.clock.rem_euclid(period) < period / 2.0 { frames - 1 } else { 0 };
                        }
                    }
                }
                FrameSourceKind::Other => {}
            }
        }
        index = index.clamp(0, frames - 1);
        if let Some(b) = el.blink {
            let period = b.period.max(0.05);
            if index > 0 && self.clock.rem_euclid(period) >= period / 2.0 {
                index = 0;
            }
        }
        let index = index as u32;
        if el.reverse == Some(true) { frames as u32 - 1 - index } else { index }
    }

    /// The frame an element is drawn with, and the number of frames its sprite is sliced into:
    /// knobs show their own parameter (`SpriteKnob`: no frame source, no reverse), switches
    /// their parameter over the resting frames (plus `pressed_frame_offset` while held), buttons
    /// frame 1 while pressed, everything else [`frame`](Self::frame).
    pub fn display_frame(&self, i: usize, params: &dyn ParamSource) -> (u32, u32) {
        let el = &self.spec.elements[i];
        let frames = el.frame_count();
        let pressed = self.pressed(i);
        match el.kind {
            ElementKind::Knob => {
                let v = el.param.map(|k| self.value(k, params)).unwrap_or(0.0);
                ((v * (frames - 1) as f64).round().clamp(0.0, (frames - 1) as f64) as u32, frames)
            }
            ElementKind::Switch => match (el.frame_source.as_ref(), el.param) {
                (None, Some(k)) => {
                    let resting = el.pressed_frame_offset.map(|o| o.min(frames)).unwrap_or(frames).max(1);
                    let f = self.frame_of(el, Some(&spec::FrameSource::param(k)), resting, params);
                    match el.pressed_frame_offset {
                        Some(o) => ((f + if pressed { o } else { 0 }).min(frames - 1), frames),
                        None => (f, frames),
                    }
                }
                _ => {
                    let f = self.frame(el, params);
                    match el.pressed_frame_offset {
                        Some(o) if pressed => ((f + o).min(frames - 1), frames),
                        _ => (f, frames),
                    }
                }
            },
            ElementKind::Button => (if pressed && frames > 1 { 1 } else { 0 }, frames),
            _ => (self.frame(el, params), frames),
        }
    }

    fn pressed(&self, i: usize) -> bool {
        self.gesture.as_ref().is_some_and(|g| g.target.element() == Some(i))
    }

    /// Rotation in degrees: `rotate`, plus `spin` turns while its condition is on.
    fn rotation(&self, el: &Element, params: &dyn ParamSource) -> Option<f64> {
        let ctx = self.ctx(params);
        let base = el.rotate.as_ref().map(|v| v.resolve(&ctx));
        if let Some(spin) = &el.spin {
            if GraphValue::is_on(spin.while_on.as_ref(), &ctx) {
                let turns = self.clock * spin.rpm / 60.0;
                return Some(base.unwrap_or(0.0) + turns.rem_euclid(1.0) * 360.0);
            }
        }
        base
    }

    // MARK: drawing

    /// Draws the panel scaled uniformly to fit `origin` (top-left anchored; pass the natural
    /// [`size`](Self::size) for 1:1) and registers its hit regions. Leaves clipping off.
    pub fn build(&self, ui: &mut Ui, origin: Rect, params: &dyn ParamSource) {
        let (sw, sh) = self.size();
        let s = if origin.w > 0.0 && origin.h > 0.0 && sw > 0.0 && sh > 0.0 { (origin.w / sw).min(origin.h / sh) } else { 1.0 };
        let place = Placement { x: origin.x, y: origin.y, scale: s, w: sw, h: sh };
        *self.placement.lock() = Some(place);
        {
            let mut cache = self.cache.lock();
            if cache.as_ref().and_then(|w| w.upgrade()).is_none_or(|c| !Arc::ptr_eq(&c, &ui.assets.images)) {
                *cache = Some(Arc::downgrade(&ui.assets.images));
            }
        }
        let panel = place.rect();
        let mut hits = Vec::new();

        ui.clip(Some(panel));
        ui.fill(panel, BACKDROP);
        if let Some(key) = self.spec.background.as_ref().and_then(|b| self.sprites.get(b)) {
            ui.image(key, place.map(Rect::new(0.0, 0.0, self.spec.size.w, self.spec.size.h)), None, None, 1.0);
        }
        for i in 0..self.spec.elements.len() {
            self.draw_element(ui, i, &place, params, &mut hits);
        }
        if !self.unbound.is_empty() {
            let r = place.map(Rect::new(sw - 4.0 - MORE_D, 4.0, MORE_D, MORE_D));
            let px = (s * 2.0) as f64;
            if let Some(key) = ui.assets.images.put_raster(&format!("plugin-ui/more/{}", px.to_bits()), px.to_bits(), || raster::more_icon(MORE_D as f64, px)) {
                ui.image(&key, r, None, None, 1.0);
            }
            ui.hit(r, self.id(Control::PluginElement(MORE_BUTTON)), HitKind::Tap);
            hits.push(PanelHit { rect: r, circle: None, target: Target::MoreButton });
        }
        match &self.popup {
            Some(Popup::Menu { el }) => self.draw_menu_popup(ui, *el, &place, params, &mut hits),
            Some(Popup::More { scroll }) => self.draw_more(ui, *scroll, &place, params, &mut hits),
            None => {}
        }
        ui.clip(None);
        *self.hits.lock() = hits;
    }

    fn sprite_key(&self, name: Option<&String>) -> Option<&Arc<str>> {
        name.and_then(|n| self.sprites.get(n))
    }

    /// One element with its modifiers: `move` shifts it, `reveal` clips it (in its unshifted
    /// frame), `rotate`/`spin` turn sprites, opacity fades it.
    fn draw_element(&self, ui: &mut Ui, i: usize, place: &Placement, params: &dyn ParamSource, hits: &mut Vec<PanelHit>) {
        let el = &self.spec.elements[i];
        let opacity = self.opacity(el, params);
        if opacity <= 0.0 {
            return;
        }
        let interactive = opacity > 0.01;
        let ctx = self.ctx(params);
        let s = place.scale;
        let shift = el.move_by.as_ref().map(|m| (m.x.as_ref().map(|v| v.resolve(&ctx)).unwrap_or(0.0), m.y.as_ref().map(|v| v.resolve(&ctx)).unwrap_or(0.0))).unwrap_or((0.0, 0.0));
        let base = el.rect();
        let local = base.offset(shift.0 as f32, shift.1 as f32);
        let r = place.map(local);
        let reveal = el.reveal.as_ref().map(|rv| {
            let (w, h) = (base.w as f64, base.h as f64);
            let l = rv.left.as_ref().map(|v| v.resolve(&ctx)).unwrap_or(0.0);
            let t = rv.top.as_ref().map(|v| v.resolve(&ctx)).unwrap_or(0.0);
            let rr = rv.right.as_ref().map(|v| v.resolve(&ctx)).unwrap_or(w);
            let b = rv.bottom.as_ref().map(|v| v.resolve(&ctx)).unwrap_or(h);
            place.map(Rect::new(base.x + l as f32, base.y + t as f32, (rr - l).max(0.0) as f32, (b - t).max(0.0) as f32))
        });
        if let Some(clip) = reveal {
            ui.clip(Some(clip));
        }
        let rotation = self.rotation(el, params).filter(|d| d.rem_euclid(360.0).abs() > 1e-9);
        let o = opacity as f32;
        let ei = i.min(MORE_LIST as usize - 1) as u16;
        let sprite = self.sprite_key(el.sprite.as_ref());
        match el.kind {
            ElementKind::Knob => {
                if let (Some(key), Some(k)) = (sprite, el.param) {
                    let (f, n) = self.display_frame(i, params);
                    self.draw_sprite(ui, i, key, n, f, r, o, rotation, s);
                    if interactive {
                        let horizontal = el.drag.as_deref() == Some("horizontal");
                        self.rack_drag(ui, r, k, horizontal, s, params);
                        hits.push(PanelHit { rect: r, circle: None, target: Target::Knob { el: i, param: k, horizontal } });
                    }
                }
            }
            ElementKind::Switch => {
                if let (Some(key), Some(k)) = (sprite, el.param) {
                    let (f, n) = self.display_frame(i, params);
                    self.draw_sprite(ui, i, key, n, f, r, o, rotation, s);
                    if interactive {
                        self.rack_switch(ui, el, ei, k, r, params);
                        hits.push(PanelHit { rect: r, circle: None, target: Target::Switch { el: i, param: k } });
                    }
                }
            }
            ElementKind::Filmstrip => {
                if let Some(key) = sprite {
                    let (f, n) = self.display_frame(i, params);
                    self.draw_sprite(ui, i, key, n, f, r, o, rotation, s);
                }
            }
            ElementKind::Image => {
                if let Some(key) = sprite {
                    if let Some(img) = ui.assets.images.get(key) {
                        // Natural size, clipped to the frame; a rotation turns the frame about its
                        // centre after the clip (sprites the size of their frame).
                        let dst = Rect::new(r.x, r.y, img.width as f32 / 2.0 * s, img.height as f32 / 2.0 * s);
                        match rotation {
                            Some(deg) => self.draw_rotated(ui, i, key, None, dst, o, deg, s),
                            None => {
                                ui.clip(Some(r));
                                ui.image(key, dst, None, None, o);
                                ui.clip(None);
                            }
                        }
                    }
                }
            }
            ElementKind::Button => {
                if let (Some(key), Some(_)) = (sprite, el.action.as_ref()) {
                    let (f, n) = self.display_frame(i, params);
                    self.draw_sprite(ui, i, key, n, f, r, o, rotation, s);
                    if interactive {
                        ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                        hits.push(PanelHit { rect: r, circle: None, target: Target::Button { el: i } });
                    }
                }
            }
            ElementKind::Zone => {
                if interactive {
                    match (el.gesture.as_deref(), el.param) {
                        (Some("drag"), Some(k)) => {
                            self.rack_drag(ui, r, k, false, s, params);
                            hits.push(PanelHit { rect: r, circle: None, target: Target::DragZone { el: i, param: k } });
                        }
                        (Some("swipe"), Some(k)) => hits.push(PanelHit { rect: r, circle: None, target: Target::Swipe { el: i, param: k } }),
                        _ => {
                            if el.action.is_some() {
                                ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                                hits.push(PanelHit { rect: r, circle: None, target: Target::Zone { el: i } });
                            }
                        }
                    }
                }
            }
            ElementKind::Text => {
                let style = el.text.as_ref();
                if style.is_some_and(|t| t.meter.is_some() || t.string.is_some()) || self.readout_param(el).is_some() {
                    let text = self.readout_text(el, params);
                    match style.and_then(|t| t.sprite_font.as_ref()) {
                        Some(font) => self.draw_sprite_text(ui, &text, font, r, o, s),
                        None => draw_text(ui, &text, style, r, o, s),
                    }
                }
            }
            ElementKind::Menu => {
                if let Some(k) = el.param.filter(|&k| self.steps(k, params) > 1) {
                    if el.text.is_some() {
                        draw_text(ui, &self.display(k, params), el.text.as_ref(), r, o, s);
                    }
                    if interactive {
                        ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
                        hits.push(PanelHit { rect: r, circle: None, target: Target::Menu { el: i } });
                    }
                }
            }
            ElementKind::Graph => {
                if let (Some(g), true) = (&el.graph, interactive) {
                    self.draw_graph(ui, i, g, r, place, o, params, hits);
                }
            }
            ElementKind::Other => {}
        }
        if reveal.is_some() {
            ui.clip(None);
        }
    }

    /// A filmstrip frame stretched over `r` (or rotated about its centre).
    #[allow(clippy::too_many_arguments)]
    fn draw_sprite(&self, ui: &mut Ui, i: usize, key: &Arc<str>, frames: u32, index: u32, r: Rect, opacity: f32, rotation: Option<f64>, s: f32) {
        let frame = film_frame(ui, key, frames, index);
        match rotation {
            Some(deg) => self.draw_rotated(ui, i, key, frame, r, opacity, deg, s),
            None => ui.image(key, r, None, frame, opacity),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rotated(&self, ui: &mut Ui, i: usize, key: &Arc<str>, frame: Option<Frame>, r: Rect, opacity: f32, degrees: f64, s: f32) {
        let Some(img) = ui.assets.images.get(key) else { return };
        let rows = frame.map(|f| (f.index * f.height, f.height)).unwrap_or((0, img.height));
        let (w, h) = ((r.w / s) as f64, (r.h / s) as f64);
        let px = (s * 2.0) as f64;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(key.as_ref(), rows, w.to_bits(), h.to_bits(), degrees.to_bits(), px.to_bits()), &mut hasher);
        let fp = std::hash::Hasher::finish(&hasher);
        let mut bounds = (w, h);
        let name = format!("{}{i}/rot", self.raster_prefix);
        let drawn = ui.assets.images.put_raster(&name, fp, || raster::rotated_sprite(&img, rows, w, h, degrees, px).map(|(img, _, _)| img));
        let rad = degrees.to_radians();
        bounds = (bounds.0 * rad.cos().abs() + bounds.1 * rad.sin().abs(), bounds.0 * rad.sin().abs() + bounds.1 * rad.cos().abs());
        if let Some(k) = drawn {
            let (bw, bh) = (bounds.0 as f32 * s, bounds.1 as f32 * s);
            let (cx, cy) = r.center();
            ui.image(&k, Rect::new(cx - bw / 2.0, cy - bh / 2.0, bw, bh), None, None, opacity);
        }
    }

    /// LED/nixie readout: each character from the font's filmstrip, aligned in the frame.
    fn draw_sprite_text(&self, ui: &mut Ui, text: &str, font: &spec::SpriteFont, r: Rect, opacity: f32, s: f32) {
        let Some(key) = self.sprites.get(&font.sprite) else { return };
        let cells = readout::sprite_cells(text, font);
        let cw = font.cell.first().copied().unwrap_or(10.0) as f32 * s;
        let ch = font.cell.get(1).copied().unwrap_or(10.0) as f32 * s;
        let total = cw * cells.len() as f32;
        let x0 = match font.align.as_deref() {
            Some("left") => r.x,
            Some("center") => r.x + (r.w - total) / 2.0,
            _ => r.max_x() - total,
        };
        let y0 = r.y + (r.h - ch) / 2.0;
        ui.clip(Some(r));
        let dp = font.decimal_sprite.as_ref().and_then(|d| self.sprites.get(d));
        for (n, c) in cells.iter().enumerate() {
            let cell = Rect::new(x0 + n as f32 * cw, y0, cw, ch);
            let frame = film_frame(ui, key, font.frames, c.frame);
            ui.image(key, cell, None, frame, opacity);
            if let (true, Some(dp)) = (c.dot, dp) {
                ui.image(dp, cell, None, None, opacity);
            }
        }
        ui.clip(None);
    }

    /// A graph element: background colour, underlay sprite, the code-drawn curves and overlays,
    /// peak labels, then the handles and their labels (Swift's `SpecGraphView` order).
    #[allow(clippy::too_many_arguments)]
    fn draw_graph(&self, ui: &mut Ui, i: usize, g: &spec::Graph, r: Rect, place: &Placement, opacity: f32, params: &dyn ParamSource, hits: &mut Vec<PanelHit>) {
        let s = place.scale;
        let (w, h) = ((r.w / s) as f64, (r.h / s) as f64);
        let ctx = self.ctx(params);
        let scene = Scene::build(g, w, h, &ctx);
        if let Some(mut bg) = crate::graph::parse_hex(g.background.as_deref()) {
            bg.a *= opacity;
            ui.fill(r, bg);
        }
        if let Some(key) = self.sprite_key(g.underlay.as_ref()) {
            if let Some(img) = ui.assets.images.get(key) {
                ui.clip(Some(r));
                ui.image(key, Rect::new(r.x, r.y, img.width as f32 / 2.0 * s, img.height as f32 / 2.0 * s), None, None, opacity);
                ui.clip(None);
            }
        }
        // Element-local bounds of everything drawn, kept inside the panel.
        let panel_local = (-(r.x - place.x) as f64 / s as f64, -(r.y - place.y) as f64 / s as f64, place.w as f64, place.h as f64);
        let px = (s * 2.0) as f64;
        let region = |extra: &mut dyn Iterator<Item = (f64, f64)>| -> Option<Region> {
            let (mut x0, mut y0, mut x1, mut y1) = (0.0f64, 0.0f64, w, h);
            for (x, y) in extra {
                if x.is_finite() && y.is_finite() {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
            let (px0, py0) = (panel_local.0, panel_local.1);
            let (x0, y0) = (x0.max(px0), y0.max(py0));
            let (x1, y1) = (x1.min(px0 + panel_local.2), y1.min(py0 + panel_local.3));
            (x1 > x0 && y1 > y0).then_some(Region { x: x0.floor(), y: y0.floor(), w: (x1 - x0.floor()).ceil(), h: (y1 - y0.floor()).ceil(), px })
        };
        let to_window = |rg: &Region| Rect::new(r.x + rg.x as f32 * s, r.y + rg.y as f32 * s, rg.w as f32 * s, rg.h as f32 * s);
        if !(scene.fills.is_empty() && scene.strokes.is_empty() && scene.overlays.is_empty()) {
            let mut extra = scene.overlays.iter().flat_map(|o| o.path.coords().collect::<Vec<_>>().chunks(2).filter(|c| c.len() == 2).map(|c| (c[0], c[1])).collect::<Vec<_>>());
            if let Some(rg) = region(&mut extra) {
                let fp = raster::scene_fingerprint(&scene, &rg, false);
                if let Some(key) = ui.assets.images.put_raster(&format!("{}{i}/curves", self.raster_prefix), fp, || raster::scene_curves(&scene, &rg)) {
                    ui.image(&key, to_window(&rg), None, None, opacity);
                }
            }
        }
        for l in &scene.labels {
            let mut color = l.color;
            color.a *= opacity;
            let size = l.size as f32 * s;
            let width = readout::estimate_width(&l.text, l.size as f32, false, false) * s + 4.0 * s;
            let style = TextStyle { family: "Helvetica".into(), size, bold: false, italic: false, color };
            ui.text(&l.text, style, Rect::new(r.x + l.x as f32 * s, r.y + l.y as f32 * s, width, size * 1.25), Align::Left);
        }
        if !scene.handles.is_empty() {
            let mut extra = scene.handles.iter().flat_map(|h| [(h.x - h.radius - 1.0, h.y - h.radius - 1.0), (h.x + h.radius + 1.0, h.y + h.radius + 1.0)]);
            if let Some(rg) = region(&mut extra) {
                let fp = raster::scene_fingerprint(&scene, &rg, true);
                if let Some(key) = ui.assets.images.put_raster(&format!("{}{i}/handles", self.raster_prefix), fp, || raster::scene_handles(&scene, &rg)) {
                    ui.image(&key, to_window(&rg), None, None, opacity);
                }
            }
            for hs in &scene.handles {
                let (cx, cy) = (r.x + hs.x as f32 * s, r.y + hs.y as f32 * s);
                if let Some(label) = &hs.label {
                    let mut color = hs.label_color;
                    color.a *= opacity;
                    let size = hs.label_size as f32 * s;
                    let style = TextStyle { family: "Helvetica".into(), size, bold: true, italic: false, color };
                    ui.text(label, style, Rect::new(cx - 30.0 * s, cy - size, 60.0 * s, size * 2.0), Align::Center);
                }
                if hs.draggable {
                    let rad = hs.hit_diameter() as f32 / 2.0 * s;
                    hits.push(PanelHit {
                        rect: Rect::new(cx - rad, cy - rad, 2.0 * rad, 2.0 * rad),
                        circle: Some((cx, cy, rad)),
                        target: Target::Handle(Box::new(HandleHit {
                            el: i,
                            x_value: hs.x_value.clone(),
                            y_value: hs.y_value.clone(),
                            axes: scene.axes,
                            origin: (r.x, r.y),
                            scale: s,
                            touch: hs.touch_state.clone(),
                        })),
                    });
                }
            }
        }
    }

    /// The menu element's step list under (or above) the element.
    fn draw_menu_popup(&self, ui: &mut Ui, i: usize, place: &Placement, params: &dyn ParamSource, hits: &mut Vec<PanelHit>) {
        let Some(el) = self.spec.elements.get(i) else { return };
        let Some(k) = el.param else { return };
        let labels = self.step_labels(k, params);
        if labels.len() < 2 {
            return;
        }
        let s = place.scale;
        let panel = place.rect();
        self.backdrop(ui, panel, hits);
        let size = 12.0;
        let widest = labels.iter().map(|l| readout::estimate_width(l, size, false, false)).fold(0.0f32, f32::max);
        let w = (widest + 34.0).max(el.w.unwrap_or(0.0)).max(80.0) * s;
        let row = MENU_ROW * s;
        let h = row * labels.len() as f32 + 8.0 * s;
        let er = place.map(el.rect());
        let mut y = er.max_y() + 2.0 * s;
        if y + h > panel.max_y() && er.y - h - 2.0 * s >= panel.y {
            y = er.y - h - 2.0 * s;
        }
        let x = er.x.min(panel.max_x() - w).max(panel.x);
        let y = y.min(panel.max_y() - h).max(panel.y);
        ui.fill(Rect::new(x, y, w, h), POPUP_BG);
        let current = step_index(self.value(k, params), labels.len());
        for (n, label) in labels.iter().enumerate() {
            let rr = Rect::new(x, y + 4.0 * s + n as f32 * row, w, row);
            if n == current {
                ui.fill(rr, ACCENT.with_alpha(0.85));
                ui.text("✓", TextStyle { family: "Helvetica".into(), size: size * s, bold: true, italic: false, color: Rgba::WHITE }, Rect::new(rr.x + 4.0 * s, rr.y, 14.0 * s, rr.h), Align::Left);
            }
            ui.text(label, TextStyle { family: "Helvetica".into(), size: size * s, bold: false, italic: false, color: Rgba::WHITE }, Rect::new(rr.x + 20.0 * s, rr.y, rr.w - 24.0 * s, rr.h), Align::Left);
            ui.hit(rr, self.id(Control::PluginStep(k, n as u16)), HitKind::Tap);
            hits.push(PanelHit { rect: rr, circle: None, target: Target::MenuRow { el: i, param: k, step: n } });
        }
    }

    fn backdrop(&self, ui: &mut Ui, panel: Rect, hits: &mut Vec<PanelHit>) {
        ui.hit(panel, self.id(Control::PluginElement(POPUP_BACKDROP)), HitKind::Tap);
        hits.push(PanelHit { rect: panel, circle: None, target: Target::Dismiss });
    }

    /// The "more" panel: every unbound parameter with its readout and a slider or step row.
    fn draw_more(&self, ui: &mut Ui, scroll: f32, place: &Placement, params: &dyn ParamSource, hits: &mut Vec<PanelHit>) {
        let s = place.scale;
        let panel = place.rect();
        self.backdrop(ui, panel, hits);
        let w = MORE_W.min(place.w - 8.0) * s;
        let x = (panel.max_x() - 4.0 * s - w).max(panel.x);
        let y = panel.y + (8.0 + MORE_D) * s;
        let content = 14.0 * 2.0 + MORE_ROW * self.unbound.len() as f32;
        let h = content.min(420.0).min(place.h - 12.0 - MORE_D).max(MORE_ROW) * s;
        let list = Rect::new(x, y, w, h);
        ui.fill(list, POPUP_BG);
        ui.hit(list, self.id(Control::PluginElement(MORE_LIST)), HitKind::Tap);
        hits.push(PanelHit { rect: list, circle: None, target: Target::MoreList });
        ui.clip(Some(list));
        let text = |size: f32, bold: bool, color: Rgba| TextStyle { family: "Helvetica".into(), size: size * s, bold, italic: false, color };
        for (n, &k) in self.unbound.iter().enumerate() {
            let top = y + (14.0 + n as f32 * MORE_ROW - scroll) * s;
            let row = Rect::new(x + 14.0 * s, top, w - 28.0 * s, MORE_ROW * s);
            if row.max_y() < list.y || row.y > list.max_y() {
                continue;
            }
            let name = self.spec.parameter(k).map(|p| p.name.clone()).or_else(|| params.name(k)).unwrap_or_else(|| format!("Parameter {k}"));
            ui.text(&name, text(12.0, true, Rgba::WHITE), Rect::new(row.x, row.y, row.w * 0.6, 16.0 * s), Align::Left);
            let mono = TextStyle { family: "Menlo".into(), ..text(11.0, false, Rgba::WHITE.with_alpha(0.8)) };
            ui.text(&self.display(k, params), mono, Rect::new(row.x + row.w * 0.4, row.y, row.w * 0.6, 16.0 * s), Align::Right);
            let control = Rect::new(row.x, row.y + 18.0 * s, row.w, 20.0 * s);
            let labels = self.step_labels(k, params);
            if labels.len() > 1 {
                let current = step_index(self.value(k, params), labels.len());
                let seg = control.w / labels.len() as f32;
                for (j, l) in labels.iter().enumerate() {
                    let cell = Rect::new(control.x + j as f32 * seg, control.y, seg - 1.0 * s, control.h);
                    ui.fill(cell, if j == current { ACCENT } else { POPUP_LINE });
                    let size = readout::fitted_size(l, seg / s - 4.0, 10.0, false, false);
                    ui.text(l, text(size, false, Rgba::WHITE), cell, Align::Center);
                    ui.hit(cell, self.id(Control::PluginStep(k, j as u16)), HitKind::Tap);
                    hits.push(PanelHit { rect: cell, circle: None, target: Target::MoreStep { param: k, step: j } });
                }
            } else {
                let v = self.value(k, params) as f32;
                let track = Rect::new(control.x + 8.0 * s, control.y + control.h / 2.0 - 1.5 * s, control.w - 16.0 * s, 3.0 * s);
                ui.fill(track, POPUP_LINE);
                ui.fill(Rect::new(track.x, track.y, track.w * v, track.h), ACCENT);
                ui.fill(Rect::new(track.x + track.w * v - 6.0 * s, control.y + 2.0 * s, 12.0 * s, control.h - 4.0 * s), Rgba::WHITE);
                let reset = self.spec.parameter(k).and_then(|p| p.default_value);
                ui.hit(control, self.id(Control::PluginParam(k)), HitKind::Drag { value: v as f64, span: track.w.max(1.0), reset });
                hits.push(PanelHit { rect: control, circle: None, target: Target::MoreSlider { param: k, track } });
            }
        }
        ui.clip(None);
    }

    /// `rack` drag hit for knobs and drag zones: [`DRAG_SPAN`] points (scaled) for the full
    /// range. rack's drag reads vertical motion plus a quarter of horizontal motion, so
    /// horizontal faders get a quarter of the span.
    fn rack_drag(&self, ui: &mut Ui, r: Rect, k: u16, horizontal: bool, scale: f32, params: &dyn ParamSource) {
        let span = DRAG_SPAN * scale * if horizontal { 0.25 } else { 1.0 };
        let reset = self.spec.parameter(k).and_then(|p| p.default_value);
        ui.hit(r, self.id(Control::PluginParam(k)), HitKind::Drag { value: self.value(k, params), span, reset });
    }

    /// `rack` hits for a switch: one per segment when taps select segments, else the element.
    fn rack_switch(&self, ui: &mut Ui, el: &Element, ei: u16, k: u16, r: Rect, params: &dyn ParamSource) {
        let n = self.steps(k, params);
        if el.action.is_none() && n > 2 {
            if el.segments.as_deref() == Some("vertical") && r.h > 0.0 {
                let h = r.h / n as f32;
                for seg in 0..n {
                    let step = if el.reverse == Some(true) { seg } else { n - 1 - seg };
                    ui.hit(Rect::new(r.x, r.y + seg as f32 * h, r.w, h), self.id(Control::PluginStep(k, step as u16)), HitKind::Tap);
                }
                return;
            }
            if (el.is_horizontal() || el.segments.as_deref() == Some("horizontal")) && r.w > 0.0 {
                let w = r.w / n as f32;
                for seg in 0..n {
                    ui.hit(Rect::new(r.x + seg as f32 * w, r.y, w, r.h), self.id(Control::PluginStep(k, seg as u16)), HitKind::Tap);
                }
                return;
            }
        }
        ui.hit(r, self.id(Control::PluginElement(ei)), HitKind::Tap);
    }

    // MARK: state

    /// An element was touched: set its touch state, and schedule the fall-back when it has a
    /// timeout (a newer touch of the same state cancels it).
    pub fn touch(&mut self, t: Option<&TouchState>) {
        let Some(t) = t else { return };
        self.ui_state.insert(t.state.clone(), t.value);
        let generation = self.touch_generation.get(&t.state).copied().unwrap_or(0) + 1;
        self.touch_generation.insert(t.state.clone(), generation);
        if let Some(timeout) = t.timeout.filter(|&x| x > 0.0) {
            self.timers.retain(|x| x.state != t.state);
            self.timers.push(TouchTimer { state: t.state.clone(), generation, due: self.clock + timeout, fallback: t.fallback, hold: t.hold.clone() });
        }
    }

    fn fire_timers(&mut self) -> bool {
        let now = self.clock;
        let (due, pending): (Vec<TouchTimer>, Vec<TouchTimer>) = std::mem::take(&mut self.timers).into_iter().partition(|t| t.due <= now);
        self.timers = pending;
        let mut changed = false;
        for t in due {
            if self.touch_generation.get(&t.state) != Some(&t.generation) {
                continue;
            }
            if t.hold.as_ref().is_some_and(|h| self.ui_state.get(h).copied().unwrap_or(0.0) > 0.5) {
                continue;
            }
            let v = t.fallback.or_else(|| self.spec.states.as_ref().and_then(|s| s.get(&t.state).copied())).unwrap_or(0.0);
            self.ui_state.insert(t.state, v);
            changed = true;
        }
        changed
    }

    // MARK: actions

    /// Runs a spec action as Swift's `perform` does (values are read as the action proceeds, so a
    /// `multi` sees its earlier parts). Writes to published parameters form one undo step.
    pub fn perform(&mut self, action: &spec::Action, params: &mut dyn ParamSource) {
        let mut tx = Tx::default();
        self.run(action, params, &mut tx);
        tx.finish(params);
    }

    fn run(&mut self, action: &spec::Action, params: &mut dyn ParamSource, tx: &mut Tx) {
        match action.kind {
            ActionKind::Multi => {
                for sub in action.actions.iter().flatten() {
                    self.run(sub, params, tx);
                }
                return;
            }
            ActionKind::Snapshot => {
                self.snapshot(action, params, tx);
                return;
            }
            ActionKind::Resend => {
                // Send the current values even though they did not change (the plug-in re-reads).
                for &p in action.params.iter().flatten() {
                    let v = self.value(p, params);
                    self.write(p, v, true, params, tx);
                }
                return;
            }
            _ => {}
        }
        if let Some(name) = &action.state {
            match action.kind {
                ActionKind::SetState => {
                    self.ui_state.insert(name.clone(), action.value.unwrap_or(0.0));
                }
                ActionKind::CycleState => {
                    let n = action.count.unwrap_or(2).max(1) as f64;
                    let v = (self.ui_state.get(name).copied().unwrap_or(0.0) + 1.0) % n;
                    self.ui_state.insert(name.clone(), v);
                }
                _ => {}
            }
            return;
        }
        let Some(param) = action.param else { return };
        let current = self.value(param, params);
        match action.kind {
            ActionKind::Set => {
                if let Some(v) = action.value {
                    self.write(param, v, false, params, tx);
                }
            }
            ActionKind::Nudge => {
                let mut v = current + action.delta.unwrap_or(0.0);
                if action.wrap == Some(true) {
                    if v < -1e-9 {
                        v = 1.0;
                    } else if v > 1.0 + 1e-9 {
                        v = 0.0;
                    }
                }
                self.write(param, v, false, params, tx);
            }
            ActionKind::Toggle => {
                let n = self.steps(param, params);
                let v = if n > 2 { step_value((step_index(current, n) + 1) % n, n) } else if current > 0.5 { 0.0 } else { 1.0 };
                self.write(param, v, false, params, tx);
            }
            ActionKind::Default => {
                let v = self.spec.parameter(param).and_then(|p| p.default_value).unwrap_or(0.0);
                self.write(param, v, false, params, tx);
            }
            ActionKind::Random => {
                let n = self.steps(param, params);
                let r = self.random();
                let v = if n > 1 { step_value(((r * n as f64) as usize).min(n - 1), n) } else { r };
                self.write(param, v, false, params, tx);
            }
            ActionKind::Copy => {
                for &t in action.to.iter().flatten() {
                    if t != param {
                        self.write(t, current, false, params, tx);
                    }
                }
            }
            ActionKind::Table => {
                let rows = action.table.as_deref().unwrap_or(&[]);
                let picked = match &action.keys {
                    Some(keys) => {
                        let key: Vec<f64> = keys.iter().map(|&k| self.value(k, params)).collect();
                        nearest_by(rows.iter().filter(|r| r.key.is_some()), |r| spec::key_distance(r.key.as_deref().unwrap_or(&[]), &key))
                    }
                    None => nearest_by(rows.iter().filter(|r| r.current.is_some()), |r| (r.current.unwrap_or(0.0) - current).abs()),
                };
                if let Some(row) = picked {
                    self.write(param, row.value, false, params, tx);
                }
            }
            _ => {}
        }
    }

    /// `snapshot`: `store` current → slot, `recall` slot → params, `switch` (store under
    /// slots[state], recall slots[value], set the state; an empty slot keeps the current values).
    fn snapshot(&mut self, action: &spec::Action, params: &mut dyn ParamSource, tx: &mut Tx) {
        let list: Vec<u16> = action.params.clone().unwrap_or_else(|| self.spec.parameters.iter().map(|p| p.index).collect());
        let store = |panel: &mut Self, slot: &str, params: &dyn ParamSource| {
            let set = list.iter().map(|&p| (p, panel.value(p, params))).collect();
            panel.snapshots.insert(slot.to_string(), set);
        };
        let recall = |panel: &mut Self, slot: &str, params: &mut dyn ParamSource, tx: &mut Tx| {
            for (p, v) in panel.snapshots.get(slot).cloned().unwrap_or_default() {
                panel.write(p, v, false, params, tx);
            }
        };
        match action.op.as_deref() {
            Some("store") => {
                if let Some(slot) = &action.slot {
                    store(self, slot, params);
                }
            }
            Some("recall") => {
                if let Some(slot) = &action.slot {
                    recall(self, slot, params, tx);
                }
            }
            Some("switch") => {
                let (Some(state), Some(slots)) = (&action.state, action.slots.as_ref().filter(|s| !s.is_empty())) else { return };
                let current = self.ui_state.get(state).copied().unwrap_or(0.0).round() as i64;
                let target = action.value.unwrap_or(0.0).round() as i64;
                if target == current || target < 0 || target as usize >= slots.len() {
                    return;
                }
                if current >= 0 && (current as usize) < slots.len() {
                    store(self, &slots[current as usize], params);
                }
                recall(self, &slots[target as usize], params, tx);
                self.ui_state.insert(state.clone(), target as f64);
            }
            _ => {}
        }
    }

    /// Snapshot slot contents (A/B compare), by slot name.
    pub fn snapshot_slot(&self, slot: &str) -> Option<&BTreeMap<u16, f64>> {
        self.snapshots.get(slot)
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// One value write: clamped, skipped when unchanged (unless `force`), to the source when it
    /// publishes the parameter (inside the open undo step), else local.
    fn write(&mut self, k: u16, v: f64, force: bool, params: &mut dyn ParamSource, tx: &mut Tx) {
        let v = clamp01(v);
        if !force && self.value(k, params) == v {
            return;
        }
        if self.is_bound(k, params) {
            if !tx.began {
                params.begin_gesture();
                tx.began = true;
            }
            params.set(k, v);
        } else {
            self.local.insert(k, v);
        }
    }

    /// A discrete change: one undo step (skipped when unchanged).
    pub fn commit(&mut self, k: u16, v: f64, params: &mut dyn ParamSource) {
        let mut tx = Tx::default();
        self.write(k, v, false, params, &mut tx);
        tx.finish(params);
    }

    /// Moves a parameter by whole steps (continuous ones by 1/100), clamped.
    pub fn step(&mut self, k: u16, delta: i32, params: &mut dyn ParamSource) {
        let n = self.steps(k, params);
        let unit = if n > 1 { 1.0 / (n - 1) as f64 } else { 0.01 };
        let v = self.value(k, params) + delta as f64 * unit;
        self.commit(k, v, params);
    }

    /// A value inside an open gesture (drags): quantised for stepped parameters.
    fn drag_write(&mut self, k: u16, v: f64, params: &mut dyn ParamSource) {
        let n = self.steps(k, params);
        let v = if n > 1 { step_value(step_index(v, n), n) } else { clamp01(v) };
        if v == self.value(k, params) {
            return;
        }
        if self.is_bound(k, params) {
            params.set(k, v);
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

    /// A tap on element `i` without a location (the `rack` route): its action, else (switches)
    /// toggle/cycle, menus open their pop-up.
    pub fn tap(&mut self, i: usize, params: &mut dyn ParamSource) {
        match i as u16 {
            MORE_BUTTON => {
                self.popup = if matches!(self.popup, Some(Popup::More { .. })) { None } else { Some(Popup::More { scroll: 0.0 }) };
                return;
            }
            POPUP_BACKDROP => {
                self.popup = None;
                return;
            }
            MORE_LIST => return,
            _ => {}
        }
        let Some(el) = self.spec.elements.get(i).cloned() else { return };
        match el.kind {
            ElementKind::Switch => {
                if let Some(k) = el.param {
                    self.tap_switch(&el, k, None, params);
                }
            }
            ElementKind::Menu => self.popup = Some(Popup::Menu { el: i }),
            ElementKind::Button | ElementKind::Zone => {
                if let Some(a) = &el.action {
                    self.touch(el.touch_state.as_ref());
                    self.perform(a, params);
                }
            }
            _ => {}
        }
    }

    /// Swift's `tapSwitch`: the action; else the tapped segment (vertical: top = last step, first
    /// when `reverse`; horizontal); else toggle/cycle. `at` is element-local (points).
    fn tap_switch(&mut self, el: &Element, k: u16, at: Option<(f32, f32)>, params: &mut dyn ParamSource) {
        self.touch(el.touch_state.as_ref());
        if let Some(a) = &el.action {
            self.perform(a, params);
            return;
        }
        let n = self.steps(k, params);
        let (w, h) = (el.w.unwrap_or(0.0), el.h.unwrap_or(0.0));
        if let Some((x, y)) = at {
            if el.segments.as_deref() == Some("vertical") && n > 2 && h > 0.0 {
                let from_top = ((y / h * n as f32) as i64).clamp(0, n as i64 - 1) as usize;
                let step = if el.reverse == Some(true) { from_top } else { n - 1 - from_top };
                self.commit(k, step_value(step, n), params);
                return;
            }
            if (el.is_horizontal() || el.segments.as_deref() == Some("horizontal")) && n > 2 && w > 0.0 {
                let seg = ((x / w * n as f32) as i64).clamp(0, n as i64 - 1) as usize;
                self.commit(k, step_value(seg, n), params);
                return;
            }
        }
        self.perform(&spec::Action::new(ActionKind::Toggle, k), params);
    }

    // MARK: rack route

    /// Applies a rack [`Action`] aimed at this panel. Returns false when it is not for this
    /// panel (another strip, or a non plug-in control).
    pub fn apply(&mut self, action: &Action, params: &mut dyn ParamSource) -> bool {
        let id = action_id(action);
        if id.strip != self.target {
            return false;
        }
        match (action, &id.control) {
            (Action::GestureBegin(_), Control::PluginParam(k)) => {
                let touch = self.spec.elements.iter().find(|e| e.param == Some(*k) && e.touch_state.is_some()).and_then(|e| e.touch_state.clone());
                self.touch(touch.as_ref());
                params.begin_gesture();
            }
            (Action::GestureEnd(_), Control::PluginParam(_)) => params.end_gesture(),
            (Action::Set(_, v) | Action::Reset(_, v), Control::PluginParam(k)) => self.drag_write(*k, *v, params),
            (Action::Tap(_) | Action::Toggle(_), Control::PluginElement(i)) => self.tap(*i as usize, params),
            (Action::Tap(_) | Action::Toggle(_), Control::PluginStep(k, step)) => {
                if let Some(Popup::Menu { el }) = self.popup.clone() {
                    let touch = self.spec.elements.get(el).and_then(|e| e.touch_state.clone());
                    self.touch(touch.as_ref());
                    self.popup = None;
                }
                let n = self.steps(*k, params);
                self.commit(*k, step_value(*step as usize, n), params);
            }
            _ => return false,
        }
        true
    }

    // MARK: pointer route

    /// True when (x, y) (window points) is on the panel as last built.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.placement().is_some_and(|p| p.rect().contains(x, y))
    }

    /// True while a pointer gesture is in progress.
    pub fn is_tracking(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn pointer_down(&mut self, x: f32, y: f32, double: bool, params: &mut dyn ParamSource) -> bool {
        let now = self.now();
        self.pointer_down_at(x, y, double, now, params)
    }

    pub fn pointer_move(&mut self, x: f32, y: f32, params: &mut dyn ParamSource) -> bool {
        let now = self.now();
        self.pointer_move_at(x, y, now, params)
    }

    pub fn pointer_up(&mut self, x: f32, y: f32, params: &mut dyn ParamSource) -> bool {
        let now = self.now();
        self.pointer_up_at(x, y, now, params)
    }

    /// Advances the panel clock: meter ballistics, touch-state fall-backs, long presses and
    /// repeats. True when something changed (redraw).
    pub fn tick(&mut self, params: &mut dyn ParamSource) -> bool {
        let now = self.now();
        self.tick_at(now, params)
    }

    /// A pointer press at window point (x, y) at time `now` (s). True when the panel takes it.
    pub fn pointer_down_at(&mut self, x: f32, y: f32, double: bool, now: f64, params: &mut dyn ParamSource) -> bool {
        self.advance(now, params);
        if let Some(g) = self.gesture.take() {
            self.finish(g, x, y, params);
        }
        let hit = self.hits.lock().iter().rev().find(|h| h.contains(x, y)).cloned();
        let Some(hit) = hit else { return self.contains(x, y) };
        let mut g = Gesture { target: hit.target.clone(), rect: hit.rect, start: (x, y), last: (x, y), start_value: 0.0, start_time: now, moved: false, long_fired: false, next_repeat: None, began: false };
        match &hit.target {
            Target::Knob { el, param, .. } | Target::DragZone { el, param } => {
                let el = self.spec.elements[*el].clone();
                self.touch(el.touch_state.as_ref());
                params.begin_gesture();
                g.began = true;
                if double && el.kind == ElementKind::Knob {
                    let d = self.spec.parameter(*param).and_then(|p| p.default_value).unwrap_or(0.0);
                    self.drag_write(*param, d, params);
                }
                g.start_value = self.value(*param, params);
            }
            Target::Button { el } => {
                let el = self.spec.elements[*el].clone();
                if let (Some(interval), Some(a)) = (el.repeat.filter(|&r| r > 0.0), el.action.as_ref()) {
                    self.touch(el.touch_state.as_ref());
                    self.perform(a, params);
                    let _ = interval;
                    g.next_repeat = Some(now + REPEAT_DELAY);
                }
            }
            Target::Handle(h) => {
                self.touch(h.touch.as_ref());
                params.begin_gesture();
                g.began = true;
                self.drag_handle(h, x, y, params);
            }
            Target::MoreSlider { param, track } => {
                params.begin_gesture();
                g.began = true;
                let v = if track.w > 0.0 { ((x - track.x) / track.w) as f64 } else { 0.0 };
                self.drag_write(*param, v, params);
            }
            Target::MoreList => {
                if let Some(Popup::More { scroll }) = self.popup {
                    g.start_value = scroll as f64;
                }
            }
            _ => {}
        }
        self.gesture = Some(g);
        true
    }

    pub fn pointer_move_at(&mut self, x: f32, y: f32, now: f64, params: &mut dyn ParamSource) -> bool {
        self.advance(now, params);
        let Some(mut g) = self.gesture.take() else { return false };
        let s = self.placement().map(|p| p.scale).unwrap_or(1.0).max(1e-3);
        let (dx, dy) = (x - g.start.0, y - g.start.1);
        if dx.hypot(dy) > TAP_SLOP {
            g.moved = true;
        }
        g.last = (x, y);
        match &g.target {
            Target::Knob { el, param, horizontal } => {
                // SwiftUI's binding setter touches the element on every drag event.
                let touch = self.spec.elements[*el].touch_state.clone();
                let travel = if *horizontal { dx } else { -dy } / s;
                let v = clamp01(g.start_value + travel as f64 / DRAG_SPAN as f64);
                self.touch(touch.as_ref());
                self.drag_write(*param, v, params);
            }
            Target::DragZone { el, param } => {
                let touch = self.spec.elements[*el].touch_state.clone();
                let v = clamp01(g.start_value - (dy / s) as f64 / DRAG_SPAN as f64);
                self.touch(touch.as_ref());
                self.drag_write(*param, v, params);
            }
            Target::Handle(h) => {
                let h = h.clone();
                self.touch(h.touch.as_ref());
                self.drag_handle(&h, x, y, params);
            }
            Target::MoreSlider { param, track } => {
                let v = if track.w > 0.0 { ((x - track.x) / track.w) as f64 } else { 0.0 };
                self.drag_write(*param, v, params);
            }
            Target::MoreList => {
                let max = (MORE_ROW * self.unbound.len() as f32 + 28.0 - 420.0f32.min(self.size().1 - 12.0 - MORE_D)).max(0.0);
                if let Some(Popup::More { scroll }) = &mut self.popup {
                    *scroll = (g.start_value as f32 - dy / s).clamp(0.0, max);
                }
            }
            _ => {}
        }
        self.gesture = Some(g);
        true
    }

    pub fn pointer_up_at(&mut self, x: f32, y: f32, now: f64, params: &mut dyn ParamSource) -> bool {
        self.advance(now, params);
        let Some(mut g) = self.gesture.take() else { return false };
        if (x - g.start.0).hypot(y - g.start.1) > TAP_SLOP {
            g.moved = true;
        }
        self.finish(g, x, y, params);
        true
    }

    /// Ends a gesture at (x, y): releases, taps, long presses, swipes.
    fn finish(&mut self, g: Gesture, x: f32, y: f32, params: &mut dyn ParamSource) {
        let s = self.placement().map(|p| p.scale).unwrap_or(1.0).max(1e-3);
        let held = self.clock - g.start_time;
        let local = ((x - g.rect.x) / s, (y - g.rect.y) / s);
        if g.began {
            params.end_gesture();
        }
        match g.target {
            Target::Knob { el, .. } => {
                let tap = self.spec.elements[el].tap_action.clone();
                if let (Some(a), false) = (tap, g.moved) {
                    let touch = self.spec.elements[el].touch_state.clone();
                    self.touch(touch.as_ref());
                    self.perform(&a, params);
                }
            }
            Target::Swipe { el, param } => {
                let el = self.spec.elements[el].clone();
                let travel = if el.axis.as_deref() == Some("vertical") { y - g.start.1 } else { x - g.start.0 } / s;
                if (x - g.start.0).hypot(y - g.start.1) >= 8.0 && travel.abs() as f64 >= el.distance.unwrap_or(100.0) {
                    let forward = (travel > 0.0) != (el.reverse == Some(true));
                    self.touch(el.touch_state.as_ref());
                    self.step(param, if forward { 1 } else { -1 }, params);
                }
            }
            Target::Switch { el, param } => {
                if g.long_fired {
                    return;
                }
                let el = self.spec.elements[el].clone();
                let held_look = el.pressed_frame_offset.is_some();
                let long = el.long_press_action.clone().filter(|_| held >= LONG_PRESS);
                if held_look {
                    // A held switch acts on release wherever the touch ended.
                    match long {
                        Some(a) => self.perform(&a, params),
                        None => self.tap_switch(&el, param, Some(local), params),
                    }
                } else if !g.moved {
                    match long {
                        Some(a) => {
                            self.touch(el.touch_state.as_ref());
                            self.perform(&a, params);
                        }
                        None => self.tap_switch(&el, param, Some(local), params),
                    }
                }
            }
            Target::Button { el } => {
                if g.next_repeat.is_some() {
                    return;
                }
                let el = self.spec.elements[el].clone();
                match (el.long_press_action.clone().filter(|_| held >= LONG_PRESS), el.action.clone()) {
                    (Some(lp), _) => self.perform(&lp, params),
                    (None, Some(a)) => {
                        self.touch(el.touch_state.as_ref());
                        self.perform(&a, params);
                    }
                    _ => {}
                }
            }
            Target::Zone { el } => {
                if g.long_fired || g.moved {
                    return;
                }
                let el = self.spec.elements[el].clone();
                let a = match el.long_press_action.clone().filter(|_| held >= LONG_PRESS) {
                    Some(lp) => Some(lp),
                    None => el.action.clone(),
                };
                if let Some(a) = a {
                    self.touch(el.touch_state.as_ref());
                    self.perform(&a, params);
                }
            }
            Target::Menu { el } => {
                if !g.moved {
                    self.popup = Some(Popup::Menu { el });
                }
            }
            Target::MenuRow { el, param, step } => {
                if !g.moved {
                    let touch = self.spec.elements.get(el).and_then(|e| e.touch_state.clone());
                    self.touch(touch.as_ref());
                    let n = self.steps(param, params);
                    self.commit(param, step_value(step, n), params);
                    self.popup = None;
                }
            }
            Target::MoreButton => {
                if !g.moved {
                    self.popup = if matches!(self.popup, Some(Popup::More { .. })) { None } else { Some(Popup::More { scroll: 0.0 }) };
                }
            }
            Target::MoreStep { param, step } => {
                if !g.moved {
                    let n = self.steps(param, params);
                    self.commit(param, step_value(step, n), params);
                }
            }
            Target::Dismiss => {
                if !g.moved {
                    self.popup = None;
                }
            }
            Target::DragZone { .. } | Target::Handle(_) | Target::MoreSlider { .. } | Target::MoreList => {}
        }
    }

    /// Absolute handle drag: the touch point in graph coordinates through the axes and the
    /// values' inverse laws.
    fn drag_handle(&mut self, h: &HandleHit, x: f32, y: f32, params: &mut dyn ParamSource) {
        let s = h.scale.max(1e-3);
        let (lx, ly) = (((x - h.origin.0) / s) as f64, ((y - h.origin.1) / s) as f64);
        if let Some(xv) = &h.x_value {
            if let (Some(p), Some(n)) = (xv.param, xv.normalized_for(h.axes.x_value(lx))) {
                self.drag_write_raw(p, n, params);
            }
        }
        if let Some(yv) = &h.y_value {
            if let (Some(p), Some(n)) = (yv.param, yv.normalized_for(h.axes.y_value(ly))) {
                self.drag_write_raw(p, n, params);
            }
        }
    }

    /// A gesture write without quantisation (handles write through their laws).
    fn drag_write_raw(&mut self, k: u16, v: f64, params: &mut dyn ParamSource) {
        let v = clamp01(v);
        if v == self.value(k, params) {
            return;
        }
        if self.is_bound(k, params) {
            params.set(k, v);
        } else {
            self.local.insert(k, v);
        }
    }

    /// Moves the clock to `now` and runs what is due. True when something changed.
    pub fn tick_at(&mut self, now: f64, params: &mut dyn ParamSource) -> bool {
        self.advance(now, params)
    }

    fn advance(&mut self, now: f64, params: &mut dyn ParamSource) -> bool {
        let dt = self.last_tick.map(|t| (now - t).max(0.0)).unwrap_or(0.0);
        self.last_tick = Some(now);
        self.clock = now.max(self.clock);
        let mut changed = self.meters.tick(&self.spec, dt);
        changed |= self.fire_timers();
        // A long press that fires while held (zones and switches without a held look).
        if let Some(g) = self.gesture.clone() {
            if let (false, false, Some(el)) = (g.long_fired, g.moved, g.target.element()) {
                let e = self.spec.elements[el].clone();
                let held_style = matches!(g.target, Target::Zone { .. }) || (matches!(g.target, Target::Switch { .. }) && e.pressed_frame_offset.is_none());
                if held_style && self.clock - g.start_time >= LONG_PRESS {
                    if let Some(lp) = e.long_press_action.clone() {
                        self.touch(e.touch_state.as_ref());
                        self.perform(&lp, params);
                        if let Some(g) = &mut self.gesture {
                            g.long_fired = true;
                        }
                        changed = true;
                    }
                }
            }
            // Repeat buttons act every `repeat` seconds while held.
            if let (Target::Button { el }, Some(mut next)) = (&g.target, g.next_repeat) {
                let e = self.spec.elements[*el].clone();
                let interval = e.repeat.unwrap_or(0.1).max(0.01);
                let mut fired = 0;
                while next <= self.clock && fired < 100 {
                    if let Some(a) = &e.action {
                        self.touch(e.touch_state.as_ref());
                        self.perform(a, params);
                    }
                    next += interval;
                    fired += 1;
                }
                if fired > 0 {
                    if let Some(g) = &mut self.gesture {
                        g.next_repeat = Some(next);
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    /// True while the panel animates on its own (meters settling, touch timers, gestures,
    /// blinking or spinning elements): the shell should keep ticking and redrawing.
    pub fn animating(&self, params: &dyn ParamSource) -> bool {
        if self.gesture.is_some() || !self.timers.is_empty() || self.meters.settling() {
            return true;
        }
        let ctx = self.ctx(params);
        self.spec.elements.iter().any(|e| {
            e.blink.is_some()
                || e.frame_source.as_ref().is_some_and(|f| f.kind == FrameSourceKind::Blink && f.param.is_some_and(|p| self.value(p, params) > 0.5))
                || e.spin.as_ref().is_some_and(|sp| GraphValue::is_on(sp.while_on.as_ref(), &ctx))
        })
    }

    /// The meter readouts' modes need parameter values: these feed the meter model.
    pub fn ingest_floats(&mut self, raw: &str, params: &dyn ParamSource) {
        let modes = self.mode_values(params);
        let spec = self.spec.clone();
        self.meters.ingest_floats(&spec, raw, |k| modes.get(&k).copied().unwrap_or(0.0));
    }

    pub fn ingest_opcode(&mut self, p0: i64, values: &[f64], params: &dyn ParamSource) {
        let modes = self.mode_values(params);
        let spec = self.spec.clone();
        self.meters.ingest_opcode(&spec, p0, values, |k| modes.get(&k).copied().unwrap_or(0.0));
    }

    fn mode_values(&self, params: &dyn ParamSource) -> HashMap<u16, f64> {
        self.spec.meter_sources.iter().flatten().filter_map(|(_, m)| m.mode_param).map(|k| (k, self.value(k, params))).collect()
    }

    /// Is a pop-up (menu list or "more" panel) open?
    pub fn popup_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Opens or closes the "more" panel.
    pub fn show_more(&mut self, open: bool) {
        self.popup = open.then_some(Popup::More { scroll: 0.0 });
    }
}

impl PluginPanel {
    /// The prefix of this panel's code-drawn image names in the image cache.
    pub fn raster_prefix(&self) -> &str {
        &self.raster_prefix
    }
}

impl Drop for PluginPanel {
    /// Releases the panel's code-drawn images (graphs, rotated sprites) from the image cache.
    fn drop(&mut self) {
        if let Some(cache) = self.cache.lock().take().and_then(|w| w.upgrade()) {
            cache.drop_rasters(&self.raster_prefix);
        }
    }
}

/// An open undo step for the writes of one action.
#[derive(Default)]
struct Tx {
    began: bool,
}

impl Tx {
    fn finish(self, params: &mut dyn ParamSource) {
        if self.began {
            params.end_gesture();
        }
    }
}

/// The first row with the smallest distance (Swift's `min(by:)`).
fn nearest_by<'a, T>(rows: impl Iterator<Item = &'a T>, dist: impl Fn(&T) -> f64) -> Option<&'a T> {
    let mut best: Option<(&T, f64)> = None;
    for r in rows {
        let d = dist(r);
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((r, d));
        }
    }
    best.map(|(r, _)| r)
}

pub(crate) fn action_id(action: &Action) -> &ControlId {
    action.id()
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

/// `#RRGGBB` / `#RRGGBBAA`.
pub fn parse_hex(hex: Option<&str>) -> Option<Rgba> {
    crate::graph::parse_hex(hex)
}

/// A system-font readout as SwiftUI's `Text` draws it: size (default 10), weight, design,
/// colour (default white), horizontal alignment, centred vertically; one line shrinks to fit
/// (down to half), more lines wrap (`lines` 0 = unlimited).
fn draw_text(ui: &mut Ui, text: &str, t: Option<&spec::TextStyle>, r: Rect, opacity: f32, scale: f32) {
    if text.is_empty() {
        return;
    }
    let size = t.and_then(|t| t.size).unwrap_or(10.0) as f32;
    let mut color = parse_hex(t.and_then(|t| t.color.as_deref())).unwrap_or(Rgba::WHITE);
    color.a *= opacity;
    let bold = t.and_then(|t| t.weight.as_deref()).is_some_and(|w| matches!(w, "bold" | "semibold" | "heavy" | "black"));
    let mono = t.and_then(|t| t.monospaced) == Some(true);
    let family = if mono { "Menlo" } else { "Helvetica" };
    let align = match t.and_then(|t| t.align.as_deref()) {
        Some("left") => Align::Left,
        Some("right") => Align::Right,
        _ => Align::Center,
    };
    let lines = t.and_then(|t| t.lines).unwrap_or(1).max(0) as usize;
    let width = r.w / scale.max(1e-3);
    let (text, size) = if lines == 1 {
        (text.to_string(), readout::fitted_size(text, width, size, mono, bold))
    } else {
        (readout::wrap(text, width, size, mono, bold, lines).join("\n"), size)
    };
    ui.text(&text, TextStyle { family: family.into(), size: size * scale, bold, italic: false, color }, r, align);
}
