//! Hit regions and pointer interaction → model actions.

use std::sync::Arc;

use houston::Rect;

/// Which control of which strip ("in0", "aux1", "monitor", …).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ControlId {
    pub strip: Arc<str>,
    pub control: Control,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Fader,
    PanL,
    PanR,
    Solo,
    Mute,
    PrePost,
    Mono,
    RecMon,
    /// A preamp layout component id (`preamp_gain`, `48v`, `pad`, `low_cut`, `phase`, …).
    Preamp(Arc<str>),
    Insert(u8),
    SendLevel(u8),
    SendMute(u8),
    MonitorLevel,
    MonitorMono,
    MonitorMute,
    // Plug-in windows (`plugin-ui`); the strip is the window's slot id. Append-only below.
    /// A plug-in parameter by ordinal (`<slot>/parameters/<k>`): knob and bar drags.
    PluginParam(u16),
    /// A plug-in panel spec element by index (switch, button, zone, menu): tap.
    PluginElement(u16),
    /// Set plug-in parameter `.0` to step `.1` (segmented switches, step selectors): tap.
    PluginStep(u16, u16),
    /// A button of the plug-in window's chrome (title bar, UAPW toolbar, footer): tap.
    PluginWindow(PluginWindowButton),
}

/// Plug-in window chrome buttons (UAD Console's `Rack_PluginWindow` / UAPW toolbar).
/// Append-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginWindowButton {
    Close,
    /// ⋮ menu at the toolbar's left.
    More,
    PresetPrev,
    PresetNext,
    /// The current-preset field (opens the preset browser).
    Preset,
    /// Slot power / bypass.
    Power,
    /// The engine button at the toolbar's right ("UAD-2 •••").
    Engine,
    Favorite,
    Compare,
    Copy,
    Paste,
    Help,
    /// Rack (vertical) / panel view toggle.
    RackView,
    /// The footer status plate.
    Status,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HitKind {
    /// Click toggles.
    Toggle,
    /// Click activates (e.g. open an insert).
    Tap,
    /// Vertical drag over `span` points covers 0…1; `value` is the current value; double-click
    /// resets to `reset`.
    Drag { value: f64, span: f32, reset: Option<f64> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub id: ControlId,
    pub kind: HitKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    GestureBegin(ControlId),
    Set(ControlId, f64),
    GestureEnd(ControlId),
    Toggle(ControlId),
    Tap(ControlId),
}

struct Active {
    id: ControlId,
    start_y: f32,
    start_x: f32,
    start_value: f64,
    span: f32,
}

/// Pointer state machine. Hits are searched topmost-first (last registered wins).
#[derive(Default)]
pub struct Interaction {
    active: Option<Active>,
}

impl Interaction {
    pub fn pointer_down(&mut self, hits: &[Hit], x: f32, y: f32, double: bool) -> Vec<Action> {
        let Some(hit) = hits.iter().rev().find(|h| h.rect.contains(x, y)) else { return vec![] };
        match &hit.kind {
            HitKind::Toggle => vec![Action::Toggle(hit.id.clone())],
            HitKind::Tap => vec![Action::Tap(hit.id.clone())],
            HitKind::Drag { value, span, reset } => {
                if double {
                    if let Some(r) = reset {
                        return vec![
                            Action::GestureBegin(hit.id.clone()),
                            Action::Set(hit.id.clone(), *r),
                            Action::GestureEnd(hit.id.clone()),
                        ];
                    }
                }
                self.active = Some(Active { id: hit.id.clone(), start_y: y, start_x: x, start_value: *value, span: *span });
                vec![Action::GestureBegin(hit.id.clone())]
            }
        }
    }

    /// `fine` divides the motion by 10 (Console's modifier-drag).
    pub fn pointer_move(&mut self, x: f32, y: f32, fine: bool) -> Vec<Action> {
        let Some(a) = &self.active else { return vec![] };
        let scale = if fine { 10.0 } else { 1.0 };
        let delta = ((a.start_y - y) as f64 + (x - a.start_x) as f64 * 0.25) / (a.span.max(1.0) as f64 * scale);
        vec![Action::Set(a.id.clone(), (a.start_value + delta).clamp(0.0, 1.0))]
    }

    pub fn pointer_up(&mut self) -> Vec<Action> {
        match self.active.take() {
            Some(a) => vec![Action::GestureEnd(a.id)],
            None => vec![],
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.active.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ControlId {
        ControlId { strip: "in0".into(), control: Control::Fader }
    }

    #[test]
    fn drag_and_reset() {
        let hits = vec![Hit { rect: Rect::new(0.0, 0.0, 50.0, 300.0), id: id(), kind: HitKind::Drag { value: 0.5, span: 200.0, reset: Some(0.78) } }];
        let mut i = Interaction::default();
        assert_eq!(i.pointer_down(&hits, 10.0, 100.0, false), vec![Action::GestureBegin(id())]);
        assert_eq!(i.pointer_move(10.0, 60.0, false), vec![Action::Set(id(), 0.7)]);
        assert_eq!(i.pointer_up(), vec![Action::GestureEnd(id())]);
        let reset = i.pointer_down(&hits, 10.0, 100.0, true);
        assert_eq!(reset[1], Action::Set(id(), 0.78));
    }
}
