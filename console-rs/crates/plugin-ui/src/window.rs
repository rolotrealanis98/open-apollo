//! UAD Console's plug-in window, rebuilt from Rack.dll (see WINDOW.md for every rule's source):
//!
//! ```text
//! Rack_PluginWindow (juce::DocumentWindow, black, 1-pt border)       W = pw + 4
//! ├─ title bar (1, 1, W−2, 22): skin Title-Bar, close, "<strip>, Insert n"
//! └─ Rack_PluginWindowContent (1, 23, cw, ch), black                cw = pw + 2
//!    ├─ Rack_NewPlugInHeader (0, 0, cw, 32): the UAPW toolbar         ch = ph + 33
//!    ├─ editor (1, 32, pw, ph): UA's HTML GUI, a spec panel or the list
//!    └─ Rack_PlugInFooter (0, ch−32, cw, 32), when shown              (+32)
//! ```
//!
//! Art is Console's `images/uapw/*.png` and Appearance skins, from the user's install.

use std::sync::Arc;

use houston::{Insets, Rect, Rgba};
use rack::{Action, Align, Control, ControlId, Hit, HitKind, PluginWindowButton as B, TextStyle, Ui};

use crate::generic::GenericPanel;
use crate::library::PluginSpecLibrary;
use crate::panel::{PluginPanel, action_id};
use crate::params::ParamSource;
use crate::web::{LunaComponent, LunaLibrary};

pub const BORDER: f32 = 1.0;
pub const TITLE_H: f32 = 22.0;
pub const HEADER_H: f32 = 32.0;
pub const FOOTER_H: f32 = 32.0;
const BUTTON_Y: f32 = 5.0;
const BUTTON_H: f32 = 22.0;

const BLACK: Rgba = Rgba::BLACK;
const TITLE_TEXT: Rgba = Rgba::rgb8(229, 245, 255);
const LABEL: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.86 };
const LABEL_ON: Rgba = Rgba { r: 1.0, g: 197.0 / 255.0, b: 74.0 / 255.0, a: 0.7 };
const SHADOW: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.4 };

fn uapw(name: &str) -> String {
    format!("uapw/{name}.png")
}

pub enum Content {
    /// UA's HTML GUI; the shell's webview covers [`WindowLayout::editor`].
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

/// Which header items the slot has (Console's visibility predicates; most are [unk] in
/// WINDOW.md, so the defaults reproduce the 1.3.1 capture of a UAD-2 plug-in).
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderItems {
    pub rack_view: bool,
    pub assign: bool,
    pub presets: bool,
    /// Sidechain source label (`SIDECHAIN: {input}`), when the effect has a sidechain.
    pub sidechain: Option<String>,
    pub power: bool,
    pub in_out: bool,
    pub compare: bool,
    pub copy_paste: bool,
    pub mixer: bool,
    /// Label on the "more" button ("UAD-2" for UAD-2 plug-ins); forces it visible.
    pub more_label: Option<String>,
    pub more_forced: bool,
}

impl Default for HeaderItems {
    fn default() -> Self {
        Self {
            rack_view: false,
            assign: true,
            presets: true,
            sidechain: None,
            power: true,
            in_out: false,
            compare: false,
            copy_paste: true,
            mixer: false,
            more_label: Some("UAD-2".into()),
            more_forced: false,
        }
    }
}

/// Slot state the chrome shows (engine: `<slot>/Preset`, `<slot>/Power`; status from the
/// license/device layer).
#[derive(Clone, Debug, PartialEq)]
pub struct ChromeState {
    /// "<strip name>, Insert <n>" (or ", Unison").
    pub title: String,
    pub preset: String,
    pub power: bool,
    pub rack_view: bool,
    pub compare_b: bool,
    /// Footer state plate text ("NO UAD-2 DEVICES FOUND"); None hides the footer.
    pub status: Option<String>,
    /// Footer plate background: 2 = error (red), 3 = blue, else neutral (`SetBgState`).
    pub status_state: u8,
    /// Footer action button ("BUY" / "START DEMO"), when shown.
    pub action: Option<String>,
    pub items: HeaderItems,
    /// Close button on the left (macOS Console, the reference capture) or right (Windows).
    pub close_on_left: bool,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            title: String::new(),
            preset: "Default".into(),
            power: true,
            rack_view: false,
            compare_b: false,
            status: None,
            status_state: 2,
            action: None,
            items: HeaderItems::default(),
            close_on_left: true,
        }
    }
}

/// Everything placed, in the coordinates `build` drew in.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowLayout {
    pub frame: Rect,
    pub title_bar: Rect,
    pub content: Rect,
    pub header: Rect,
    /// Where the plug-in editor goes (the webview's rect for HTML GUIs).
    pub editor: Rect,
    pub footer: Option<Rect>,
    pub buttons: Vec<(B, Rect)>,
    /// Header layout mode 0/1/2 (WINDOW.md §3.3).
    pub mode: u8,
    /// Header items that did not fit (reachable from "more").
    pub overflow: Vec<B>,
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
    /// A chrome button the shell acts on: `Power` → `<slot>/Power`, preset prev/next, copy,
    /// paste, compare, footer buttons, help.
    Button(B),
}

/// The header's toolbar items, left to right, in header coordinates (WINDOW.md §3.3). Returns
/// the placed buttons, the layout mode and the items that overflowed into "more".
pub fn header_layout(cw: f32, items: &HeaderItems) -> (Vec<(B, Rect)>, u8, Vec<B>) {
    let r = |x: f32, w: f32| Rect::new(x, BUTTON_Y, w, BUTTON_H);
    // Layout mode: the smallest of {0, 1} whose shown items fit, else 2.
    let width = |m: u8| {
        let mut ws = Vec::new();
        if items.rack_view {
            ws.push(22.0);
        }
        if items.assign {
            ws.push(22.0);
        }
        if items.presets {
            ws.push(if m == 2 { 153.0 } else { 263.0 });
        }
        if items.sidechain.is_some() {
            ws.push(if m == 0 { 160.0 } else { 22.0 });
        }
        for (on, w) in [(items.power, 22.0), (items.in_out, 22.0), (items.compare, 44.0), (items.copy_paste, 80.0), (items.mixer, 102.0)] {
            if on {
                ws.push(w);
            }
        }
        5.0 + ws.iter().map(|w| w + 5.0).sum::<f32>()
    };
    let mode = (0..2u8).find(|&m| width(m) <= cw).unwrap_or(2);

    let mut out = Vec::new();
    let mut x = 5.0;
    if items.rack_view {
        out.push((B::RackView, r(5.0, 22.0)));
        x = 32.0;
    }
    if items.assign {
        out.push((B::Assign, r(x, 22.0)));
        x += 27.0;
    }
    if items.presets {
        let p = if mode == 2 { 110.0 } else { 220.0 };
        out.extend([(B::PresetPrev, r(x, 22.0)), (B::PresetNext, r(x + 22.0, 21.0)), (B::Presets, r(x + 43.0, p))]);
        x += 43.0 + p + 5.0;
    }
    // Overflowable groups: (buttons as (id, dx, w), group width, advance).
    let side = if mode == 0 { 160.0 } else { 22.0 };
    let groups: Vec<(Vec<(B, f32, f32)>, f32, f32)> = [
        (items.sidechain.is_some(), vec![(B::Sidechain, 0.0, side)], side, side + 5.0),
        (items.power, vec![(B::Power, 0.0, 22.0)], 22.0, 27.0),
        (items.in_out, vec![(B::InOut, 0.0, 22.0)], 22.0, 27.0),
        (items.compare, vec![(B::CompareA, 0.0, 22.0), (B::CompareB, 22.0, 22.0)], 44.0, 49.0),
        (items.copy_paste, vec![(B::Copy, 0.0, 40.0), (B::Paste, 40.0, 40.0)], 80.0, 85.0),
        (items.mixer, vec![(B::Mixer, 0.0, 102.0)], 102.0, 107.0),
    ]
    .into_iter()
    .filter(|(on, ..)| *on)
    .map(|(_, b, w, adv)| (b, w, adv))
    .collect();
    let more_w = if items.more_label.is_some() { 58.0 } else { 22.0 };
    let place = |reserve: f32| {
        let mut placed = Vec::new();
        let mut hidden = Vec::new();
        let mut cx = x;
        let mut full = false;
        for (bs, w, adv) in &groups {
            if !full && (cw - reserve - cx) - 5.0 >= *w {
                placed.extend(bs.iter().map(|(b, dx, bw)| (*b, r(cx + dx, *bw))));
                cx += adv;
            } else {
                full = true;
                hidden.extend(bs.iter().map(|(b, ..)| *b));
            }
        }
        (placed, hidden)
    };
    // Pass 0 is skipped when "more" is forced (its label, the flag, or A/B shown).
    let forced = items.more_forced || items.more_label.is_some() || items.compare;
    let (placed, hidden) = match forced {
        false => match place(0.0) {
            (p, h) if h.is_empty() => (p, h),
            _ => place(27.0),
        },
        true => place(if items.more_label.is_some() { 63.0 } else { 27.0 }),
    };
    let more = forced || !hidden.is_empty();
    out.extend(placed);
    if more {
        out.push((B::More, r(cw - more_w - 5.0, more_w)));
    }
    (out, mode, hidden)
}

pub struct PluginWindow {
    /// The slot path; the `ControlId::strip` of every hit in this window.
    pub slot: Arc<str>,
    pub effect_name: String,
    pub chrome: ChromeState,
    pub content: Content,
    /// Open popover: `More` (the dark menu), `Assign` (the ⋮ insert menu), `Presets` (stub).
    pub popover: Option<B>,
    /// The ⋮ menu (`rack::menu::plugin_insert_menu`, s41), refreshed by the host from the
    /// mixer; its rows are mixer controls the host routes (they are not this window's).
    pub insert_menu: Option<rack::menu::PopupMenu>,
    /// "Resize Plug-In" scale (percent): spec panels and UA's HTML GUIs draw at this size.
    pub scale: u16,
    /// The insert's presets (the host fills them from the model): the tree the presets field's
    /// menu shows, and the same files flattened in list order (`PresetItem` indexes these; ‹ ›
    /// step through them).
    pub preset_tree: Vec<rack::PresetNode>,
    pub presets: Vec<String>,
    /// PASTE enabled (`CanPastePluginBarSettings`).
    pub paste_enabled: bool,
    /// The "Help & Video" panel of the more menu (s43).
    pub help_open: bool,
    /// Plug-in version for the help panel ("v1.0.0").
    pub version: Option<String>,
    /// `build` draws the open popover too (default). Hosts that clip the window to its frame
    /// turn this off and call [`PluginWindow::build_popover`] unclipped: Console's menus are
    /// their own windows and may leave the plug-in window.
    pub inline_popover: bool,
}

impl PluginWindow {
    /// Picks the content for `effect_name`: UA's HTML GUI when a lunacomponent with images
    /// matches (and `prefer_web`), else a panel spec with elements, else the generic list.
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
        let more_label = effect_name.starts_with("UAD ").then(|| "UAD-2".to_string());
        Self {
            slot,
            effect_name: effect_name.into(),
            chrome: ChromeState { title: title.into(), items: HeaderItems { more_label, ..Default::default() }, ..Default::default() },
            content,
            popover: None,
            insert_menu: None,
            scale: 100,
            preset_tree: Vec::new(),
            presets: Vec::new(),
            paste_enabled: false,
            help_open: false,
            version: None,
            inline_popover: true,
        }
    }

    /// The editor's size in points (pw × ph), at the Resize Plug-In scale (the generic list
    /// does not scale).
    pub fn editor_size(&self, params: &dyn ParamSource) -> (f32, f32) {
        let k = self.scale.clamp(25, 400) as f32 / 100.0;
        let scaled = |(w, h): (f32, f32)| ((w * k).round(), (h * k).round());
        match &self.content {
            Content::Web(c) => scaled((c.window.width, c.window.height)),
            Content::Spec(p) => scaled(p.size()),
            Content::Generic(g) => {
                let (w, h) = g.size(params);
                (w, h.min(600.0))
            }
        }
    }

    /// Window size W × H: `pw + 4` × `ph + 57`, plus 32 with the footer.
    pub fn size(&self, params: &dyn ParamSource) -> (f32, f32) {
        let (pw, ph) = self.editor_size(params);
        let footer = if self.chrome.status.is_some() { FOOTER_H } else { 0.0 };
        (pw + 2.0 + 2.0 * BORDER, ph + HEADER_H + 1.0 + footer + TITLE_H + 2.0 * BORDER)
    }

    /// Layout for an editor of `(pw, ph)` with the window's top-left at `origin`.
    pub fn layout(&self, origin: (f32, f32), editor: (f32, f32)) -> WindowLayout {
        let (ox, oy) = origin;
        let (pw, ph) = editor;
        let footer_h = if self.chrome.status.is_some() { FOOTER_H } else { 0.0 };
        let (cw, ch) = (pw + 2.0, ph + HEADER_H + 1.0 + footer_h);
        let frame = Rect::new(ox, oy, cw + 2.0 * BORDER, ch + TITLE_H + 2.0 * BORDER);
        let title_bar = Rect::new(ox + BORDER, oy + BORDER, frame.w - 2.0 * BORDER, TITLE_H);
        let content = Rect::new(ox + BORDER, title_bar.max_y(), cw, ch);
        let header = Rect::new(content.x, content.y, cw, HEADER_H);
        let editor = Rect::new(content.x + 1.0, content.y + HEADER_H, pw, ph);
        let footer = self.chrome.status.is_some().then(|| Rect::new(content.x, content.max_y() - FOOTER_H, cw, FOOTER_H));

        let (items, mode, overflow) = header_layout(cw, &self.chrome.items);
        let mut buttons: Vec<(B, Rect)> = items.into_iter().map(|(b, r)| (b, r.offset(header.x, header.y))).collect();
        let close = if self.chrome.close_on_left {
            Rect::new(ox + 6.0, oy + 2.0, 20.0, 20.0)
        } else {
            Rect::new(ox + frame.w - 24.0, oy + 3.0, 20.0, 20.0)
        };
        buttons.insert(0, (B::Close, close));
        if let Some(f) = footer {
            let state_w: f32 = 220.0;
            let action_w = if self.chrome.action.is_some() { 72.0 + 4.0 } else { 0.0 };
            let total = (state_w + action_w).min(cw - 8.0);
            let x = f.x + (cw - total) / 2.0;
            buttons.push((B::FooterState, Rect::new(x, f.y + 5.0, state_w, BUTTON_H)));
            if self.chrome.action.is_some() {
                buttons.push((B::FooterAction, Rect::new(x + state_w + 4.0, f.y + 5.0, 72.0, BUTTON_H)));
            }
        }
        WindowLayout { frame, title_bar, content, header, editor, footer, buttons, mode, overflow }
    }

    fn id(&self, control: Control) -> ControlId {
        ControlId { strip: self.slot.clone(), control }
    }

    /// Draws the window at its natural size with its top-left at `origin` and registers its
    /// hits. For web content the editor rect is left for the shell's webview.
    pub fn build(&self, ui: &mut Ui, origin: (f32, f32), params: &dyn ParamSource) -> WindowLayout {
        let l = self.layout(origin, self.editor_size(params));
        ui.fill(l.frame, BLACK);
        self.title_bar(ui, &l);
        ui.fill(l.content, BLACK);
        self.header(ui, &l);
        match &self.content {
            Content::Web(c) => {
                // The shell's webview covers this rect (or, on Wayland, sits in its own window).
                let style = Ui::plain_style("Helvetica", 11.0, false, LABEL.with_alpha(0.5));
                ui.text(&format!("{}\nUA plug-in GUI", c.name), style, l.editor, Align::Center);
            }
            Content::Spec(p) => p.build(ui, l.editor, params),
            Content::Generic(g) => g.build(ui, l.editor, params),
        }
        if let Some(f) = l.footer {
            self.footer(ui, &l, f);
        }
        if let (Some(which), true) = (self.popover, self.inline_popover) {
            self.popover(ui, &l, which);
        }
        l
    }

    /// Draws the open popover (menus) for a window at `origin`; see `inline_popover`.
    pub fn build_popover(&self, ui: &mut Ui, origin: (f32, f32), params: &dyn ParamSource) {
        if let Some(which) = self.popover {
            let l = self.layout(origin, self.editor_size(params));
            self.popover(ui, &l, which);
        }
    }

    fn title_bar(&self, ui: &mut Ui, l: &WindowLayout) {
        ui.background("Title-Bar", l.title_bar);
        let style = TextStyle { family: "Helvetica Neue".into(), size: 11.0, bold: true, italic: false, color: TITLE_TEXT };
        ui.text(&self.chrome.title, style, l.title_bar, Align::Center);
        if let Some(r) = l.button(B::Close) {
            let skin = if self.chrome.close_on_left { "Window-Button-Close" } else { "Small-Window-Button-Close-Win" };
            match ui.skin("views", skin).and_then(|s| s.image("image").or(s.image("image-off"))).map(str::to_string) {
                Some(img) => ui.image_centered(&img, r, 1.0),
                None => ui.text("×", Ui::plain_style("Helvetica", 16.0, true, TITLE_TEXT), r, Align::Center),
            }
            ui.hit(r, self.id(Control::PluginWindow(B::Close)), HitKind::Tap);
        }
    }

    /// `Rack_NewPlugInHeader`: HTML plug-ins get their manifest's `window-background` colour
    /// with a 1-pt black outline; others the `Plug-In-Header` skin.
    fn header(&self, ui: &mut Ui, l: &WindowLayout) {
        let h = l.header;
        match &self.content {
            Content::Web(c) => {
                ui.fill(h, c.colors.get("window-background").copied().unwrap_or(Rgba::rgb8(85, 85, 85)));
                ui.fill(Rect::new(h.x, h.y, 1.0, h.h), BLACK);
                ui.fill(Rect::new(h.max_x() - 1.0, h.y, 1.0, h.h), BLACK);
                ui.fill(Rect::new(h.x, h.max_y() - 1.0, h.w, 1.0), BLACK);
            }
            _ => {
                ui.fill(h, Rgba::rgb8(75, 75, 75));
                ui.background("Plug-In-Header", h);
            }
        }
        let label = TextStyle { family: "Helvetica".into(), size: 10.0, bold: true, italic: false, color: LABEL };
        let on = |b: B| match b {
            B::Power => self.chrome.power,
            B::RackView => self.chrome.rack_view,
            B::CompareB => self.chrome.compare_b,
            B::CompareA => !self.chrome.compare_b,
            B::More => self.popover == Some(B::More),
            _ => false,
        };
        for &(b, r) in &l.buttons {
            let bg = match b {
                B::PresetPrev | B::CompareA | B::Copy => "btn_header_bg_l",
                B::PresetNext => "btn_header_bg_c",
                B::Presets | B::CompareB | B::Paste => "btn_header_bg_r",
                B::Close | B::FooterState | B::FooterAction => continue,
                _ => "btn_header_bg",
            };
            nine(ui, &uapw(bg), r);
            let text = |ui: &mut Ui, s: &str, lit: bool| {
                let style = TextStyle { color: if lit { LABEL_ON } else { LABEL }, ..label.clone() };
                ui.text(s, style, r, Align::Center);
            };
            match b {
                B::RackView => ui.image_centered(&uapw(if on(b) { "plugin_rack_view_on" } else { "plugin_rack_view" }), r, 1.0),
                B::Assign => ui.image(&uapw("icon_more_vertical"), Rect::new(r.x + 9.0, r.y + 2.0, 5.0, 18.0), None, None, 1.0),
                B::PresetPrev => ui.image_centered(&uapw("icon_prev"), r, 1.0),
                B::PresetNext => ui.image_centered(&uapw("icon_next"), r, 1.0),
                B::Presets => text(ui, &self.chrome.preset, false),
                B::Sidechain => {
                    let s = self.chrome.items.sidechain.clone().unwrap_or_default();
                    text(ui, &if r.w > 22.0 { format!("SIDECHAIN: {s}") } else { "SC".into() }, false)
                }
                B::Power => ui.image_centered(&uapw(if on(b) { "icon_power_on" } else { "icon_power" }), r, 1.0),
                B::InOut => text(ui, "IN", false),
                B::CompareA => text(ui, "A", on(b)),
                B::CompareB => text(ui, "B", on(b)),
                B::Copy => text(ui, "COPY", false),
                B::Paste => {
                    let style = TextStyle { color: LABEL.with_alpha(if self.paste_enabled { LABEL.a } else { 0.35 }), ..label.clone() };
                    ui.text("PASTE", style, r, Align::Center);
                    if !self.paste_enabled {
                        continue;
                    }
                }
                B::Mixer => text(ui, "MULTI-OUT MIXER", false),
                B::More => self.more_button(ui, r, on(b)),
                _ => {}
            }
            ui.hit(r, self.id(Control::PluginWindow(b)), HitKind::Tap);
        }
    }

    /// `UAPW::Toolbar::ImageButtonWithLabel`: label at +6 (shadow +7,+2), dots right-aligned 4 pt
    /// from the edge, or centred without a label.
    fn more_button(&self, ui: &mut Ui, r: Rect, toggled: bool) {
        let icon = uapw(if toggled { "icon_more_yellow" } else { "icon_more" });
        let (iw, ih) = ui.image_size(&icon).unwrap_or((16.0, 6.0));
        match &self.chrome.items.more_label {
            Some(text) => {
                let style = TextStyle { family: "Helvetica".into(), size: 10.0, bold: true, italic: false, color: SHADOW };
                ui.text(text, style.clone(), Rect::new(r.x + 7.0, r.y + 1.0, r.w - 7.0, r.h), Align::Left);
                ui.text(text, TextStyle { color: LABEL, ..style }, Rect::new(r.x + 6.0, r.y, r.w - 6.0, r.h), Align::Left);
                ui.image(&icon, Rect::new(r.max_x() - iw - 4.0, r.y + (r.h - ih) / 2.0, iw, ih), None, None, 1.0);
            }
            None => ui.image_centered(&icon, r, 1.0),
        }
    }

    /// `Rack_PlugInFooter`: the manifest's `scrollbar-thumbnail` colour, a black outline, the
    /// state plate (background by state) and the optional action button.
    fn footer(&self, ui: &mut Ui, l: &WindowLayout, f: Rect) {
        let fill = match &self.content {
            Content::Web(c) => c.colors.get("scrollbar-thumbnail").copied(),
            _ => None,
        };
        ui.fill(f, fill.unwrap_or(Rgba::rgb8(85, 85, 85)));
        for r in [Rect::new(f.x, f.y, f.w, 1.0), Rect::new(f.x, f.max_y() - 1.0, f.w, 1.0), Rect::new(f.x, f.y, 1.0, f.h), Rect::new(f.max_x() - 1.0, f.y, 1.0, f.h)] {
            ui.fill(r, BLACK);
        }
        let label = TextStyle { family: "Helvetica".into(), size: 10.0, bold: true, italic: false, color: LABEL };
        if let (Some(r), Some(text)) = (l.button(B::FooterState), &self.chrome.status) {
            let bg = match self.chrome.status_state {
                2 => "btn_header_error_bg",
                3 => "btn_footer_blue",
                _ => "btn_header_bg",
            };
            nine(ui, &uapw(bg), r);
            ui.text(text, label.clone(), r, Align::Center);
            ui.hit(r, self.id(Control::PluginWindow(B::FooterState)), HitKind::Tap);
        }
        if let (Some(r), Some(text)) = (l.button(B::FooterAction), &self.chrome.action) {
            nine(ui, &uapw("btn_footer_blue"), r);
            ui.text(text, label, r, Align::Center);
            ui.hit(r, self.id(Control::PluginWindow(B::FooterAction)), HitKind::Tap);
        }
    }

    /// Popovers: the ⋮ insert menu (light console menu, s41), the "more" menu (the plug-in
    /// window's dark menu, s42, with the Help & Video panel s43); the preset browser is a stub.
    fn popover(&self, ui: &mut Ui, l: &WindowLayout, which: B) {
        use rack::menu::{MenuLook, PopupEntry, PopupItem, PopupMenu, layout_menu};
        let bounds = Rect::new(l.frame.x - 400.0, l.frame.y, l.frame.w + 800.0, 2000.0);
        match which {
            B::Assign => {
                let Some(menu) = &self.insert_menu else { return };
                let anchor = l.button(B::Assign).unwrap_or(l.header);
                let f = layout_menu(ui, menu, MenuLook::Context, (anchor.x, anchor.max_y()), 0.0, bounds);
                ui.popup_menu(menu, MenuLook::Context, &f, 0, None, None);
            }
            B::More => {
                // `Rack_PlugInHeader` 0x180dfb8d0: overflowed toolbar items, then Resize Plug-In
                // (scales), Apply To All, separator, Help & Video.
                let mut m = PopupMenu::new();
                for b in &l.overflow {
                    let name = match b {
                        B::Sidechain => "Sidechain",
                        B::Power => "Power",
                        B::InOut => "In",
                        B::CompareA => "Copy A to B",
                        B::Copy => "Copy",
                        B::Paste => "Paste",
                        _ => continue,
                    };
                    m.push(PopupItem::new(name, self.id(Control::PluginWindow(*b))));
                }
                m.separator();
                m.header("Resize Plug-In");
                for pct in [75u16, 100, 125, 150, 175, 200] {
                    m.push(PopupItem::new(format!("{pct}%"), self.id(Control::PluginWindow(B::Scale(pct)))).ticked(self.scale == pct));
                }
                m.push(PopupItem::new("Apply To All", self.id(Control::PluginWindow(B::ApplyToAll))));
                m.separator();
                m.submenu("Help & Video", PopupMenu::new(), true);
                let menu = m.finish();
                let anchor = l.button(B::More).unwrap_or(l.header);
                let probe = layout_menu(ui, &menu, MenuLook::Plugin, (0.0, 0.0), 0.0, bounds);
                let x = (anchor.max_x() - probe.frame.w).max(l.frame.x);
                let f = layout_menu(ui, &menu, MenuLook::Plugin, (x, anchor.max_y()), 0.0, bounds);
                let help = if self.help_open { menu.entries.len().checked_sub(1).map(|i| i as u16) } else { None };
                ui.popup_menu(&menu, MenuLook::Plugin, &f, 0, None, help);
                // Help & Video opens this window's panel (topmost over the row's submenu hit).
                if let (Some(PopupEntry::Submenu { .. }), Some(row)) = (menu.entries.last(), f.rows.last()) {
                    ui.hit(*row, self.id(Control::PluginWindow(B::Help)), HitKind::Tap);
                    if self.help_open {
                        self.help_panel(ui, Rect::new(f.frame.max_x() - 2.0, row.y - 2.0, 145.0, 122.0));
                    }
                }
            }
            B::Presets => {
                // The presets field's menu (`UAPW::PresetFileOperations`; not captured): the
                // plug-in's presets in the dark plug-in menu, folders as section headers, the
                // current one ticked; then Save Preset and Preset Browser... (the mixer's
                // PRESETS tab on this insert: SAVE AS, RENAME, DELETE, NEW FOLDER).
                let menu = self.preset_menu();
                let anchor = l.button(B::Presets).unwrap_or(l.header);
                let f = layout_menu(ui, &menu, MenuLook::Plugin, (anchor.x, anchor.max_y()), 0.0, bounds);
                ui.popup_menu(&menu, MenuLook::Plugin, &f, 0, None, None);
            }
            _ => {}
        }
    }

    /// The presets field's menu rows (see [`popover`](Self::popover)).
    pub fn preset_menu(&self) -> rack::menu::PopupMenu {
        use rack::menu::{PopupItem, PopupMenu};
        let mut m = PopupMenu::new();
        let current = self.chrome.preset.trim_start_matches('/');
        let mut index = 0u16;
        let mut item = |m: &mut PopupMenu, label: &str| {
            let value = self.presets.get(index as usize).cloned().unwrap_or_default();
            m.push(PopupItem::new(label.to_string(), self.id(Control::PluginWindow(B::PresetItem(index)))).ticked(value == current));
            index += 1;
        };
        if self.preset_tree.is_empty() {
            m.push(PopupItem::new("No Presets", self.id(Control::PluginWindow(B::Presets))).enabled(false));
        }
        for node in &self.preset_tree {
            match &node.children {
                None => item(&mut m, &node.name),
                Some(kids) => {
                    m.header(node.name.clone());
                    for k in kids.iter().filter(|k| k.children.is_none()) {
                        item(&mut m, &k.name);
                    }
                }
            }
        }
        m.separator();
        let user = !current.is_empty() && self.preset_locked(current) == Some(false);
        m.push(PopupItem::new("Save Preset", self.id(Control::PluginWindow(B::PresetSave))).enabled(user));
        m.push(PopupItem::new("Preset Browser...", self.id(Control::PluginWindow(B::PresetBrowser))));
        m.finish()
    }

    /// Whether `[folder/]name` is a locked (factory) preset; None when it is not in the tree.
    fn preset_locked(&self, value: &str) -> Option<bool> {
        let (folder, name) = value.rsplit_once('/').map(|(f, n)| (Some(f), n)).unwrap_or((None, value));
        let list = match folder {
            Some(f) => self.preset_tree.iter().find(|n| n.name == f)?.children.as_ref()?,
            None => &self.preset_tree,
        };
        list.iter().find(|n| n.name == name && n.children.is_none()).map(|n| n.locked)
    }

    /// The Help & Video panel (s43): Documentation (a link) and the version, a rule, and the
    /// Quick Tip Video tile (the thumbnail is the plug-in's own art; a dark well stands in).
    fn help_panel(&self, ui: &mut Ui, r: Rect) {
        ui.fill(r.inset(-0.5, -0.5, -0.5, -0.5), Rgba::rgb8(2, 3, 4));
        ui.fill(r, Rgba::rgb8(70, 80, 86));
        ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(22, 27, 30));
        let link = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(214, 226, 232));
        let docs = Rect::new(r.x + 12.0, r.y + 8.0, 76.0, 18.0);
        ui.text("Documentation", link.clone(), docs, Align::Left);
        ui.fill(Rect::new(docs.x, docs.max_y() - 2.0, 74.0, 1.0), link.color);
        let v = self.version.clone().map(|v| format!("v{v}")).unwrap_or_else(|| "v1.0.0".into());
        ui.text(&v, Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE), Rect::new(docs.max_x() + 4.0, docs.y, 50.0, 18.0), Align::Left);
        ui.fill(Rect::new(r.x + 6.0, r.y + 33.0, r.w - 12.0, 1.0), Rgba::rgb8(40, 46, 50));
        let thumb = Rect::new(r.x + 13.0, r.y + 43.0, r.w - 26.0, 45.0);
        ui.fill(thumb, Rgba::rgb8(60, 64, 66));
        ui.text("Quick Tip Video", Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE), Rect::new(r.x, thumb.max_y() + 4.0, r.w, 20.0), Align::Center);
        ui.hit(docs, self.id(Control::PluginWindow(B::Help)), HitKind::Tap);
    }

    /// Applies an action aimed at this window (chrome or content); None when it is not for it.
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
                B::Presets | B::Assign | B::More => {
                    self.popover = if self.popover == Some(b) { None } else { Some(b) };
                    self.help_open = false;
                    WindowEvent::Handled
                }
                B::Help if self.popover == Some(B::More) && !self.help_open => {
                    self.help_open = true;
                    WindowEvent::Handled
                }
                B::Scale(pct) => {
                    self.scale = pct;
                    self.popover = None;
                    WindowEvent::Button(b)
                }
                B::RackView => {
                    self.chrome.rack_view ^= true;
                    WindowEvent::Handled
                }
                B::CompareA | B::CompareB => {
                    self.chrome.compare_b = b == B::CompareB;
                    WindowEvent::Button(b)
                }
                other => {
                    self.popover = None;
                    WindowEvent::Button(other)
                }
            });
        }
        let handled = match &mut self.content {
            Content::Spec(p) => p.apply(action, params),
            Content::Generic(g) => g.apply(action, params),
            Content::Web(_) => false,
        };
        handled.then_some(WindowEvent::Handled)
    }

    // MARK: spec panel gestures

    /// A press at window point (x, y) for a spec panel's own gestures (long presses, repeats,
    /// held looks, swipes, graph handles, its pop-ups): taken when it lands on the panel and the
    /// topmost of `hits` (this frame's hits) there is the panel's, not chrome or a popover.
    /// Follow with [`panel_pointer_move`](Self::panel_pointer_move) /
    /// [`panel_pointer_up`](Self::panel_pointer_up); untaken presses go through `rack`.
    pub fn panel_pointer_down(&mut self, hits: &[Hit], x: f32, y: f32, double: bool, params: &mut dyn ParamSource) -> bool {
        let slot = self.slot.clone();
        let Content::Spec(p) = &mut self.content else { return false };
        if let Some(h) = hits.iter().rev().find(|h| h.rect.contains(x, y)) {
            let ours = h.id.strip == slot && matches!(h.id.control, Control::PluginParam(_) | Control::PluginElement(_) | Control::PluginStep(..));
            if !ours {
                return false;
            }
        }
        if !p.contains(x, y) {
            return false;
        }
        p.pointer_down(x, y, double, params)
    }

    pub fn panel_pointer_move(&mut self, x: f32, y: f32, params: &mut dyn ParamSource) -> bool {
        match &mut self.content {
            Content::Spec(p) => p.pointer_move(x, y, params),
            _ => false,
        }
    }

    pub fn panel_pointer_up(&mut self, x: f32, y: f32, params: &mut dyn ParamSource) -> bool {
        match &mut self.content {
            Content::Spec(p) => p.pointer_up(x, y, params),
            _ => false,
        }
    }

    /// Advances a spec panel's clock (meters, touch-state timeouts, long presses, repeats).
    /// True when something changed.
    pub fn tick(&mut self, params: &mut dyn ParamSource) -> bool {
        match &mut self.content {
            Content::Spec(p) => p.tick(params),
            _ => false,
        }
    }

    /// True while the spec panel animates on its own (the shell keeps redrawing).
    pub fn animating(&self, params: &dyn ParamSource) -> bool {
        match &self.content {
            Content::Spec(p) => p.animating(params),
            _ => false,
        }
    }

    /// The read-only meter queries of a spec panel (empty for other content).
    pub fn meter_queries(&self) -> Vec<String> {
        match &self.content {
            Content::Spec(p) => p.meter_queries(),
            _ => Vec::new(),
        }
    }

    /// A reply to one of [`meter_queries`](Self::meter_queries).
    pub fn ingest_meter_reply(&mut self, query: &str, reply: &serde_json::Value, params: &dyn ParamSource) -> bool {
        match &mut self.content {
            Content::Spec(p) => p.ingest_reply(query, reply, params),
            _ => false,
        }
    }

    /// The insert's channel layout: a spec declaring state `channels` gets 1 (mono) or 2
    /// (stereo), and a mono insert draws at `monoSize` when the spec has one.
    pub fn set_channel_layout(&mut self, stereo: bool) {
        if let Content::Spec(p) = &mut self.content {
            p.set_channel_layout(stereo);
        }
    }
}

/// UAPW 9-slice: corner = floor(min(w, h) / 3) image px, drawn at half size (WINDOW.md §3.2).
fn nine(ui: &mut Ui, key: &str, r: Rect) {
    match ui.assets.images.get(key) {
        Some(img) => {
            let c = (img.width.min(img.height) / 3) as f32;
            ui.image(key, r, Some(Insets::uniform(c)), None, 1.0);
        }
        None => ui.fill(r, Rgba::BLACK.with_alpha(0.3)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 1.3.1 capture: SSL G (300×621) → cw 302, mode 2, "UAD-2" more (WINDOW.md §3.3).
    #[test]
    fn header_matches_capture() {
        let (items, mode, overflow) = header_layout(302.0, &HeaderItems::default());
        assert_eq!(mode, 2);
        let x = |b: B| items.iter().find(|(x, _)| *x == b).map(|(_, r)| (r.x, r.max_x()));
        assert_eq!(x(B::Assign), Some((5.0, 27.0)));
        assert_eq!(x(B::PresetPrev), Some((32.0, 54.0)));
        assert_eq!(x(B::PresetNext), Some((54.0, 75.0)));
        assert_eq!(x(B::Presets), Some((75.0, 185.0)));
        assert_eq!(x(B::Power), Some((190.0, 212.0)));
        assert_eq!(x(B::More), Some((239.0, 297.0)));
        assert_eq!(overflow, vec![B::Copy, B::Paste]);
        assert!(items.iter().all(|(_, r)| r.y == 5.0 && r.h == 22.0));
    }

    #[test]
    fn preset_menu_lists_ticks_and_gates_save() {
        use rack::PresetNode;
        use rack::menu::PopupEntry;
        let mut w = PluginWindow::open("/devices/0/inputs/0/effects/0", "UAD Pultec EQP-1A", "SM7B, Insert 1", None, None, false);
        w.preset_tree = vec![PresetNode::file("Bright").locked(true), PresetNode::folder("Mine", vec![PresetNode::file("Air")])];
        w.presets = rack::presets::flatten(&w.preset_tree);
        let rows = |w: &PluginWindow| {
            w.preset_menu()
                .entries
                .iter()
                .filter_map(|e| match e {
                    PopupEntry::Item(i) => Some((i.label.clone(), i.ticked, i.enabled)),
                    PopupEntry::Header(h) => Some((format!("[{h}]"), false, true)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        w.chrome.preset = "Bright".into();
        let r = rows(&w);
        assert_eq!(r.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["Bright", "[Mine]", "Air", "Save Preset", "Preset Browser..."]);
        assert!(r[0].1 && !r[2].1, "the current preset is ticked");
        assert!(!r[3].2, "a factory preset cannot be saved over");
        w.chrome.preset = "Mine/Air".into();
        let r = rows(&w);
        assert!(r[2].1 && r[3].2, "a user preset is ticked and Save Preset is enabled");
    }

    #[test]
    fn wide_header_shows_everything_in_mode_0() {
        let items = HeaderItems { more_label: None, ..Default::default() };
        let (placed, mode, overflow) = header_layout(952.0, &items);
        assert_eq!(mode, 0);
        assert!(overflow.is_empty());
        assert!(!placed.iter().any(|(b, _)| *b == B::More), "nothing overflowed and more is not forced");
        assert_eq!(placed.iter().find(|(b, _)| *b == B::Presets).unwrap().1.w, 220.0);
        assert!(placed.iter().any(|(b, _)| *b == B::Paste));
    }
}
