//! Pointer and keyboard helpers that do not need a window: double-click detection, wheel
//! steps on drag controls, and the keyboard shortcut map.

use std::time::{Duration, Instant};

use rack::{Action, Hit, HitKind};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Value change per wheel line on a knob/fader (Option/Alt + wheel), and with Shift too.
pub const WHEEL_STEP: f64 = 0.01;
pub const WHEEL_STEP_FINE: f64 = 0.001;
/// Touchpad pixels per wheel line.
pub const PIXELS_PER_LINE: f64 = 24.0;

/// Second press within [`DOUBLE_CLICK`] at (nearly) the same spot.
#[derive(Default)]
pub struct ClickTracker {
    last: Option<(Instant, (f64, f64))>,
}

impl ClickTracker {
    /// Records a press at `pos` (pixels); returns whether it completes a double click.
    /// `slop` is the allowed movement in pixels.
    pub fn press(&mut self, now: Instant, pos: (f64, f64), slop: f64) -> bool {
        let double = self.last.is_some_and(|(t, p)| {
            now.duration_since(t) < DOUBLE_CLICK && (p.0 - pos.0).hypot(p.1 - pos.1) <= slop
        });
        // A double click consumes both presses: a third press starts over.
        self.last = if double { None } else { Some((now, pos)) };
        double
    }
}

/// Option/Alt + wheel over a `Drag` hit at (`x`, `y`) (points): a one-step gesture. None when
/// no drag control is under the pointer (the wheel then scrolls or pans).
pub fn wheel_actions(hits: &[Hit], x: f32, y: f32, lines: f64, fine: bool) -> Option<Vec<Action>> {
    let hit = hits.iter().rev().find(|h| h.rect.contains(x, y))?;
    let (HitKind::Drag { value, .. } | HitKind::Knob { value, .. }) = &hit.kind else {
        return None;
    };
    let step = if fine { WHEEL_STEP_FINE } else { WHEEL_STEP };
    let v = (value + lines * step).clamp(0.0, 1.0);
    let id = hit.id.clone();
    Some(vec![
        Action::GestureBegin(id.clone()),
        Action::Set(id.clone(), v),
        Action::GestureEnd(id),
    ])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Quit,
    ToggleFullscreen,
    ZoomReset,
    ZoomIn,
    ZoomOut,
    ToggleViewMode,
    ToggleCompact,
    ToggleStats,
    Escape,
}

/// Keyboard shortcuts. Characters are matched on the logical key (layout aware); the physical
/// key is the fallback for layouts whose keys produce non-ASCII characters.
pub fn shortcut(key: &Key, physical: PhysicalKey, mods: ModifiersState) -> Option<Command> {
    let ctrl = mods.control_key() || mods.super_key();
    match key {
        Key::Named(NamedKey::Escape) => return Some(Command::Escape),
        Key::Named(NamedKey::F1) => return Some(Command::ToggleStats),
        Key::Named(NamedKey::F11) => return Some(Command::ToggleFullscreen),
        Key::Named(NamedKey::Tab) if !ctrl && !mods.alt_key() => {
            return Some(Command::ToggleCompact);
        }
        _ => {}
    }
    if !ctrl {
        return None;
    }
    let ch = match key {
        Key::Character(s) if s.is_ascii() => s.to_ascii_lowercase(),
        _ => match physical {
            PhysicalKey::Code(KeyCode::KeyQ) => "q".into(),
            PhysicalKey::Code(KeyCode::Digit0 | KeyCode::Numpad0) => "0".into(),
            PhysicalKey::Code(KeyCode::Digit1 | KeyCode::Numpad1) => "1".into(),
            PhysicalKey::Code(KeyCode::Equal | KeyCode::NumpadAdd) => "=".into(),
            PhysicalKey::Code(KeyCode::Minus | KeyCode::NumpadSubtract) => "-".into(),
            _ => return None,
        },
    };
    match ch.as_str() {
        "q" => Some(Command::Quit),
        "0" | ")" => Some(Command::ZoomReset),
        "1" | "!" => Some(Command::ToggleViewMode),
        "=" | "+" => Some(Command::ZoomIn),
        "-" | "_" => Some(Command::ZoomOut),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use houston::Rect;
    use rack::{Control, ControlId};

    #[test]
    fn double_click() {
        let mut c = ClickTracker::default();
        let t = Instant::now();
        assert!(!c.press(t, (10.0, 10.0), 4.0));
        assert!(c.press(t + Duration::from_millis(200), (12.0, 11.0), 4.0));
        assert!(
            !c.press(t + Duration::from_millis(300), (12.0, 11.0), 4.0),
            "third press starts over"
        );
        assert!(
            !c.press(t + Duration::from_millis(900), (12.0, 11.0), 4.0),
            "too slow"
        );
        assert!(
            !c.press(t + Duration::from_millis(1000), (40.0, 11.0), 4.0),
            "moved"
        );
    }

    #[test]
    fn wheel_steps_drag_controls() {
        let id = ControlId {
            strip: "in0".into(),
            control: Control::Fader,
        };
        let hits = vec![
            Hit {
                rect: Rect::new(0.0, 0.0, 50.0, 300.0),
                id: id.clone(),
                kind: HitKind::Drag {
                    value: 0.5,
                    span: 200.0,
                    reset: None,
                },
            },
            Hit {
                rect: Rect::new(0.0, 0.0, 50.0, 20.0),
                id: ControlId {
                    strip: "in0".into(),
                    control: Control::Mute,
                },
                kind: HitKind::Toggle,
            },
        ];
        let set = |a: Vec<Action>| match &a[..] {
            [
                Action::GestureBegin(_),
                Action::Set(i, v),
                Action::GestureEnd(_),
            ] if *i == id => *v,
            other => panic!("{other:?}"),
        };
        assert!((set(wheel_actions(&hits, 10.0, 100.0, 2.0, false).unwrap()) - 0.52).abs() < 1e-9);
        assert!((set(wheel_actions(&hits, 10.0, 100.0, -1.0, true).unwrap()) - 0.499).abs() < 1e-9);
        assert!(
            wheel_actions(&hits, 10.0, 10.0, 1.0, false).is_none(),
            "toggle on top"
        );
        assert!(wheel_actions(&hits, 100.0, 10.0, 1.0, false).is_none());
    }

    #[test]
    fn shortcuts() {
        let ctrl = ModifiersState::CONTROL;
        let none = ModifiersState::empty();
        let ch = |s: &str| Key::Character(s.into());
        let unid = PhysicalKey::Code(KeyCode::KeyA);
        assert_eq!(shortcut(&ch("q"), unid, ctrl), Some(Command::Quit));
        assert_eq!(shortcut(&ch("q"), unid, none), None);
        assert_eq!(
            shortcut(&ch("+"), unid, ctrl | ModifiersState::SHIFT),
            Some(Command::ZoomIn)
        );
        assert_eq!(shortcut(&ch("-"), unid, ctrl), Some(Command::ZoomOut));
        assert_eq!(shortcut(&ch("0"), unid, ctrl), Some(Command::ZoomReset));
        assert_eq!(
            shortcut(&ch("й"), PhysicalKey::Code(KeyCode::KeyQ), ctrl),
            Some(Command::Quit)
        );
        assert_eq!(
            shortcut(&ch("a"), PhysicalKey::Code(KeyCode::KeyQ), ctrl),
            None,
            "AZERTY: logical key wins"
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::Tab), unid, none),
            Some(Command::ToggleCompact)
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::F1), unid, none),
            Some(Command::ToggleStats)
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::F11), unid, none),
            Some(Command::ToggleFullscreen)
        );
        assert_eq!(
            shortcut(&Key::Named(NamedKey::Escape), unid, none),
            Some(Command::Escape)
        );
    }
}
