//! Console's separate OS windows: popup menus, the Settings window, panels and dialogs.
//!
//! UAD Console is a JUCE app: every `PopupMenu` level, the Settings window, the Monitor and Cues
//! windows, the Sends and TEMPO callouts and the dialogs are top-level windows of their own. A
//! menu opened near the bottom of the mixer hangs past the mixer window (drive captures lbl-*,
//! sb-rate, monitor-meter); JUCE only keeps it on the display.
//!
//! `build_window_with` draws them all into one [`crate::DrawList`] (so single-surface hosts, the
//! iPad and plain `--render` keep working) and records each as a [`Layer`]: its id, kind and
//! window rect in mixer-window points, plus [`LayerMark`]s that say which draws, hits and scroll
//! regions belong to it. A shell that can open windows splits the frame with [`split`] and
//! shows each layer in a borderless window at `main window origin + rect`.
//!
//! With [`ViewState::windowing`](crate::ViewState::windowing) set, placement follows Console:
//! menus are clamped to [`Windowing::screen`] instead of the mixer window, the Settings window
//! and panels open where they were last left ([`Windowing::positions`]), dialogs centre on the
//! screen (set-adddev), and nothing dims the mixer behind them.

use std::collections::HashMap;

use houston::Rect;

use crate::draw::{Draw, DrawList};
use crate::input::{Hit, ScrollRegion};

/// Separate-window placement (desktop shells with real windows; `--render --screen`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Windowing {
    /// The display's usable area in mixer-window points (macOS: the visible frame, under the
    /// menu bar). Menus flip and shift to stay inside it, as JUCE keeps them on the display.
    pub screen: Rect,
    /// Where each movable window was left (its top-left in mixer-window points), by layer id.
    pub positions: HashMap<String, (f32, f32)>,
}

impl Windowing {
    pub fn new(screen: Rect) -> Self {
        Self { screen, positions: HashMap::new() }
    }
}

/// What a layer is, for the window a shell opens for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayerKind {
    /// A popup menu level: borderless, no focus, dismissed by a click elsewhere or focus loss.
    Menu,
    /// A free-floating window with Console's own title bar (Settings, Monitor, I/O Matrix
    /// source, Route Presets, plug-in windows): moved by its title bar, remembered.
    Window,
    /// A modal dialog centred on the screen (ADD DEVICE).
    Dialog,
    /// A callout anchored to the control that opened it (TEMPO, Sends, Cues Assignments);
    /// placed by the caller.
    Popover,
}

/// One separate window of the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// Stable across frames while the window stays open (`menu:0`, `menu:1`, `settings`,
    /// `panel:cue-outputs`, …): shells keep one OS window per id.
    pub id: String,
    pub kind: LayerKind,
    /// The window rect in mixer-window points (may lie partly or wholly outside the mixer).
    pub rect: Rect,
    /// Height of Console's drawn title bar (drag to move); 0 for menus and callouts.
    pub title_h: f32,
}

/// From draw `draw`, hit `hit` and scroll region `scroll` on, items belong to `layer`
/// (index into the frame's layers + 1; 0 = the mixer window itself).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerMark {
    pub draw: usize,
    pub hit: usize,
    pub scroll: usize,
    pub layer: u16,
}

/// One window's share of a frame: its draws, hits and scroll regions, still in mixer-window
/// points (render with an offset of −`rect` origin; translate pointer input by +origin).
#[derive(Clone, Debug, Default)]
pub struct LayerPart {
    pub list: DrawList,
    pub hits: Vec<Hit>,
    pub scrolls: Vec<ScrollRegion>,
}

/// Which layer (0 = mixer window) item `i` of a sequence belongs to, given its mark positions.
fn owner(marks: &[LayerMark], i: usize, at: impl Fn(&LayerMark) -> usize) -> u16 {
    marks.iter().rev().find(|m| at(m) <= i).map(|m| m.layer).unwrap_or(0)
}

/// Layer index (0 = mixer window) of hit `i`.
pub fn hit_layer(marks: &[LayerMark], i: usize) -> u16 {
    owner(marks, i, |m| m.hit)
}

/// Splits a frame into the mixer window (part 0) and one part per layer (part `k` = layer
/// `k − 1`). Each part starts unclipped.
pub fn split(list: &DrawList, hits: &[Hit], scrolls: &[ScrollRegion], layers: usize, marks: &[LayerMark]) -> Vec<LayerPart> {
    let mut parts = vec![LayerPart::default(); layers + 1];
    for (i, d) in list.items.iter().enumerate() {
        let k = owner(marks, i, |m| m.draw) as usize;
        if let Some(p) = parts.get_mut(k) {
            p.list.items.push(d.clone());
        }
    }
    for (i, h) in hits.iter().enumerate() {
        let k = owner(marks, i, |m| m.hit) as usize;
        if let Some(p) = parts.get_mut(k) {
            p.hits.push(h.clone());
        }
    }
    for (i, s) in scrolls.iter().enumerate() {
        let k = owner(marks, i, |m| m.scroll) as usize;
        if let Some(p) = parts.get_mut(k) {
            p.scrolls.push(s.clone());
        }
    }
    parts
}

/// The bounding box of the mixer window (`0, 0, size`) and every layer: the canvas a composite
/// render (`uaconsole --render --screen`) needs to show menus hanging outside the mixer.
pub fn canvas(size: (f32, f32), layers: &[Layer]) -> Rect {
    let mut r = Rect::new(0.0, 0.0, size.0, size.1);
    for l in layers {
        let x0 = r.x.min(l.rect.x);
        let y0 = r.y.min(l.rect.y);
        let x1 = r.max_x().max(l.rect.max_x());
        let y1 = r.max_y().max(l.rect.max_y());
        r = Rect::new(x0, y0, x1 - x0, y1 - y0);
    }
    r
}

impl DrawList {
    /// A copy with every rect moved by (`dx`, `dy`) points.
    pub fn translated(&self, dx: f32, dy: f32) -> DrawList {
        let items = self
            .items
            .iter()
            .map(|d| match d {
                Draw::Image { key, frame, dst, slice, tint } => Draw::Image { key: key.clone(), frame: *frame, dst: dst.offset(dx, dy), slice: *slice, tint: *tint },
                Draw::Fill { rect, color } => Draw::Fill { rect: rect.offset(dx, dy), color: *color },
                Draw::Text { text, style, rect, align } => Draw::Text { text: text.clone(), style: style.clone(), rect: rect.offset(dx, dy), align: *align },
                Draw::Clip(c) => Draw::Clip(c.map(|r| r.offset(dx, dy))),
            })
            .collect();
        DrawList { items }
    }
}

/// The layer id of a panel's window (stable while it is open; keys remembered positions).
pub fn panel_id(p: crate::settings::Panel) -> String {
    use crate::settings::{Panel as P, SendsOf};
    match p {
        P::CueOutputs => "panel:cue-outputs".into(),
        P::MonitorController => "panel:monitor-controller".into(),
        P::HeadphoneCorrection => "panel:headphone-correction".into(),
        P::PluginScenes => "panel:plugin-scenes".into(),
        P::About => "panel:about".into(),
        P::MeterWindow => "panel:meters".into(),
        P::Sends(SendsOf::Talkback) => "panel:sends:talkback".into(),
        P::Sends(SendsOf::Aux(k)) => format!("panel:sends:aux{k}"),
        P::Sends(SendsOf::Input(k)) => format!("panel:sends:in{k}"),
        P::AddDevice => "panel:add-device".into(),
        P::IoPresets => "panel:io-presets".into(),
        P::RouteSource { route, outputs } => format!("panel:route-source:{}{route}", if outputs { "out" } else { "in" }),
        P::Tempo => "panel:tempo".into(),
    }
}

/// How Console opens a panel's window (drive captures): callouts at their control (panel-cue,
/// send-cell, sends-aux, sb-tempo, set-d-iopresets, set-d-insrc), ADD DEVICE and About centred
/// on the screen (set-adddev), the rest as free windows that keep where they were moved
/// (panel-monctl, s44).
pub fn panel_kind(p: crate::settings::Panel) -> LayerKind {
    use crate::settings::Panel as P;
    match p {
        P::CueOutputs | P::Sends(_) | P::Tempo | P::IoPresets | P::RouteSource { .. } => LayerKind::Popover,
        P::AddDevice | P::About => LayerKind::Dialog,
        P::MonitorController | P::HeadphoneCorrection | P::PluginScenes | P::MeterWindow => LayerKind::Window,
    }
}

/// Keeps `r` on `screen` (JUCE `setBoundsConstrained`): shifted in, never resized.
pub fn constrain(r: Rect, screen: Rect) -> Rect {
    let x = if r.max_x() > screen.max_x() { screen.max_x() - r.w } else { r.x };
    let y = if r.max_y() > screen.max_y() { screen.max_y() - r.h } else { r.y };
    Rect::new(x.max(screen.x), y.max(screen.y), r.w, r.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Control, ControlId, HitKind};
    use houston::Rgba;

    fn hit(n: u8) -> Hit {
        Hit { rect: Rect::new(0.0, 0.0, 1.0, 1.0), id: ControlId { strip: n.to_string().into(), control: Control::PopoverDismiss }, kind: HitKind::Tap }
    }

    #[test]
    fn split_by_marks() {
        let fill = |x: f32| Draw::Fill { rect: Rect::new(x, 0.0, 1.0, 1.0), color: Rgba::BLACK };
        let list = DrawList { items: vec![fill(0.0), fill(1.0), fill(2.0), fill(3.0)] };
        let hits = vec![hit(0), hit(1), hit(2)];
        // draws 0 | layer 1: 1, 2 | back to the mixer: 3; hits 0 | 1, 2 in layer 1.
        let marks = [LayerMark { draw: 1, hit: 1, scroll: 0, layer: 1 }, LayerMark { draw: 3, hit: 3, scroll: 0, layer: 0 }];
        let parts = split(&list, &hits, &[], 1, &marks);
        assert_eq!(parts[0].list.len(), 2);
        assert_eq!(parts[1].list.len(), 2);
        assert_eq!(parts[0].hits.len(), 1);
        assert_eq!(parts[1].hits.len(), 2);
        assert_eq!(hit_layer(&marks, 0), 0);
        assert_eq!(hit_layer(&marks, 2), 1);
    }

    #[test]
    fn canvas_and_constrain() {
        let l = Layer { id: "menu:0".into(), kind: LayerKind::Menu, rect: Rect::new(488.0, 1065.0, 153.0, 136.0), title_h: 0.0 };
        assert_eq!(canvas((1470.0, 1105.0), &[l]), Rect::new(0.0, 0.0, 1470.0, 1201.0));
        let screen = Rect::new(-853.0, -300.0, 3200.0, 1770.0);
        assert_eq!(constrain(Rect::new(2300.0, 1400.0, 100.0, 100.0), screen), Rect::new(2247.0, 1370.0, 100.0, 100.0));
        assert_eq!(constrain(Rect::new(-900.0, -400.0, 10.0, 10.0), screen), Rect::new(-853.0, -300.0, 10.0, 10.0));
    }
}
