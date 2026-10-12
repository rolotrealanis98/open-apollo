//! Sessions: the navigator SESSION box's menu, the save-changes question and the Save As / Open
//! name fields. Console 1.2.9 `Rack_MixerPresets` (the SESSION box, componentID `mixer_presets`)
//! builds a popup menu (Rack.dll 0x180db27b0): New (1), Open... (2), Save (3), Save As... (4), a
//! separator and the Sessions folder (ids from 6, the open session ticked; folders are
//! sub-menus), a separator and the modified sessions (`[time] - name`, ids from 0x2717). Picking
//! New, Open or a session while the session is dirty first asks `Rack_Window`'s question
//! (0x18165c1a0): "SAVE SESSION" — `Console session "<name>" has been modified.` with Don't Save /
//! Cancel / Save (Save as... for a never-saved session).
//!
//! Differences from Console (scaffold): folders are flattened (`Folder/Name` rows) instead of
//! sub-menus; Open... takes a path on the engine host in a text field instead of a native file
//! chooser (the engine opens the file, so a client-side chooser would browse the wrong machine).

use std::sync::Arc;

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::popover::{MenuItem, Placement, pid};
use crate::surfaces::view::{Effect, Popover, PopoverState, TextEdit, TextTarget, ViewState};
use crate::ui::Ui;
use crate::Action;

/// What the SESSION menu, a shortcut or a menu bar asks for. Routed through
/// [`ViewState::route`], which asks about unsaved changes and opens the name fields; the rest
/// reaches [`MixerModel::apply`] as `Action::Tap(Control::Session(op))`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionOp {
    New,
    /// Opens the path field (model: [`Control::SessionOpenPath`] via `set_text`).
    Open,
    /// Saves to the open session's file; a never-saved session gets the Save As field.
    Save,
    /// Opens the name field (model: [`Control::SessionSaveAsName`] via `set_text`).
    SaveAs,
    /// A file of [`SessionInfo::files`] by index.
    Load(u16),
    /// A [`SessionInfo::modified`] entry by index.
    Restore(u16),
}

impl SessionOp {
    /// Replaces the open session (Console asks about unsaved changes first).
    pub fn replaces_session(self) -> bool {
        matches!(self, SessionOp::New | SessionOp::Open | SessionOp::Load(_) | SessionOp::Restore(_))
    }
}

/// The save-changes question's answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SaveChoice {
    DontSave,
    Cancel,
    Save,
}

/// A file of the Sessions folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFile {
    /// `[folder/]name`: what the model loads.
    pub path: String,
    /// What the menu shows (the path; Console nests folders as sub-menus).
    pub label: String,
}

/// The open session and the session menu's content ([`MixerModel::session`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionInfo {
    /// The name Console shows (`Session`, `- New Session -`).
    pub name: String,
    /// `[folder/]name` of the open session (ticks it in the list).
    pub current: String,
    pub dirty: bool,
    /// The session has a file, so Save needs no name.
    pub has_file: bool,
    /// The engine's progress text while it loads or saves (empty when idle).
    pub progress: String,
    pub files: Vec<SessionFile>,
    /// Modified-session labels (`[Sep 18, 2026 05:14:34 AM] - - New Session -`).
    pub modified: Vec<String>,
}

impl SessionInfo {
    /// What models without session data report: the name (and `*`) from `session_name`.
    pub fn from_name(session_name: &str) -> SessionInfo {
        let dirty = session_name.ends_with('*');
        let name = session_name.trim_end_matches('*').to_owned();
        SessionInfo { has_file: !name.is_empty(), current: name.clone(), name, dirty, ..Default::default() }
    }
}

pub(crate) fn sid(op: SessionOp) -> ControlId {
    ControlId { strip: SESSION_STRIP.into(), control: Control::Session(op) }
}

fn choice_id(c: SaveChoice) -> ControlId {
    ControlId { strip: SESSION_STRIP.into(), control: Control::SessionSaveChoice(c) }
}

/// Strip id of session controls.
pub const SESSION_STRIP: &str = "session";

impl ViewState {
    /// Opens the session menu anchored at `anchor` (the SESSION box).
    pub fn open_session_menu(&mut self, anchor: ControlId) {
        self.popover = Some(PopoverState { kind: Popover::Session, anchor });
    }

    /// A session request from the menu (or a shortcut): asks about unsaved changes, opens the
    /// Save As / Open fields, or hands the op to the model.
    pub fn route_session(&mut self, op: SessionOp, model: &dyn MixerModel) -> Vec<Effect> {
        let anchor = self.popover.as_ref().map(|p| p.anchor.clone()).unwrap_or_else(|| sid(op));
        self.popover = None;
        let info = model.session();
        if op.replaces_session() && info.dirty {
            self.popover = Some(PopoverState { kind: Popover::SaveChanges { then: op }, anchor });
            return Vec::new();
        }
        self.proceed(op, &info, anchor)
    }

    /// Runs `op` without asking about unsaved changes.
    fn proceed(&mut self, op: SessionOp, info: &SessionInfo, anchor: ControlId) -> Vec<Effect> {
        match op {
            SessionOp::Open => {
                self.text_edit = Some(TextEdit { target: TextTarget::SessionOpen, text: String::new() });
                self.popover = Some(PopoverState { kind: Popover::SessionOpen, anchor });
                Vec::new()
            }
            SessionOp::SaveAs => self.ask_save_as(info, None, anchor),
            SessionOp::Save if !info.has_file => self.ask_save_as(info, None, anchor),
            op => vec![model_tap(op)],
        }
    }

    fn ask_save_as(&mut self, info: &SessionInfo, then: Option<SessionOp>, anchor: ControlId) -> Vec<Effect> {
        let name = if info.has_file { info.name.clone() } else { String::new() };
        self.text_edit = Some(TextEdit { target: TextTarget::SessionSaveAs { then }, text: name });
        self.popover = Some(PopoverState { kind: Popover::SessionSaveAs, anchor });
        Vec::new()
    }

    /// The save-changes question's answer.
    pub(crate) fn session_choice(&mut self, choice: SaveChoice, model: &dyn MixerModel) -> Vec<Effect> {
        let Some(PopoverState { kind: Popover::SaveChanges { then }, anchor }) = self.popover.take() else { return Vec::new() };
        let info = model.session();
        match choice {
            SaveChoice::Cancel => Vec::new(),
            SaveChoice::DontSave => self.proceed(then, &info, anchor),
            SaveChoice::Save if info.has_file => {
                let mut out = vec![model_tap(SessionOp::Save)];
                // The save is queued ahead of the load on the same connection.
                out.extend(self.proceed(then, &SessionInfo { dirty: false, ..info }, anchor));
                out
            }
            SaveChoice::Save => self.ask_save_as(&info, Some(then), anchor),
        }
    }

    /// Text commits of the session fields.
    pub(crate) fn commit_session_text(&mut self, target: &TextTarget, text: String, model: &dyn MixerModel) -> Vec<Effect> {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        let set = |control| Effect::SetText(ControlId { strip: SESSION_STRIP.into(), control }, text.clone());
        match target {
            TextTarget::SessionOpen => vec![set(Control::SessionOpenPath)],
            TextTarget::SessionSaveAs { then } => {
                let mut out = vec![set(Control::SessionSaveAsName)];
                if let Some(op) = then {
                    let info = SessionInfo { dirty: false, has_file: true, ..model.session() };
                    let anchor = sid(*op);
                    out.extend(self.proceed(*op, &info, anchor));
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

fn model_tap(op: SessionOp) -> Effect {
    Effect::Model(Action::Tap(sid(op)))
}

/// The SESSION menu: New, Open..., Save, Save As..., the Sessions folder, the modified sessions.
pub fn session_menu_items(info: &SessionInfo) -> Vec<MenuItem> {
    let mut items = vec![
        MenuItem::new("New", sid(SessionOp::New)),
        MenuItem::new("Open...", sid(SessionOp::Open)),
        MenuItem::new("Save", sid(SessionOp::Save)),
        MenuItem::new("Save As...", sid(SessionOp::SaveAs)),
    ];
    for (i, f) in info.files.iter().enumerate() {
        let mut item = MenuItem::new(f.label.clone(), sid(SessionOp::Load(i as u16))).checked(f.path == info.current);
        if i == 0 {
            item = item.separated();
        }
        items.push(item);
    }
    for (i, m) in info.modified.iter().enumerate() {
        let mut item = MenuItem::new(m.clone(), sid(SessionOp::Restore(i as u16)));
        if i == 0 {
            item = item.separated();
        }
        items.push(item);
    }
    items
}

/// Draws the session popovers; false for other kinds.
pub(crate) fn draw_session_popover(ui: &mut Ui, model: &dyn MixerModel, kind: &Popover, anchor: Rect, bounds: Rect) -> bool {
    match kind {
        Popover::Session => {
            let info = model.session();
            let items = session_menu_items(&info);
            let width = items.iter().map(|i| i.label.chars().count()).max().unwrap_or(10).clamp(14, 48) as f32 * 6.4 + 40.0;
            ui.popover_menu(anchor, Placement::Below, bounds, None, &items, width);
            true
        }
        Popover::SaveChanges { .. } => {
            save_changes_dialog(ui, &model.session(), bounds);
            true
        }
        _ => false,
    }
}

/// `Rack_Window`'s question, centred in the window.
fn save_changes_dialog(ui: &mut Ui, info: &SessionInfo, bounds: Rect) {
    let (w, h) = (360.0, 118.0);
    let (cx, cy) = bounds.center();
    let r = Rect::new((cx - w / 2.0).round(), (cy - h / 2.0).round(), w, h);
    ui.fill(r.offset(2.0, 3.0), Rgba::BLACK.with_alpha(0.35));
    ui.fill(r, Rgba::rgb8(16, 17, 18));
    ui.fill(r.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(47, 50, 53));
    // Swallow taps on the body so they do not dismiss.
    ui.hit(r, pid(Control::PopoverDismiss), HitKind::Drag { value: 0.0, span: 1.0, reset: None });
    let band = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, 24.0);
    ui.fill(band, Rgba::rgb8(36, 38, 41));
    let mut title = ui.style("views", "Popover-Header");
    title.size = 11.0;
    ui.text("SAVE SESSION", title, band, Align::Center);
    let msg = if info.has_file {
        format!("Console session \"{}\" has been modified.", info.name)
    } else {
        "The Console session has been modified.".to_owned()
    };
    let body = Ui::plain_style("Helvetica", 12.0, false, Rgba::WHITE.with_alpha(0.85));
    ui.text(&msg, body, Rect::new(r.x + 16.0, r.y + 36.0, r.w - 32.0, 30.0), Align::Center);
    let by = r.max_y() - 34.0;
    ui.flat_button("Don't Save", false, Rect::new(r.x + 12.0, by, 96.0, 22.0), choice_id(SaveChoice::DontSave));
    ui.flat_button("Cancel", false, Rect::new(r.max_x() - 12.0 - 96.0 * 2.0 - 8.0, by, 96.0, 22.0), choice_id(SaveChoice::Cancel));
    let save = if info.has_file { "Save" } else { "Save as..." };
    ui.flat_button(save, true, Rect::new(r.max_x() - 12.0 - 96.0, by, 96.0, 22.0), choice_id(SaveChoice::Save));
}

/// The navigator SESSION box's hit (the whole box opens the menu).
pub fn session_box_hit(ui: &mut Ui, r: Rect) {
    ui.hit(r, ControlId { strip: Arc::from(SESSION_STRIP), control: Control::SessionMenu }, HitKind::Tap);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PreviewMixer;

    /// PreviewMixer plus a recorder of the session requests the model receives.
    struct Rec {
        inner: PreviewMixer,
        info: SessionInfo,
        got: Vec<String>,
    }

    impl Rec {
        fn new(dirty: bool, has_file: bool) -> Rec {
            let inner = PreviewMixer::default();
            let info = SessionInfo { dirty, has_file, name: if has_file { "Session".into() } else { "- New Session -".into() }, ..inner.session() };
            Rec { inner, info, got: Vec::new() }
        }
    }

    impl MixerModel for Rec {
        fn session_name(&self) -> String {
            self.info.name.clone()
        }
        fn session(&self) -> SessionInfo {
            self.info.clone()
        }
        fn inputs(&self) -> Vec<crate::model::StripInfo> {
            self.inner.inputs()
        }
        fn auxes(&self) -> Vec<crate::model::StripInfo> {
            self.inner.auxes()
        }
        fn main(&self, s: &crate::model::StripInfo) -> crate::model::MainState {
            self.inner.main(s)
        }
        fn preamp(&self, s: &crate::model::StripInfo) -> Option<crate::model::PreampState> {
            self.inner.preamp(s)
        }
        fn inserts(&self, s: &crate::model::StripInfo) -> Vec<crate::model::InsertState> {
            self.inner.inserts(s)
        }
        fn unison(&self, s: &crate::model::StripInfo) -> crate::model::InsertState {
            self.inner.unison(s)
        }
        fn sends(&self, s: &crate::model::StripInfo) -> Vec<crate::model::SendState> {
            self.inner.sends(s)
        }
        fn records_pre_effects(&self, s: &crate::model::StripInfo) -> bool {
            self.inner.records_pre_effects(s)
        }
        fn meters(&self, s: &crate::model::StripInfo) -> crate::model::MeterFrame {
            self.inner.meters(s)
        }
        fn monitor(&self) -> crate::model::MonitorState {
            self.inner.monitor()
        }
        fn monitor_meters(&self) -> crate::model::MeterFrame {
            self.inner.monitor_meters()
        }
        fn apply(&mut self, action: &Action) {
            if let Control::Session(op) = action.id().control {
                self.got.push(format!("{op:?}"));
            }
        }
        fn set_text(&mut self, id: &ControlId, text: &str) {
            self.got.push(format!("{:?}={text}", id.control));
        }
    }

    fn tap(v: &mut ViewState, m: &mut Rec, control: Control) {
        v.dispatch(Action::Tap(ControlId { strip: SESSION_STRIP.into(), control }), m);
    }

    #[test]
    fn menu_lists_like_console() {
        let info = PreviewMixer::default().session();
        let items = session_menu_items(&info);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(&labels[..5], ["New", "Open...", "Save", "Save As...", "Session"]);
        assert!(items[4].separator && items[4].checked);
        assert_eq!(labels.last(), Some(&"[Sep 18, 2026 05:14:34 AM] - - New Session -"));
        assert!(items.last().unwrap().separator);
    }

    #[test]
    fn clean_session_loads_at_once() {
        let mut m = Rec::new(false, true);
        let mut v = ViewState::default();
        tap(&mut v, &mut m, Control::SessionMenu);
        assert_eq!(v.popover.as_ref().map(|p| &p.kind), Some(&Popover::Session));
        tap(&mut v, &mut m, Control::Session(SessionOp::Load(1)));
        assert!(v.popover.is_none());
        assert_eq!(m.got, ["Load(1)"]);
    }

    #[test]
    fn dirty_session_asks_first() {
        let mut m = Rec::new(true, true);
        let mut v = ViewState::default();
        tap(&mut v, &mut m, Control::SessionMenu);
        tap(&mut v, &mut m, Control::Session(SessionOp::New));
        assert_eq!(v.popover.as_ref().map(|p| &p.kind), Some(&Popover::SaveChanges { then: SessionOp::New }));
        assert!(m.got.is_empty());
        tap(&mut v, &mut m, Control::SessionSaveChoice(SaveChoice::Cancel));
        assert!(v.popover.is_none() && m.got.is_empty());
        // Save, then the new session.
        tap(&mut v, &mut m, Control::Session(SessionOp::Load(2)));
        tap(&mut v, &mut m, Control::SessionSaveChoice(SaveChoice::Save));
        assert_eq!(m.got, ["Save", "Load(2)"]);
        // Don't Save.
        tap(&mut v, &mut m, Control::Session(SessionOp::Restore(0)));
        tap(&mut v, &mut m, Control::SessionSaveChoice(SaveChoice::DontSave));
        assert_eq!(m.got, ["Save", "Load(2)", "Restore(0)"]);
    }

    #[test]
    fn untitled_save_asks_for_a_name() {
        let mut m = Rec::new(true, false);
        let mut v = ViewState::default();
        tap(&mut v, &mut m, Control::Session(SessionOp::Save));
        assert_eq!(v.popover.as_ref().map(|p| &p.kind), Some(&Popover::SessionSaveAs));
        v.key_text("Tracking");
        v.key_enter(&mut m);
        assert_eq!(m.got, ["SessionSaveAsName=Tracking"]);
        // Save from the question on an untitled session: name first, then the pending op.
        tap(&mut v, &mut m, Control::Session(SessionOp::New));
        tap(&mut v, &mut m, Control::SessionSaveChoice(SaveChoice::Save));
        assert!(matches!(v.text_edit, Some(TextEdit { target: TextTarget::SessionSaveAs { then: Some(SessionOp::New) }, .. })));
        v.key_text("Mix");
        tap(&mut v, &mut m, Control::TextCommit);
        assert_eq!(m.got, ["SessionSaveAsName=Tracking", "SessionSaveAsName=Mix", "New"]);
    }

    #[test]
    fn open_takes_an_engine_path() {
        let mut m = Rec::new(false, true);
        let mut v = ViewState::default();
        tap(&mut v, &mut m, Control::Session(SessionOp::Open));
        assert_eq!(v.popover.as_ref().map(|p| &p.kind), Some(&Popover::SessionOpen));
        v.key_text("/Users/me/Live.uadmix");
        v.key_enter(&mut m);
        assert_eq!(m.got, ["SessionOpenPath=/Users/me/Live.uadmix"]);
        // An empty field sends nothing.
        tap(&mut v, &mut m, Control::Session(SessionOp::SaveAs));
        assert_eq!(v.text_edit.as_ref().map(|t| t.text.as_str()), Some("Session"));
        v.key_escape();
        assert!(v.popover.is_none());
    }
}
