//! Panels drawn above the mixer (plug-in editors, popovers) and the hook that opens a plug-in
//! panel when an insert is tapped.
//!
//! The plugin-ui crate plugs in by implementing [`PluginHook`] (or by exposing a view that a
//! few-line adapter here wraps in a [`Panel`]); swap [`PlaceholderHook`] for it in `app.rs`.

use houston::{Rect, Rgba};
use rack::model::InsertState;
use rack::{Align, MixerModel, Ui};

/// Insert slot number used for the UNISON slot (`Control::Insert(255)`).
pub const UNISON_SLOT: u8 = 255;

/// Pointer input for a panel, in panel-local points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerEvent {
    Down {
        x: f32,
        y: f32,
        double: bool,
    },
    /// `fine`: Shift or Alt held (fine adjust).
    Move {
        x: f32,
        y: f32,
        fine: bool,
    },
    Up {
        x: f32,
        y: f32,
    },
    /// `lines` > 0 scrolls up / increases.
    Wheel {
        x: f32,
        y: f32,
        lines: f32,
        fine: bool,
    },
}

impl PointerEvent {
    pub fn pos(&self) -> (f32, f32) {
        match *self {
            PointerEvent::Down { x, y, .. }
            | PointerEvent::Move { x, y, .. }
            | PointerEvent::Up { x, y }
            | PointerEvent::Wheel { x, y, .. } => (x, y),
        }
    }

    fn translated(self, dx: f32, dy: f32) -> Self {
        match self {
            PointerEvent::Down { x, y, double } => PointerEvent::Down {
                x: x + dx,
                y: y + dy,
                double,
            },
            PointerEvent::Move { x, y, fine } => PointerEvent::Move {
                x: x + dx,
                y: y + dy,
                fine,
            },
            PointerEvent::Up { x, y } => PointerEvent::Up {
                x: x + dx,
                y: y + dy,
            },
            PointerEvent::Wheel { x, y, lines, fine } => PointerEvent::Wheel {
                x: x + dx,
                y: y + dy,
                lines,
                fine,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelResponse {
    Ignored,
    /// Consumed; redraw.
    Handled,
    /// Close this panel.
    Close,
}

/// Something drawn above the mixer.
pub trait Panel {
    /// Whether this panel already edits the requested insert.
    fn matches_plugin(&self, _strip_id: &str, _slot: u8, _model: &dyn MixerModel) -> bool {
        false
    }
    /// Preferred size in window points; the stack centres it in the visible area.
    fn size(&self) -> (f32, f32);
    /// Draws into `rect` (window points; drawing is clipped to it). Hit regions added to `ui`
    /// are dropped: panels hit-test their own [`PointerEvent`]s.
    fn build(&mut self, ui: &mut Ui, rect: Rect);
    fn pointer(&mut self, _ev: PointerEvent, _model: &mut dyn MixerModel) -> PanelResponse {
        PanelResponse::Ignored
    }
    /// Modal panels dim and block the mixer, and close on Escape or a click outside.
    fn modal(&self) -> bool {
        true
    }
    /// Wants 30 Hz redraws while open (meters, animations).
    fn live(&self) -> bool {
        false
    }
    /// Called before each build with the model (read state, keep subscriptions).
    fn sync(&mut self, _model: &mut dyn MixerModel) {}
    /// Mixer actions the panel's menus produced (a plug-in window's ⋮ menu rows are mixer
    /// controls); the shell routes them through the mixer.
    fn take_mixer_actions(&mut self) -> Vec<rack::Action> {
        Vec::new()
    }
    /// Draws the panel's menus after its clip is lifted (they may leave the panel).
    fn build_popups(&mut self, _ui: &mut Ui, _rect: Rect) {}
    /// Where those menus are (window points), so clicks on them reach the panel.
    fn popup_rect(&self) -> Option<Rect> {
        None
    }
    /// Opens one of the panel's menus by name (renders: a plug-in window's `assign`, `more`,
    /// `help`). False when it has none.
    fn open_menu(&mut self, _which: &str) -> bool {
        false
    }
    /// What it shows (a plug-in window: "in0:1"), so reopening it raises it instead.
    fn key(&self) -> Option<String> {
        None
    }
    /// Its window title ("<strip>, Insert N").
    fn title(&self) -> Option<String> {
        None
    }
    /// Whether a press at (`x`, `y`) (window points) is on its title bar, off its controls:
    /// the press moves the window.
    fn title_bar_at(&self, _x: f32, _y: f32) -> bool {
        false
    }
    /// Its hit regions at the last build (window points), for `--drive` scripts.
    fn hits(&self) -> &[rack::Hit] {
        &[]
    }
}

/// Called when the user opens a plug-in insert (`Action::Tap` on `Control::Insert(slot)`).
/// `slot` is 0-based, [`UNISON_SLOT`] for UNISON.
pub trait PluginHook {
    fn on_open_plugin(
        &mut self,
        strip_id: &str,
        slot: u8,
        model: &dyn MixerModel,
    ) -> Option<Box<dyn Panel>>;
}

/// Opens [`PlaceholderPanel`]s until the plugin-ui crate provides real editors.
pub struct PlaceholderHook;

impl PluginHook for PlaceholderHook {
    fn on_open_plugin(
        &mut self,
        strip_id: &str,
        slot: u8,
        model: &dyn MixerModel,
    ) -> Option<Box<dyn Panel>> {
        let strip = model
            .inputs()
            .into_iter()
            .chain(model.auxes())
            .find(|s| &*s.id == strip_id);
        let (title, insert) = match &strip {
            Some(s) => {
                let insert = if slot == UNISON_SLOT {
                    Some(model.unison(s))
                } else {
                    model.inserts(s).get(slot as usize).cloned()
                };
                (format!("{} · {}", s.title, model.main(s).name), insert)
            }
            None => (strip_id.to_string(), None),
        };
        let insert = match insert {
            Some(InsertState::Loaded { name, power }) => {
                format!("{name}{}", if power { "" } else { " (off)" })
            }
            Some(InsertState::Empty) => "empty slot".into(),
            None => "unknown slot".into(),
        };
        let slot = if slot == UNISON_SLOT {
            "UNISON".to_string()
        } else {
            format!("INSERT {}", slot as u32 + 1)
        };
        Some(Box::new(PlaceholderPanel {
            lines: [format!("{title} ({strip_id})"), format!("{slot}: {insert}")],
        }))
    }
}

/// Shows which strip and slot was opened.
pub struct PlaceholderPanel {
    lines: [String; 2],
}

const CLOSE_BOX: Rect = Rect::new(-28.0, 6.0, 22.0, 22.0); // from the top-right corner

impl Panel for PlaceholderPanel {
    fn size(&self) -> (f32, f32) {
        (420.0, 190.0)
    }

    fn build(&mut self, ui: &mut Ui, r: Rect) {
        ui.fill(r, Rgba::rgb8(82, 87, 89));
        ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(35, 37, 40));
        ui.fill(
            Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 33.0),
            Rgba::rgb8(28, 30, 32),
        );
        let title = Ui::plain_style("Helvetica Neue", 13.0, true, Rgba::WHITE.with_alpha(0.9));
        ui.text(
            "PLUG-IN",
            title,
            Rect::new(r.x + 14.0, r.y, r.w - 60.0, 34.0),
            Align::Left,
        );
        let close = CLOSE_BOX.offset(r.max_x(), r.y);
        ui.fill(close, Rgba::rgb8(60, 63, 66));
        ui.text(
            "×",
            Ui::plain_style("Helvetica", 16.0, false, Rgba::WHITE.with_alpha(0.8)),
            close,
            Align::Center,
        );
        let body = Ui::plain_style("Helvetica", 13.0, false, Rgba::WHITE.with_alpha(0.85));
        for (i, line) in self.lines.iter().enumerate() {
            ui.text(
                line,
                body.clone(),
                Rect::new(r.x + 18.0, r.y + 50.0 + i as f32 * 24.0, r.w - 36.0, 22.0),
                Align::Left,
            );
        }
        let hint = Ui::plain_style("Helvetica", 11.0, false, Rgba::rgb8(255, 197, 74));
        ui.text(
            "Plug-in editors arrive with the plugin-ui crate.",
            hint.clone(),
            Rect::new(r.x + 18.0, r.y + 112.0, r.w - 36.0, 20.0),
            Align::Left,
        );
        let dim = Ui::plain_style("Helvetica", 11.0, false, Rgba::WHITE.with_alpha(0.45));
        ui.text(
            "Esc or click outside to close",
            dim,
            Rect::new(r.x + 18.0, r.max_y() - 34.0, r.w - 36.0, 20.0),
            Align::Left,
        );
    }

    fn pointer(&mut self, ev: PointerEvent, _model: &mut dyn MixerModel) -> PanelResponse {
        let (w, _) = self.size();
        match ev {
            PointerEvent::Down { x, y, .. } if CLOSE_BOX.offset(w, 0.0).contains(x, y) => {
                PanelResponse::Close
            }
            PointerEvent::Down { .. } => PanelResponse::Handled,
            _ => PanelResponse::Ignored,
        }
    }
}

/// Panels above the mixer, topmost last.
pub struct OverlayStack {
    panels: Vec<Box<dyn Panel>>,
    hook: Box<dyn PluginHook>,
    /// Visible window area (points) at the last build; panels are centred in it.
    area: Rect,
    /// The top panel took the pointer-down: moves and the release go to it.
    captured: bool,
    /// A panel reopened while open (layer id): the shell focuses its window.
    pub focus: Option<String>,
}

pub const DIM: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.45,
};

impl OverlayStack {
    pub fn new(hook: Box<dyn PluginHook>) -> Self {
        Self {
            panels: Vec::new(),
            hook,
            area: Rect::default(),
            captured: false,
            focus: None,
        }
    }

    pub fn live(&self) -> bool {
        self.panels.iter().any(|p| p.live())
    }

    pub fn push(&mut self, panel: Box<dyn Panel>) {
        self.panels.push(panel);
        self.captured = false;
    }

    /// The layer id of the topmost panel (its OS window, where windows are separate).
    pub fn top_layer_id(&self) -> Option<String> {
        self.panels.len().checked_sub(1).map(|i| self.layer_id(i))
    }

    /// Asks the hook for a panel for `strip_id`/`slot` and opens it, or raises the one already
    /// open for it (Console has one window per insert). Returns whether one opened.
    pub fn on_open_plugin(&mut self, strip_id: &str, slot: u8, model: &dyn MixerModel) -> bool {
        if let Some(index) = self.panels.iter().position(|p| p.matches_plugin(strip_id, slot, model)) {
            let panel = self.panels.remove(index);
            self.push(panel);
            return true;
        }
        match self.hook.on_open_plugin(strip_id, slot, model) {
            Some(p) => {
                let open = p.key().and_then(|k| self.panels.iter().position(|q| q.key().as_deref() == Some(&k)));
                match open {
                    Some(i) => {
                        let id = self.layer_id(i);
                        self.raise(&id);
                        self.focus = Some(id);
                    }
                    None => self.push(p),
                }
                true
            }
            None => false,
        }
    }

    /// The layer (separate window) id of panel `i`: stable while it is open.
    fn layer_id(&self, i: usize) -> String {
        match self.panels[i].key() {
            Some(k) => format!("plugin:{k}"),
            None => format!("plugin:{i}"),
        }
    }

    fn index_of(&self, layer_id: &str) -> Option<usize> {
        (0..self.panels.len()).find(|&i| self.layer_id(i) == layer_id)
    }

    /// Brings the panel of layer `layer_id` to the top (pointer input goes to the top panel).
    pub fn raise(&mut self, layer_id: &str) {
        if let Some(i) = self.index_of(layer_id).filter(|&i| i + 1 < self.panels.len()) {
            let p = self.panels.remove(i);
            self.panels.push(p);
            self.captured = false;
        }
    }

    /// Closes the panel of layer `layer_id` (its window was closed). False when there is none.
    pub fn close(&mut self, layer_id: &str) -> bool {
        let Some(i) = self.index_of(layer_id) else { return false };
        self.panels.remove(i);
        self.captured = false;
        true
    }

    /// The window title of layer `layer_id`'s panel.
    pub fn title(&self, layer_id: &str) -> Option<String> {
        self.panels[self.index_of(layer_id)?].title()
    }

    /// A press at (`x`, `y`) (window points) on that panel's title bar (`Panel::title_bar_at`).
    pub fn title_bar_at(&self, layer_id: &str, x: f32, y: f32) -> bool {
        self.index_of(layer_id).is_some_and(|i| self.panels[i].title_bar_at(x, y))
    }

    /// The top panel's hit regions (pointer input goes to it), for `--drive` scripts.
    pub fn top_hits(&self) -> &[rack::Hit] {
        self.panels.last().map_or(&[], |p| p.hits())
    }

    /// Opens a menu of the top panel (`Panel::open_menu`).
    pub fn open_menu(&mut self, which: &str) -> bool {
        self.panels.last_mut().is_some_and(|p| p.open_menu(which))
    }

    /// Mixer actions from every panel's menus, oldest first.
    pub fn take_mixer_actions(&mut self) -> Vec<rack::Action> {
        self.panels.iter_mut().flat_map(|p| p.take_mixer_actions()).collect()
    }

    /// Lets every panel read the model before the next build.
    pub fn sync(&mut self, model: &mut dyn MixerModel) {
        for p in &mut self.panels {
            p.sync(model);
        }
    }

    /// Takes the floating (non-modal) panels out of the stack, oldest first: Window ›
    /// Show/Hide Floating Windows hides the plug-in windows (their OS windows close with
    /// their layers).
    pub fn take_floating(&mut self) -> Vec<Box<dyn Panel>> {
        let (floating, modal): (Vec<_>, Vec<_>) = std::mem::take(&mut self.panels).into_iter().partition(|p| !p.modal());
        self.panels = modal;
        self.captured = false;
        floating
    }

    /// Puts hidden panels back under the open ones, skipping any reopened since (same key).
    pub fn restore_floating(&mut self, panels: Vec<Box<dyn Panel>>) {
        let open: Vec<String> = self.panels.iter().filter_map(|p| p.key()).collect();
        let back: Vec<_> = panels.into_iter().filter(|p| p.key().is_none_or(|k| !open.contains(&k))).collect();
        self.panels.splice(0..0, back);
        self.captured = false;
    }

    /// Escape: closes the top modal panel.
    pub fn dismiss_top(&mut self) -> bool {
        if self.panels.last().is_some_and(|p| p.modal()) {
            self.panels.pop();
            self.captured = false;
            return true;
        }
        false
    }

    fn rect_of(&self, p: &dyn Panel) -> Rect {
        let (w, h) = p.size();
        let r = self.area.centered(w, h);
        Rect::new(
            r.x.max(self.area.x).round(),
            r.y.max(self.area.y).round(),
            w,
            h,
        )
    }

    /// Draws the panels after the mixer. `area` is the visible part of the window in points.
    pub fn build(&mut self, ui: &mut Ui, area: Rect) {
        self.area = area;
        let hits = ui.hits.len();
        for i in 0..self.panels.len() {
            let r = self.rect_of(&*self.panels[i]);
            if self.panels[i].modal() && !ui.windowed() {
                ui.fill(area, DIM);
            }
            // Console's plug-in windows are windows of their own (`rack::layers`).
            ui.begin_layer(self.layer_id(i), rack::LayerKind::Popover, r, 0.0);
            ui.clip(Some(r));
            self.panels[i].build(ui, r);
            ui.clip(None);
            ui.end_layer();
            match self.panels[i].popup_rect() {
                Some(p) => {
                    ui.begin_layer(self.layer_id(i).replacen("plugin:", "plugin-menu:", 1), rack::LayerKind::Menu, p, 0.0);
                    self.panels[i].build_popups(ui, r);
                    ui.end_layer();
                }
                None => self.panels[i].build_popups(ui, r),
            }
        }
        ui.hits.truncate(hits);
    }

    /// Routes a pointer event (window points) to the top panel.
    pub fn pointer(&mut self, ev: PointerEvent, model: &mut dyn MixerModel) -> Routed {
        let Some(top) = self.panels.last() else {
            return Routed::ToMixer;
        };
        let r = self.rect_of(&**top);
        let modal = top.modal();
        let (x, y) = ev.pos();
        let inside = r.contains(x, y) || top.popup_rect().is_some_and(|p| p.contains(x, y));
        let forward = match ev {
            PointerEvent::Down { .. } => {
                self.captured = inside;
                if !inside {
                    if modal {
                        self.panels.pop();
                        return Routed::Changed;
                    }
                    return Routed::ToMixer;
                }
                true
            }
            PointerEvent::Move { .. } => self.captured,
            PointerEvent::Up { .. } => std::mem::take(&mut self.captured),
            PointerEvent::Wheel { .. } => inside,
        };
        if forward {
            let top = self.panels.last_mut().expect("top panel");
            return match top.pointer(ev.translated(-r.x, -r.y), model) {
                PanelResponse::Ignored => Routed::Consumed,
                PanelResponse::Handled => Routed::Changed,
                PanelResponse::Close => {
                    self.panels.pop();
                    self.captured = false;
                    Routed::Changed
                }
            };
        }
        if modal {
            Routed::Consumed
        } else {
            Routed::ToMixer
        }
    }
}

/// Where a pointer event went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Routed {
    /// Not the overlay's: the mixer handles it.
    ToMixer,
    /// Taken by the overlay, nothing changed.
    Consumed,
    /// Taken by the overlay; redraw.
    Changed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rack::PreviewMixer;

    /// A non-modal panel (a plug-in window).
    struct Free(&'static str);
    impl Panel for Free {
        fn size(&self) -> (f32, f32) {
            (100.0, 100.0)
        }
        fn build(&mut self, _ui: &mut Ui, _rect: Rect) {}
        fn modal(&self) -> bool {
            false
        }
        fn key(&self) -> Option<String> {
            Some(self.0.into())
        }
    }

    /// Show/Hide Floating Windows: the plug-in windows leave the stack (modal panels stay) and
    /// come back under what opened since, without doubling one reopened meanwhile.
    #[test]
    fn floating_panels_hide_and_return() {
        let mut stack = OverlayStack::new(Box::new(PlaceholderHook));
        stack.push(Box::new(Free("in0:0")));
        stack.push(Box::new(Free("in1:0")));
        let hidden = stack.take_floating();
        assert_eq!(hidden.len(), 2);
        assert!(stack.panels.is_empty());
        stack.push(Box::new(Free("in1:0")));
        stack.restore_floating(hidden);
        let keys: Vec<_> = stack.panels.iter().filter_map(|p| p.key()).collect();
        assert_eq!(keys, ["in0:0", "in1:0"]);
    }

    #[test]
    fn reopening_insert_raises_same_panel_and_closed_insert_can_reopen() {
        use std::{cell::Cell, rc::Rc};
        struct InsertPanel(String, u8);
        impl Panel for InsertPanel {
            fn matches_plugin(&self, strip: &str, slot: u8, _: &dyn MixerModel) -> bool {
                self.0 == strip && self.1 == slot
            }
            fn size(&self) -> (f32, f32) { (100.0, 100.0) }
            fn build(&mut self, _: &mut Ui, _: Rect) {}
        }
        struct Hook(Rc<Cell<usize>>);
        impl PluginHook for Hook {
            fn on_open_plugin(&mut self, strip: &str, slot: u8, _: &dyn MixerModel) -> Option<Box<dyn Panel>> {
                self.0.set(self.0.get() + 1);
                Some(Box::new(InsertPanel(strip.into(), slot)))
            }
        }
        let model = PreviewMixer::default();
        let opens = Rc::new(Cell::new(0));
        let mut stack = OverlayStack::new(Box::new(Hook(opens.clone())));
        for (strip, slot) in [("in0", 0), ("in0", 1), ("in1", 0)] {
            assert!(stack.on_open_plugin(strip, slot, &model));
        }
        let first = &*stack.panels[0] as *const dyn Panel;
        for _ in 0..5 {
            stack.captured = true;
            assert!(stack.on_open_plugin("in0", 0, &model));
            assert_eq!(stack.panels.len(), 3);
            assert_eq!(stack.top_layer_id().as_deref(), Some(stack.layer_id(2).as_str()));
            assert!(std::ptr::eq(first, &**stack.panels.last().unwrap()));
            assert!(!stack.captured);
        }
        assert_eq!(opens.get(), 3);
        assert!(stack.dismiss_top());
        assert!(stack.on_open_plugin("in0", 0, &model));
        assert_eq!(opens.get(), 4);
        assert_eq!(stack.panels.len(), 3);
    }

    #[test]
    fn placeholder_opens_and_dismisses() {
        let mut model = PreviewMixer::default();
        let mut stack = OverlayStack::new(Box::new(PlaceholderHook));
        assert!(stack.on_open_plugin("in0", 2, &model));
        let p = PlaceholderHook.on_open_plugin("in0", UNISON_SLOT, &model);
        assert!(p.is_some());
        stack.area = Rect::new(0.0, 0.0, 1000.0, 800.0);
        // Inside: consumed, stays open.
        assert_eq!(
            stack.pointer(
                PointerEvent::Down {
                    x: 500.0,
                    y: 400.0,
                    double: false
                },
                &mut model
            ),
            Routed::Changed
        );
        assert_eq!(
            stack.pointer(
                PointerEvent::Move {
                    x: 1.0,
                    y: 1.0,
                    fine: false
                },
                &mut model
            ),
            Routed::Consumed
        );
        assert_eq!(
            stack.pointer(PointerEvent::Up { x: 500.0, y: 400.0 }, &mut model),
            Routed::Consumed
        );
        assert_eq!(stack.panels.len(), 1);
        // Outside a modal panel: consumed and closed.
        assert_eq!(
            stack.pointer(
                PointerEvent::Down {
                    x: 5.0,
                    y: 5.0,
                    double: false
                },
                &mut model
            ),
            Routed::Changed
        );
        assert!(stack.panels.is_empty());
        assert_eq!(
            stack.pointer(
                PointerEvent::Down {
                    x: 5.0,
                    y: 5.0,
                    double: false
                },
                &mut model
            ),
            Routed::ToMixer
        );
        // Close box and Escape.
        stack.on_open_plugin("aux0", 0, &model);
        let r = stack.rect_of(&**stack.panels.last().unwrap());
        assert_eq!(
            stack.pointer(
                PointerEvent::Down {
                    x: r.max_x() - 15.0,
                    y: r.y + 15.0,
                    double: false
                },
                &mut model
            ),
            Routed::Changed
        );
        assert!(stack.panels.is_empty());
        stack.on_open_plugin("aux0", 0, &model);
        assert!(stack.dismiss_top());
        assert!(!stack.dismiss_top());
    }
}
