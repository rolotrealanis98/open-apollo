//! UAD Console's plug-in window: `Rack_PluginWindow` → `Rack_PluginWindowContent` → the UA
//! Plug-in Wrapper (UAPW) toolbar above the plug-in, and a status footer. Geometry and art are in
//! WINDOW.md; the art is Console's `images/uapw/*.png`, loaded from the user's install.
//!
//! The content area holds UA's HTML GUI (a webview the shell places over
//! [`WindowLayout::content`]), a spec panel, or the generic list.

use std::sync::Arc;

use houston::{Rect, Rgba};
use rack::{Action, Align, Control, ControlId, HitKind, PluginWindowButton as B, TextStyle, Ui};

use crate::generic::GenericPanel;
use crate::library::PluginSpecLibrary;
use crate::panel::{PluginPanel, action_id};
use crate::params::ParamSource;
use crate::web::{LunaComponent, LunaLibrary};

/// Chrome geometry in points. See WINDOW.md for provenance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeMetrics {
    pub title_h: f32,
    pub toolbar_h: f32,
    pub footer_h: f32,
    /// Content inset from the window's left/right edges.
    pub side: f32,
    /// Toolbar buttons: top offset within the toolbar and height.
    pub button_y: f32,
    pub button_h: f32,
    pub more_x: f32,
    pub more_w: f32,
    /// Prev, next and the preset field form one group.
    pub prev_x: f32,
    pub arrow_w: f32,
    pub preset_w: f32,
    pub gap: f32,
    pub power_w: f32,
    /// Engine button ("UAD-2 •••"), right-aligned with `right_margin`.
    pub engine_w: f32,
    pub right_margin: f32,
    /// Footer status plate height and horizontal inset.
    pub plate_h: f32,
    pub plate_inset: f32,
    pub min_width: f32,
}

/// Measured from the 1.3.1 reference capture (`plugin-window-gtr-ins4.png`, 304 pt wide).
pub const METRICS: ChromeMetrics = ChromeMetrics {
    title_h: 23.0,
    toolbar_h: 30.0,
    footer_h: 30.0,
    side: 2.0,
    button_y: 5.0,
    button_h: 22.0,
    more_x: 6.0,
    more_w: 21.5,
    prev_x: 33.0,
    arrow_w: 21.0,
    preset_w: 110.5,
    gap: 5.5,
    power_w: 21.5,
    engine_w: 58.0,
    right_margin: 6.0,
    plate_h: 22.0,
    plate_inset: 42.0,
    min_width: 304.0,
};

const WINDOW_BG: Rgba = Rgba::rgb8(64, 66, 69);
const TITLE_BG: Rgba = Rgba::rgb8(47, 50, 53);
const TOOLBAR_BG: Rgba = Rgba::rgb8(64, 66, 69);
const BUTTON_BG: Rgba = Rgba::rgb8(47, 49, 52);
const CONTENT_BG: Rgba = Rgba::rgb8(20, 21, 22);
const PLATE_RED: Rgba = Rgba::rgb8(74, 18, 21);
const TEXT: Rgba = Rgba::rgb8(222, 224, 226);

fn img(name: &str) -> String {
    format!("uapw/{name}.png")
}

pub enum Content {
    /// UA's HTML GUI; the shell's webview covers [`WindowLayout::content`].
    Web(LunaComponent),
    Spec(PluginPanel),
    Generic(GenericPanel),
}

impl Content {
    pub fn kind(&self) -> &'static str {
        match self {
            Content::Web(_) => "web",
            Content::Spec(_) => "spec",
            Content::Generic(_) => "generic",
        }
    }
}

/// Slot state shown by the chrome (from the engine: `<slot>/Preset`, `<slot>/Power`, device status).
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeState {
    /// Title bar: "<strip name>, Insert <n>" (or "…, Unison").
    pub title: String,
    pub preset: String,
    pub power: bool,
    /// The engine button's label ("UAD-2").
    pub engine: String,
    /// Footer plate text, e.g. "NO UAD-2 DEVICES FOUND"; None hides the plate.
    pub status: Option<String>,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self { title: String::new(), preset: "Default".into(), power: true, engine: "UAD-2".into(), status: None }
    }
}

/// Where everything went, in the coordinates `build` drew in.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowLayout {
    pub frame: Rect,
    pub title_bar: Rect,
    pub toolbar: Rect,
    pub content: Rect,
    pub footer: Rect,
    pub buttons: Vec<(B, Rect)>,
}

impl WindowLayout {
    pub fn button(&self, b: B) -> Option<Rect> {
        self.buttons.iter().find(|(x, _)| *x == b).map(|(_, r)| *r)
    }
}

/// What a chrome action asks of the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEvent {
    /// Consumed by the window or its content.
    Handled,
    /// Close the window.
    Close,
    /// A chrome button the shell acts on (power → `<slot>/Power`, preset prev/next, …).
    Button(B),
}

pub struct PluginWindow {
    /// The slot path; the `ControlId::strip` of every hit in this window.
    pub slot: Arc<str>,
    pub effect_name: String,
    pub chrome: ChromeState,
    pub content: Content,
    /// Placeholder overlays for the preset browser and the ⋮ menu (not implemented yet).
    pub browser_open: bool,
    pub menu_open: bool,
}

impl PluginWindow {
    /// Picks the content for `effect_name`: UA's HTML GUI when a lunacomponent matches (and
    /// `prefer_web`), else a panel spec with controls, else the generic list.
    pub fn open(
        slot: impl Into<Arc<str>>,
        effect_name: &str,
        title: &str,
        specs: Option<&PluginSpecLibrary>,
        luna: Option<&LunaLibrary>,
        prefer_web: bool,
    ) -> Self {
        let slot: Arc<str> = slot.into();
        let web = luna.filter(|_| prefer_web).and_then(|l| l.component_for_effect(effect_name)).filter(|c| c.has_images);
        let content = if let Some(c) = web {
            Content::Web(c.clone())
        } else if let Some((lib, spec)) = specs.and_then(|l| l.spec_for_effect(effect_name).map(|s| (l, s))).filter(|(_, s)| !s.elements.is_empty()) {
            Content::Spec(PluginPanel::new(spec, lib, slot.clone()))
        } else {
            Content::Generic(GenericPanel::new(slot.clone()))
        };
        Self {
            slot,
            effect_name: effect_name.into(),
            chrome: ChromeState { title: title.into(), ..Default::default() },
            content,
            browser_open: false,
            menu_open: false,
        }
    }

    /// The content's natural size in points.
    pub fn content_size(&self, params: &dyn ParamSource) -> (f32, f32) {
        match &self.content {
            Content::Web(c) => (c.window.width, c.window.height),
            Content::Spec(p) => p.size(),
            Content::Generic(g) => {
                let (w, h) = g.size(params);
                (w, h.min(600.0))
            }
        }
    }

    /// Window size for the content at its natural size.
    pub fn size(&self, params: &dyn ParamSource) -> (f32, f32) {
        let m = METRICS;
        let (cw, ch) = self.content_size(params);
        ((cw + 2.0 * m.side).max(m.min_width), m.title_h + m.toolbar_h + ch + m.footer_h)
    }

    /// Layout for a window of `size` at `origin`.
    pub fn layout(&self, origin: (f32, f32), size: (f32, f32)) -> WindowLayout {
        let m = METRICS;
        let (x, y) = origin;
        let (w, h) = size;
        let title_bar = Rect::new(x, y, w, m.title_h);
        let toolbar = Rect::new(x, title_bar.max_y(), w, m.toolbar_h);
        let footer = Rect::new(x, y + h - m.footer_h, w, m.footer_h);
        let content = Rect::new(x + m.side, toolbar.max_y(), (w - 2.0 * m.side).max(0.0), (footer.y - toolbar.max_y()).max(0.0));
        let by = toolbar.y + m.button_y;
        let bh = m.button_h;
        let mut buttons = vec![(B::Close, Rect::new(x + 4.0, y + (m.title_h - 18.0) / 2.0, 18.0, 18.0))];
        buttons.push((B::More, Rect::new(x + m.more_x, by, m.more_w, bh)));
        let prev = Rect::new(x + m.prev_x, by, m.arrow_w, bh);
        let next = Rect::new(prev.max_x(), by, m.arrow_w, bh);
        let preset = Rect::new(next.max_x(), by, m.preset_w, bh);
        let power = Rect::new(preset.max_x() + m.gap, by, m.power_w, bh);
        let engine = Rect::new(x + w - m.right_margin - m.engine_w, by, m.engine_w, bh);
        buttons.extend([(B::PresetPrev, prev), (B::PresetNext, next), (B::Preset, preset), (B::Power, power), (B::Engine, engine)]);
        if self.chrome.status.is_some() {
            let plate = Rect::new(x + m.plate_inset, footer.y + (m.footer_h - m.plate_h) / 2.0, (w - 2.0 * m.plate_inset).max(0.0), m.plate_h);
            buttons.push((B::Status, plate));
        }
        WindowLayout { frame: Rect::new(x, y, w, h), title_bar, toolbar, content, footer, buttons }
    }

    fn id(&self, control: Control) -> ControlId {
        ControlId { strip: self.slot.clone(), control }
    }

    /// Draws the window at its natural size at `origin` and registers its hits. For web content
    /// the content rect is left for the shell's webview.
    pub fn build(&self, ui: &mut Ui, origin: (f32, f32), params: &dyn ParamSource) -> WindowLayout {
        let l = self.layout(origin, self.size(params));
        ui.fill(l.frame, WINDOW_BG);
        self.title_bar(ui, &l);
        self.toolbar(ui, &l);
        ui.fill(l.content, CONTENT_BG);
        match &self.content {
            Content::Web(c) => {
                // Placeholder under the webview (also what a headless render shows).
                let style = Ui::plain_style("Helvetica", 11.0, false, TEXT.with_alpha(0.6));
                ui.text(&format!("{} (web GUI)", c.name), style, l.content, Align::Center);
            }
            Content::Spec(p) => {
                let (pw, ph) = p.size();
                p.build(ui, Rect::new(l.content.x + (l.content.w - pw).max(0.0) / 2.0, l.content.y, pw, ph), params);
            }
            Content::Generic(g) => g.build(ui, l.content, params),
        }
        if self.browser_open || self.menu_open {
            let what = if self.browser_open { "Preset browser" } else { "Plug-in menu" };
            ui.fill(l.content, Rgba::BLACK.with_alpha(0.75));
            let style = Ui::plain_style("Helvetica", 13.0, true, TEXT);
            ui.text(&format!("{what} (not implemented)"), style, l.content, Align::Center);
            ui.hit(l.content, self.id(Control::PluginWindow(if self.browser_open { B::Preset } else { B::More })), HitKind::Tap);
        }
        self.footer(ui, &l);
        l
    }

    fn title_bar(&self, ui: &mut Ui, l: &WindowLayout) {
        ui.fill(l.title_bar, TITLE_BG);
        if let Some(r) = l.button(B::Close) {
            ui.image_centered(&img("icon_close"), r, 1.0);
            ui.hit(r, self.id(Control::PluginWindow(B::Close)), HitKind::Tap);
        }
        let style = TextStyle { family: "Helvetica".into(), size: 12.0, bold: true, italic: false, color: TEXT };
        ui.text(&self.chrome.title, style, l.title_bar, Align::Center);
    }

    fn toolbar(&self, ui: &mut Ui, l: &WindowLayout) {
        ui.fill(l.toolbar, TOOLBAR_BG);
        let label = TextStyle { family: "Helvetica".into(), size: 12.0, bold: true, italic: false, color: TEXT };
        for &(b, r) in &l.buttons {
            match b {
                B::More => {
                    self.button_bg(ui, r);
                    ui.image_centered(&img("icon_more_vertical"), r, 1.0);
                }
                B::PresetPrev => {
                    self.button_bg(ui, r);
                    ui.image_centered(&img("icon_prev"), r, 1.0);
                }
                B::PresetNext => {
                    self.button_bg(ui, r);
                    ui.image_centered(&img("icon_next"), r, 1.0);
                }
                B::Preset => {
                    self.button_bg(ui, r);
                    ui.text(&self.chrome.preset, label.clone(), r, Align::Center);
                }
                B::Power => {
                    self.button_bg(ui, r);
                    ui.image_centered(&img(if self.chrome.power { "icon_power_on" } else { "icon_power" }), r, 1.0);
                }
                B::Engine => {
                    self.button_bg(ui, r);
                    let dots = ui.image_size(&img("icon_more")).unwrap_or((16.0, 6.0));
                    let text_r = Rect::new(r.x + 4.0, r.y, r.w - dots.0 - 10.0, r.h);
                    ui.text(&self.chrome.engine, label.clone(), text_r, Align::Right);
                    ui.image_centered(&img("icon_more"), Rect::new(r.max_x() - dots.0 - 5.0, r.y, dots.0, r.h), 1.0);
                }
                _ => continue,
            }
            ui.hit(r, self.id(Control::PluginWindow(b)), HitKind::Tap);
        }
    }

    /// Toolbar button background: `btn_header_bg_{l,c,r}` (3-slice), else a flat fill.
    fn button_bg(&self, ui: &mut Ui, r: Rect) {
        if ui.assets.images.get(&img("btn_header_bg")).is_some() {
            ui.image_stretched(&img("btn_header_bg"), r, 1.0);
        } else {
            ui.fill(r, BUTTON_BG);
        }
    }

    fn footer(&self, ui: &mut Ui, l: &WindowLayout) {
        ui.fill(l.footer, TOOLBAR_BG);
        let (Some(text), Some(r)) = (&self.chrome.status, l.button(B::Status)) else { return };
        if ui.assets.images.get(&img("btn_header_error_bg")).is_some() {
            ui.image_stretched(&img("btn_header_error_bg"), r, 1.0);
        } else {
            ui.fill(r, PLATE_RED);
        }
        let style = TextStyle { family: "Helvetica".into(), size: 12.0, bold: true, italic: false, color: TEXT };
        ui.text(text, style, r, Align::Center);
    }

    /// Applies an action aimed at this window (chrome or content).
    pub fn apply(&mut self, action: &Action, params: &mut dyn ParamSource) -> Option<WindowEvent> {
        let id = action_id(action);
        if id.strip != self.slot {
            return None;
        }
        if let Control::PluginWindow(b) = id.control {
            if !matches!(action, Action::Tap(_) | Action::Toggle(_)) {
                return Some(WindowEvent::Handled);
            }
            return Some(match b {
                B::Close => WindowEvent::Close,
                B::Preset => {
                    self.browser_open ^= true;
                    self.menu_open = false;
                    WindowEvent::Handled
                }
                B::More => {
                    self.menu_open ^= true;
                    self.browser_open = false;
                    WindowEvent::Handled
                }
                other => WindowEvent::Button(other),
            });
        }
        let handled = match &mut self.content {
            Content::Spec(p) => p.apply(action, params),
            Content::Generic(g) => g.apply(action, params),
            Content::Web(_) => false,
        };
        handled.then_some(WindowEvent::Handled)
    }
}
