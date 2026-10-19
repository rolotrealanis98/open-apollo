//! Drag and drop, as UAD Console 1.3.1 does it (`Rack_BrowserItemDragController::HandleRowDrag`
//! 0x1813b1eb0, `Rack_PluginDropTargetHelper::HandleItemDroppedAfter` 0x1812daa60,
//! `Rack_PresetDropTargetHelper` 0x181309570 / 0x181309c80, `Rack_Inserts` drops):
//!
//! * a browser plug-in row onto an insert or Unison slot assigns it (`EffectName`, as a
//!   double click or assign mode does);
//! * a loaded insert onto another slot moves it, or copies it with the copy modifier
//!   (`post /MoveInsert {"copy", "srcPath", "dstPath"}`, the engine's `CMxMoveInsertAction`);
//! * a channel preset onto a strip loads it (`Control::PresetDrop`, the presets layer's).
//!
//! A press on a drag source arms a drag; the shell defers the press's tap to the release and
//! starts the drag once the pointer has moved [`DRAG_START`] points (JUCE
//! `DragAndDropContainer`). The drag draws a ghost of the dragged item under the pointer and
//! the drop target's `image-targeted` highlight (the assign-mode skin of `inserts:Small-Normal`).
//!
//! A slot the model already knows the plug-in cannot load on (`MixerModel::insert_refusal`:
//! native hosting's mono / stereo slot lists) is forbidden: no highlight, a dimmed ghost. A
//! drop there, or an engine refusal of a drop, shows the reason under the slot for a while
//! ([`DropNotice`]) as well as in the browser.
//!
//! ```ignore
//! // press:   if view.drag_press(&hits, x, y, model) { defer the tap }
//! // move:    if view.drag_motion(&hits, x, y, copy_modifier, model) { redraw }
//! // release: match view.drag_release(model) { Some(fx) => apply fx, None => fire the tap }
//! ```
//!
//! Strip reorder (`MoveChannel`) is not a Console 1.x gesture: Rack.dll never sends it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Action, Control, ControlId, Hit};
use crate::model::{InsertState, MixerModel, StripInfo};
use crate::surfaces::{BrowserTab, Effect, ViewState};
use crate::ui::Ui;

/// Movement (points) before a press on a drag source becomes a drag.
pub const DRAG_START: f32 = 4.0;
/// The Unison slot's `Insert` index.
pub const UNISON: u8 = 255;
/// Drag image opacity (JUCE `DragAndDropContainer`'s default snapshot alpha).
pub const GHOST_ALPHA: f32 = 0.6;
/// Over a forbidden slot.
pub const FORBIDDEN_ALPHA: f32 = 0.25;
/// How long a drop's refusal stays under the slot.
pub const NOTICE_FOR: Duration = Duration::from_secs(6);

/// What is being dragged.
#[derive(Clone, Debug, PartialEq)]
pub enum DragPayload {
    /// A plug-in browser row: the catalogue name written to `EffectName`.
    Plugin { name: String },
    /// A loaded insert (slot 255: Unison) of `strip`, with its catalogue name.
    Insert { strip: Arc<str>, slot: u8, name: String },
    /// A channel preset of the browser's PRESETS tab, by its preset path.
    Preset { path: String },
}

/// An armed press: becomes a [`DragState`] after [`DRAG_START`] points of motion.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingDrag {
    pub payload: DragPayload,
    pub start: (f32, f32),
    /// The source's rect (the ghost keeps the grab offset into it).
    pub origin: Rect,
}

/// A drag in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct DragState {
    pub payload: DragPayload,
    /// Pointer, Console points.
    pub x: f32,
    pub y: f32,
    /// Pointer offset into the ghost.
    pub grab: (f32, f32),
    /// Ghost size.
    pub size: (f32, f32),
    /// The copy modifier is down (insert drags copy instead of move).
    pub copy: bool,
    /// The drop target under the pointer (its hit), if it accepts the payload.
    pub target: Option<Hit>,
    /// The slot under the pointer that refuses the payload, and why.
    pub forbidden: Option<(Hit, String)>,
}

/// The slot of the last plug-in drop: while it lasts, the model's plug-in notice (a refusal,
/// now or from the engine) is drawn under it.
#[derive(Clone, Debug, PartialEq)]
pub struct DropNotice {
    pub rect: Rect,
    pub until: Instant,
}

fn strips(model: &dyn MixerModel) -> Vec<StripInfo> {
    model.inputs().into_iter().chain(model.auxes()).collect()
}

fn strip_of(model: &dyn MixerModel, id: &str) -> Option<StripInfo> {
    strips(model).into_iter().find(|s| &*s.id == id)
}

/// A slot's state (`255` is the Unison slot).
pub fn slot_state(model: &dyn MixerModel, strip: &str, slot: u8) -> InsertState {
    let Some(s) = strip_of(model, strip) else { return InsertState::Empty };
    if slot == UNISON { model.unison(&s) } else { model.inserts(&s).get(slot as usize).cloned().unwrap_or(InsertState::Empty) }
}

impl ViewState {
    /// The drag source under (x, y), with its rect: a loaded insert or Unison slot, or a
    /// plug-in row of the PLUG-INS tab. None in assign mode (a row tap assigns there).
    pub fn drag_source(&self, hits: &[Hit], x: f32, y: f32, model: &dyn MixerModel) -> Option<(DragPayload, Rect)> {
        if self.menu.is_some() || self.popover.is_some() {
            return None;
        }
        let hit = hits.iter().rev().find(|h| h.rect.contains(x, y))?;
        match hit.id.control {
            Control::Insert(slot) => match slot_state(model, &hit.id.strip, slot) {
                InsertState::Loaded { name, .. } => Some((DragPayload::Insert { strip: hit.id.strip.clone(), slot, name }, hit.rect)),
                InsertState::Empty => None,
            },
            Control::BrowserItem(i) if self.browser.tab == BrowserTab::Plugins && self.browser.assign_to.is_none() => {
                let p = model.plugin_catalog().into_iter().nth(i as usize)?;
                (p.enabled && p.native_supported != Some(false)).then(|| (DragPayload::Plugin { name: p.name }, hit.rect))
            }
            // A PRESETS file row with no assign target (`Rack_PresetDropTargetHelper`): drags
            // its "[folder/]name" onto a strip.
            Control::BrowserPresetFile(top, child) if self.browser.preset_target.is_none() => {
                let path = crate::presets::PresetRow::decode(top, child).value(&self.preset_tree(model))?;
                Some((DragPayload::Preset { path }, hit.rect))
            }
            _ => None,
        }
    }

    /// Press: arms a drag when (x, y) is on a drag source. True means the shell must hold the
    /// press's tap until [`ViewState::drag_release`] says no drag happened.
    pub fn drag_press(&mut self, hits: &[Hit], x: f32, y: f32, model: &dyn MixerModel) -> bool {
        self.drag = None;
        self.drop_notice = None;
        self.drag_pending = self.drag_source(hits, x, y, model).map(|(payload, origin)| PendingDrag { payload, start: (x, y), origin });
        self.drag_pending.is_some()
    }

    /// Starts a drag of `payload` grabbed at (x, y) inside `origin` (for sources outside
    /// [`ViewState::drag_source`], e.g. the PRESETS tab rows).
    pub fn begin_drag(&mut self, payload: DragPayload, x: f32, y: f32, origin: Rect) {
        self.drag_pending = Some(PendingDrag { payload, start: (x, y), origin });
    }

    /// Pointer motion. Promotes an armed press to a drag after [`DRAG_START`] points and keeps
    /// the target current. True while a drag is in progress (redraw).
    pub fn drag_motion(&mut self, hits: &[Hit], x: f32, y: f32, copy: bool, model: &dyn MixerModel) -> bool {
        if self.drag.is_none() {
            let Some(p) = &self.drag_pending else { return false };
            if (x - p.start.0).hypot(y - p.start.1) < DRAG_START {
                return false;
            }
            let p = self.drag_pending.take().expect("checked");
            let (gw, gh) = ghost_size(&p.payload, p.origin);
            let grab = ((p.start.0 - p.origin.x).clamp(0.0, gw), (p.start.1 - p.origin.y).clamp(0.0, gh));
            self.drag =
                Some(DragState { payload: p.payload, x, y, grab, size: (gw, gh), copy, target: None, forbidden: None });
        }
        let d = self.drag.as_mut().expect("dragging");
        let target = drop_target(hits, x, y, &d.payload, model);
        d.forbidden = match (&d.payload, &target) {
            (DragPayload::Plugin { name }, Some(t)) => model.insert_refusal(&t.id, name).map(|why| (t.clone(), why)),
            _ => None,
        };
        d.target = target.filter(|_| d.forbidden.is_none());
        d.x = x;
        d.y = y;
        d.copy = copy;
        true
    }

    /// Release: `Some(effects)` ends a drag (empty when there was no valid target); `None`
    /// means no drag happened, so the shell fires the press's tap.
    pub fn drag_release(&mut self, model: &dyn MixerModel) -> Option<Vec<Effect>> {
        self.drag_pending = None;
        let d = self.drag.take()?;
        // A forbidden slot still gets the assignment: the model refuses it without writing,
        // and keeps the reason for the browser and the slot.
        let Some(target) = d.target.or_else(|| d.forbidden.map(|(hit, _)| hit)) else { return Some(Vec::new()) };
        if matches!(d.payload, DragPayload::Plugin { .. }) {
            self.drop_notice = Some(DropNotice { rect: target.rect, until: Instant::now() + NOTICE_FOR });
        }
        Some(drop_effects(&d.payload, &target.id, d.copy, model))
    }

    /// Escape / pointer cancel / focus loss.
    pub fn drag_cancel(&mut self) {
        self.drag = None;
        self.drag_pending = None;
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

/// Ghost size: an insert slot for plug-ins and inserts, a row for presets.
fn ghost_size(payload: &DragPayload, origin: Rect) -> (f32, f32) {
    match payload {
        DragPayload::Insert { .. } => (origin.w, origin.h),
        DragPayload::Plugin { .. } => (80.0, 48.0),
        DragPayload::Preset { .. } => (origin.w.min(220.0), origin.h.max(20.0)),
    }
}

/// The hit under (x, y) that accepts `payload`.
fn drop_target(hits: &[Hit], x: f32, y: f32, payload: &DragPayload, model: &dyn MixerModel) -> Option<Hit> {
    let hit = hits.iter().rev().find(|h| h.rect.contains(x, y))?;
    let on_strip = strips(model).into_iter().any(|s| s.id == hit.id.strip);
    if !on_strip {
        return None;
    }
    match payload {
        DragPayload::Plugin { name } => match hit.id.control {
            Control::Insert(UNISON) if model.plugin_is_unison(name) == Some(false) => None,
            Control::Insert(_) => Some(hit.clone()),
            _ => None,
        },
        DragPayload::Insert { strip, slot, name } => match hit.id.control {
            Control::Insert(k) if hit.id.strip == *strip && k == *slot => None,
            Control::Insert(UNISON) if model.plugin_is_unison(name) == Some(false) => None,
            Control::Insert(_) => Some(hit.clone()),
            _ => None,
        },
        // A preset lands on the strip: any of its controls, highlighted by its label.
        DragPayload::Preset { .. } => {
            let strip = hit.id.strip.clone();
            hits.iter().rev().find(|h| h.id.strip == strip && h.id.control == Control::Name).cloned().or_else(|| Some(hit.clone()))
        }
    }
}

/// What a drop of `payload` on `target` does.
pub fn drop_effects(payload: &DragPayload, target: &ControlId, copy: bool, _model: &dyn MixerModel) -> Vec<Effect> {
    match (payload, &target.control) {
        // Same as a double click / assign mode: `EffectName` ← catalogue name.
        (DragPayload::Plugin { name }, Control::Insert(_)) => vec![Effect::SetText(target.clone(), name.clone())],
        (DragPayload::Insert { strip, slot, .. }, Control::Insert(k)) => vec![Effect::Model(Action::Tap(ControlId {
            strip: target.strip.clone(),
            control: Control::InsertMove { slot: *k, from: strip.clone(), from_slot: *slot, copy },
        }))],
        (DragPayload::Preset { path }, _) => vec![Effect::Model(Action::Tap(ControlId { strip: target.strip.clone(), control: Control::PresetDrop(path.as_str().into()) }))],
        _ => Vec::new(),
    }
}

/// Draws the drop target highlight and the ghost, or the last drop's refusal under its slot.
/// Call last (over menus and popovers).
pub fn draw_drag(ui: &mut Ui, view: &ViewState, model: &dyn MixerModel) {
    if let (None, Some(n), Some(text)) = (&view.drag, &view.drop_notice, model.plugin_notice()) {
        if Instant::now() < n.until {
            draw_notice(ui, n.rect, text);
        }
    }
    let Some(d) = &view.drag else { return };
    let alpha = if d.forbidden.is_some() { FORBIDDEN_ALPHA } else { GHOST_ALPHA };
    if let Some(t) = &d.target {
        match t.id.control {
            Control::Insert(_) => {
                if let Some(img) = ui.skin("inserts", "Small-Normal").and_then(|k| k.image("image-targeted")) {
                    let img = img.to_string();
                    ui.image_stretched(&img, t.rect, 1.0);
                }
            }
            _ => {
                let c = Rgba::rgb8(255, 196, 0);
                let r = t.rect;
                ui.fill(Rect::new(r.x, r.y, r.w, 1.0), c);
                ui.fill(Rect::new(r.x, r.max_y() - 1.0, r.w, 1.0), c);
                ui.fill(Rect::new(r.x, r.y, 1.0, r.h), c);
                ui.fill(Rect::new(r.max_x() - 1.0, r.y, 1.0, r.h), c);
            }
        }
    }
    let g = Rect::new(d.x - d.grab.0, d.y - d.grab.1, d.size.0, d.size.1);
    match &d.payload {
        DragPayload::Insert { name, .. } | DragPayload::Plugin { name } => {
            if let Some(img) = ui.skin("inserts", "Small-Normal").and_then(|k| k.image("image-active")) {
                let img = img.to_string();
                ui.image_stretched(&img, g, alpha);
            }
            let key = format!("plugin:{name}");
            if let Some((w, h)) = ui.image_size(&key) {
                let b = Rect::new(g.x + (g.w - 64.0) / 2.0, g.y + 4.0, 64.0, 25.0);
                let s = (b.w / w).min(b.h / h);
                ui.image(&key, b.centered(w * s, h * s), None, None, alpha);
            }
            let mut style = ui.style("inserts", "Small-Normal");
            style.color.a *= alpha;
            let label = name.strip_prefix("UAD ").unwrap_or(name);
            ui.text(label, style, Rect::new(g.x + 4.0, g.y + g.h - 18.0, g.w - 8.0, 14.0), Align::Center);
        }
        DragPayload::Preset { path } => {
            ui.fill(g, Rgba::rgb8(40, 42, 44).with_alpha(GHOST_ALPHA));
            let style = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(235, 236, 236).with_alpha(GHOST_ALPHA));
            let label = path.rsplit('/').next().unwrap_or(path);
            ui.text(label, style, Rect::new(g.x + 8.0, g.y, g.w - 12.0, g.h), Align::Left);
        }
    }
}

/// A refusal under slot `slot`: amber text (the browser notice's) on a dark box, wrapped.
/// The newest refused write ([`MixerModel::last_refusal`]) for [`NOTICE_FOR`], centred over the
/// status bar at `status_y`, so a refusal that answers a menu (a link) is not silent.
pub fn draw_refusal(ui: &mut Ui, model: &dyn MixerModel, width: f32, status_y: f32) {
    let Some((text, at)) = model.last_refusal() else { return };
    if at.elapsed() < NOTICE_FOR {
        let lines = crate::surfaces::browser::wrap(&text, (240.0 / 5.5) as usize).len().min(4) as f32;
        draw_notice(ui, Rect::new(width / 2.0 - 120.0, status_y - lines * 14.0 - 14.0, 240.0, 0.0), &text);
    }
}

fn draw_notice(ui: &mut Ui, slot: Rect, text: &str) {
    const W: f32 = 240.0;
    const LINE: f32 = 14.0;
    let mut lines = crate::surfaces::browser::wrap(text, (W / 5.5) as usize);
    lines.truncate(4);
    let r = Rect::new(slot.x, slot.max_y() + 2.0, W, lines.len() as f32 * LINE + 8.0);
    ui.fill(r, Rgba::rgb8(24, 25, 27).with_alpha(0.95));
    let style = Ui::plain_style("Helvetica", 10.0, true, crate::surfaces::browser::AMBER);
    for (i, line) in lines.iter().enumerate() {
        ui.text(line, style.clone(), Rect::new(r.x + 6.0, r.y + 4.0 + i as f32 * LINE, W - 12.0, LINE), Align::Left);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::HitKind;
    use crate::model::{MainState, MeterFrame, MonitorState, PreampState, PreviewMixer, SendState};

    /// Preview data whose insert 1 refuses every plug-in (a stereo-only bundle on a mono input).
    struct Refusing(PreviewMixer);

    impl MixerModel for Refusing {
        fn session_name(&self) -> String {
            self.0.session_name()
        }
        fn inputs(&self) -> Vec<StripInfo> {
            self.0.inputs()
        }
        fn auxes(&self) -> Vec<StripInfo> {
            self.0.auxes()
        }
        fn main(&self, s: &StripInfo) -> MainState {
            self.0.main(s)
        }
        fn preamp(&self, s: &StripInfo) -> Option<PreampState> {
            self.0.preamp(s)
        }
        fn inserts(&self, s: &StripInfo) -> Vec<InsertState> {
            self.0.inserts(s)
        }
        fn unison(&self, s: &StripInfo) -> InsertState {
            self.0.unison(s)
        }
        fn sends(&self, s: &StripInfo) -> Vec<SendState> {
            self.0.sends(s)
        }
        fn records_pre_effects(&self, s: &StripInfo) -> bool {
            self.0.records_pre_effects(s)
        }
        fn meters(&self, s: &StripInfo) -> MeterFrame {
            self.0.meters(s)
        }
        fn monitor(&self) -> MonitorState {
            self.0.monitor()
        }
        fn monitor_meters(&self) -> MeterFrame {
            self.0.monitor_meters()
        }
        fn apply(&mut self, _action: &Action) {}
        fn insert_refusal(&self, slot: &ControlId, name: &str) -> Option<String> {
            (slot.control == Control::Insert(1)).then(|| format!("{name}: stereo only"))
        }
    }

    #[test]
    fn a_slot_that_refuses_the_plug_in_is_forbidden_and_the_drop_says_why() {
        let m = Refusing(PreviewMixer::default());
        let strip = m.inputs()[0].id.clone();
        let slot = |k: u8, y: f32| Hit {
            rect: Rect::new(0.0, y, 80.0, 48.0),
            id: ControlId { strip: strip.clone(), control: Control::Insert(k) },
            kind: HitKind::Tap,
        };
        let hits = [slot(0, 0.0), slot(1, 100.0)];
        let mut v = ViewState::default();
        v.begin_drag(DragPayload::Plugin { name: "UAD X".into() }, 300.0, 300.0, Rect::new(300.0, 300.0, 80.0, 48.0));
        // Over insert 1: forbidden (no target, so no highlight; the ghost dims), with the reason.
        assert!(v.drag_motion(&hits, 40.0, 120.0, false, &m));
        let d = v.drag.as_ref().unwrap();
        assert!(d.target.is_none());
        let (hit, why) = d.forbidden.as_ref().expect("forbidden");
        assert_eq!((&hit.id.control, why.as_str()), (&Control::Insert(1), "UAD X: stereo only"));
        // Over insert 0: a target.
        v.drag_motion(&hits, 40.0, 20.0, false, &m);
        let d = v.drag.as_ref().unwrap();
        assert_eq!(d.target.as_ref().map(|h| h.id.control.clone()), Some(Control::Insert(0)));
        assert!(d.forbidden.is_none());
        // Dropped on insert 1: the assignment still goes to the model (which refuses it and
        // keeps the reason), and the slot shows the model's notice for a while.
        v.drag_motion(&hits, 40.0, 120.0, false, &m);
        let fx = v.drag_release(&m).unwrap();
        let to_slot_1 = |id: &ControlId, name: &str| id.control == Control::Insert(1) && name == "UAD X";
        assert!(matches!(&fx[..], [Effect::SetText(id, name)] if to_slot_1(id, name)), "{fx:?}");
        assert_eq!(v.drop_notice.as_ref().map(|n| n.rect), Some(hits[1].rect));
        // The next press clears it.
        v.drag_press(&hits, 500.0, 500.0, &m);
        assert!(v.drop_notice.is_none());
    }
}
