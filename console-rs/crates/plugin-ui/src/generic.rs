//! The generic plug-in panel: every parameter as a row (index, name, a 0…1 bar or a step
//! selector, and the display text), built only from the source's parameter metadata, so it works
//! for any plug-in with or without a spec. Port of ConsoleLab's `GenericPluginView`.

use std::collections::BTreeSet;
use std::sync::Arc;

use houston::{Rect, Rgba};
use rack::{Action, Align, Control, ControlId, HitKind, Ui};

use crate::panel::{DRAG_SPAN, action_id};
use crate::params::{ParamSource, clamp01, step_index, step_value};

pub const ROW_H: f32 = 28.0;
pub const WIDTH: f32 = 600.0;
const PAD: f32 = 12.0;
/// Up to this many steps show as a segmented row; more show as ‹ label ›.
const MAX_SEGMENTS: usize = 6;

const BG: Rgba = Rgba::rgb8(33, 36, 38);
const TEXT: Rgba = Rgba::rgb8(230, 232, 234);
const DIM: Rgba = Rgba::rgb8(140, 146, 150);
const ACCENT: Rgba = Rgba::rgb8(255, 197, 74);

pub struct GenericPanel {
    target: Arc<str>,
    /// Restrict to these ordinals (e.g. the parameters a spec panel cannot reach).
    pub only: Option<BTreeSet<u16>>,
    /// Scroll offset in points (rows above it are not built).
    pub scroll: f32,
}

/// Geometry of one row, relative to the panel origin (before scrolling).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowLayout {
    pub row: Rect,
    pub index: Rect,
    pub name: Rect,
    pub control: Rect,
    pub value: Rect,
}

impl GenericPanel {
    pub fn new(target: impl Into<Arc<str>>) -> Self {
        Self { target: target.into(), only: None, scroll: 0.0 }
    }

    pub fn target(&self) -> &Arc<str> {
        &self.target
    }

    pub fn rows(&self, params: &dyn ParamSource) -> Vec<u16> {
        params.indices().into_iter().filter(|k| self.only.as_ref().is_none_or(|o| o.contains(k))).collect()
    }

    /// Natural size: [`WIDTH`] × rows (at least one row for the empty message).
    pub fn size(&self, params: &dyn ParamSource) -> (f32, f32) {
        (WIDTH, self.rows(params).len().max(1) as f32 * ROW_H + 2.0 * 4.0)
    }

    pub fn row_layout(width: f32, i: usize) -> RowLayout {
        let row = Rect::new(0.0, 4.0 + i as f32 * ROW_H, width, ROW_H);
        let index = Rect::new(PAD, row.y, 22.0, ROW_H);
        let name = Rect::new(index.max_x() + 10.0, row.y, 130.0, ROW_H);
        let value = Rect::new(width - PAD - 84.0, row.y, 84.0, ROW_H);
        let control = Rect::new(name.max_x() + 10.0, row.y + 3.0, (value.x - 10.0 - (name.max_x() + 10.0)).max(40.0), ROW_H - 6.0);
        RowLayout { row, index, name, control, value }
    }

    pub fn build(&self, ui: &mut Ui, origin: Rect, params: &dyn ParamSource) {
        ui.clip(Some(origin));
        ui.fill(origin, BG);
        let rows = self.rows(params);
        let small = Ui::plain_style("Helvetica", 10.0, false, DIM);
        let label = Ui::plain_style("Helvetica", 12.0, true, TEXT);
        let value_style = Ui::plain_style("Helvetica", 11.0, false, TEXT);
        if rows.is_empty() {
            ui.text("The engine lists no parameters for this plug-in.", small, Rect::new(origin.x, origin.y, origin.w, ROW_H * 2.0), Align::Center);
            ui.clip(None);
            return;
        }
        for (i, &k) in rows.iter().enumerate() {
            let l = Self::row_layout(origin.w, i);
            let dy = origin.y - self.scroll;
            if l.row.max_y() + dy < origin.y || l.row.y + dy > origin.max_y() {
                continue;
            }
            let at = |r: Rect| r.offset(origin.x, dy);
            let v = params.normalized(k).unwrap_or(0.0);
            ui.text(&k.to_string(), small.clone(), at(l.index), Align::Right);
            ui.text(&params.name(k).unwrap_or_else(|| format!("Parameter {k}")), label.clone(), at(l.name), Align::Left);
            let steps = params.steps(k);
            let c = at(l.control);
            if steps.len() > 1 {
                self.step_selector(ui, k, &steps, step_index(v, steps.len()), c);
            } else {
                let track = Rect::new(c.x, c.y + (c.h - 6.0) / 2.0, c.w, 6.0);
                ui.fill(track, Rgba::WHITE.with_alpha(0.12));
                ui.fill(Rect::new(track.x, track.y, (c.w * clamp01(v) as f32).max(6.0), 6.0), ACCENT.with_alpha(0.85));
                ui.hit(c, self.id(Control::PluginParam(k)), HitKind::Drag { value: v, span: DRAG_SPAN, reset: None });
            }
            let text = params.display(k).unwrap_or_else(|| format!("{v:.3}"));
            ui.text(&text, value_style.clone(), at(l.value), Align::Right);
            ui.fill(Rect::new(origin.x + PAD, at(l.row).max_y() - 0.5, origin.w - 2.0 * PAD, 0.5), Rgba::WHITE.with_alpha(0.08));
        }
        ui.clip(None);
    }

    fn step_selector(&self, ui: &mut Ui, k: u16, labels: &[String], selected: usize, c: Rect) {
        let n = labels.len();
        if n <= MAX_SEGMENTS {
            let w = (c.w - 2.0 * (n - 1) as f32) / n as f32;
            for (i, label) in labels.iter().enumerate() {
                let r = Rect::new(c.x + i as f32 * (w + 2.0), c.y, w, c.h);
                let on = i == selected;
                ui.fill(r, if on { ACCENT.with_alpha(0.85) } else { Rgba::WHITE.with_alpha(0.1) });
                let style = Ui::plain_style("Helvetica", 10.0, on, if on { Rgba::BLACK } else { TEXT });
                ui.text(label, style, r, Align::Center);
                ui.hit(r, self.id(Control::PluginStep(k, i as u16)), HitKind::Tap);
            }
        } else {
            ui.fill(c, Rgba::WHITE.with_alpha(0.1));
            let arrow = Ui::plain_style("Helvetica", 12.0, true, TEXT);
            let left = Rect::new(c.x, c.y, 22.0, c.h);
            let right = Rect::new(c.max_x() - 22.0, c.y, 22.0, c.h);
            let mid = Rect::new(left.max_x(), c.y, (right.x - left.max_x()).max(0.0), c.h);
            ui.text("‹", arrow.clone(), left, Align::Center);
            ui.text("›", arrow, right, Align::Center);
            ui.text(&labels[selected.min(n - 1)], Ui::plain_style("Helvetica", 11.0, false, TEXT), mid, Align::Center);
            if selected > 0 {
                ui.hit(left, self.id(Control::PluginStep(k, (selected - 1) as u16)), HitKind::Tap);
            }
            if selected + 1 < n {
                ui.hit(right, self.id(Control::PluginStep(k, (selected + 1) as u16)), HitKind::Tap);
            }
            ui.hit(mid, self.id(Control::PluginStep(k, ((selected + 1) % n) as u16)), HitKind::Tap);
        }
    }

    fn id(&self, control: Control) -> ControlId {
        ControlId { strip: self.target.clone(), control }
    }

    /// Applies a rack [`Action`] aimed at this list; false when it is not for it.
    pub fn apply(&mut self, action: &Action, params: &mut dyn ParamSource) -> bool {
        let id = action_id(action);
        if id.strip != self.target {
            return false;
        }
        match (action, &id.control) {
            (Action::GestureBegin(_), Control::PluginParam(_)) => params.begin_gesture(),
            (Action::GestureEnd(_), Control::PluginParam(_)) => params.end_gesture(),
            (Action::Set(_, v), Control::PluginParam(k)) => params.set(*k, clamp01(*v)),
            (Action::Tap(_) | Action::Toggle(_), Control::PluginStep(k, s)) => {
                let n = params.steps(*k).len();
                params.commit(*k, step_value(*s as usize, n));
            }
            _ => return false,
        }
        true
    }

    /// Scrolls by `dy` points, clamped to the content height for a viewport of `view_h`.
    pub fn scroll_by(&mut self, dy: f32, view_h: f32, params: &dyn ParamSource) {
        let max = (self.size(params).1 - view_h).max(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
    }
}
