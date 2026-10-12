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
    /// Delete a file of [`SessionInfo::files`] (panel row menu).
    Delete(u16),
    /// Opens the name field for a file (model: [`Control::SessionRenameTo`] via `set_text`).
    Rename(u16),
    /// Opens the folder name field (model: [`Control::SessionNewFolderName`] via `set_text`).
    NewFolder,
    /// A folder row's Delete (`DeleteFolder {folder, type: session}`), by panel row index.
    FolderDelete(u16),
    /// Opens a folder row's name field (model: [`Control::SessionFolderRenameTo`]).
    FolderRename(u16),
    /// Opens the name field for a folder inside a folder row (model: [`Control::SessionNewFolderIn`]).
    NewFolderIn(u16),
    /// Show In Finder / Show In File Explorer: reveals a panel row's file or folder (or, with
    /// [`REVEAL_SESSIONS_FOLDER`], the Sessions folder) on this computer. Console does this
    /// client-side (`juce::File::revealToUser`, `Rack_PresetsView::ShowPopupMenu` 0x181229470,
    /// case 5), so it needs the engine's Sessions folder to be local
    /// ([`MixerModel::local_sessions_dir`]).
    Reveal(u16),
}

/// [`SessionOp::Reveal`] of the Sessions folder itself (the ••• menu).
pub const REVEAL_SESSIONS_FOLDER: u16 = u16::MAX;

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
    /// The engine's Sessions folder as a path on this computer, when the engine runs here
    /// (Show In Finder needs it; Console reveals files client-side).
    pub local_dir: Option<String>,
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

/// The CURRENT SESSION panel's view state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionPanel {
    pub open: bool,
    /// The selected tree row (panel row index).
    pub selected: Option<u16>,
}

impl ViewState {
    /// The SESSION box: Console 1.3.1 shows the CURRENT SESSION panel in the browser column
    /// (capture sess-panel; 1.2.9 opened a popup menu); a second click closes it.
    pub fn open_session_menu(&mut self, _anchor: ControlId) {
        self.popover = None;
        self.session_panel.open ^= true;
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
            SessionOp::Rename(i) => {
                let name = info.files.get(i as usize).map(|f| f.path.rsplit('/').next().unwrap_or(&f.path).to_owned()).unwrap_or_default();
                self.text_edit = Some(TextEdit { target: TextTarget::SessionRename(i), text: name });
                self.popover = Some(PopoverState { kind: Popover::SessionRename, anchor });
                Vec::new()
            }
            SessionOp::NewFolder => {
                self.text_edit = Some(TextEdit { target: TextTarget::SessionNewFolder, text: String::new() });
                self.popover = Some(PopoverState { kind: Popover::SessionNewFolder, anchor });
                Vec::new()
            }
            SessionOp::NewFolderIn(row) => {
                self.text_edit = Some(TextEdit { target: TextTarget::SessionNewFolderIn(row), text: String::new() });
                self.popover = Some(PopoverState { kind: Popover::SessionNewFolder, anchor });
                Vec::new()
            }
            SessionOp::FolderRename(row) => {
                let name = panel_rows(info).get(row as usize).map(|r| r.label.clone()).unwrap_or_default();
                self.text_edit = Some(TextEdit { target: TextTarget::SessionFolderRename(row), text: name });
                self.popover = Some(PopoverState { kind: Popover::SessionRename, anchor });
                Vec::new()
            }
            SessionOp::Reveal(row) => {
                if let Some(url) = reveal_url(info, row) {
                    self.open_urls.push(url);
                }
                Vec::new()
            }
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
            TextTarget::SessionRename(i) => vec![set(Control::SessionRenameTo(*i))],
            TextTarget::SessionNewFolder => vec![set(Control::SessionNewFolderName)],
            TextTarget::SessionFolderRename(row) => vec![set(Control::SessionFolderRenameTo(*row))],
            TextTarget::SessionNewFolderIn(row) => vec![set(Control::SessionNewFolderIn(*row))],
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

/// The `file://` URL Show In Finder opens for CURRENT SESSION row `row`: the session file
/// (`.uadmix`) or folder under the local Sessions folder, or the folder itself for the root row
/// and [`REVEAL_SESSIONS_FOLDER`]. `None` when the engine's Sessions folder isn't on this computer.
pub fn reveal_url(info: &SessionInfo, row: u16) -> Option<String> {
    let dir = info.local_dir.as_deref()?.trim_end_matches('/').to_owned();
    let path = if row == REVEAL_SESSIONS_FOLDER {
        dir
    } else {
        match &panel_rows(info).get(row as usize)?.kind {
            PanelRowKind::Folder { path, root } if *root || path.is_empty() => dir,
            PanelRowKind::Folder { path, .. } => format!("{dir}/{path}"),
            PanelRowKind::File(i) => format!("{dir}/{}{}", info.files.get(*i as usize)?.path, ua_session_ext()),
        }
    };
    Some(file_url(&path))
}

/// Session files' extension (the engine's `.uadmix`).
fn ua_session_ext() -> &'static str {
    ".uadmix"
}

/// `file://` URL of an absolute path, percent-encoding what a URL can't carry.
pub fn file_url(path: &str) -> String {
    let mut out = String::from("file://");
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
            // Rows are 11-pt bold Helvetica after a 22-pt tick column.
            let widest = items.iter().map(|i| crate::window::helvetica_bold_width(&i.label, 11.0)).fold(0.0, f32::max);
            let width = (widest + 40.0).clamp(140.0, 420.0).ceil();
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

// MARK: CURRENT SESSION panel

/// A row of the CURRENT SESSION tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelRowKind {
    /// The Sessions folder ("User", the root) or a sub-folder (its path).
    Folder { path: String, root: bool },
    /// A file of [`SessionInfo::files`].
    File(u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelRow {
    pub label: String,
    pub kind: PanelRowKind,
    /// Tree depth (the root folder 0).
    pub depth: u8,
}

/// The tree: the Sessions folder as "User" (capture sess-panel: `User` › `Session` for the
/// engine's root file `Session`), its files, then each sub-folder and its files.
pub fn panel_rows(info: &SessionInfo) -> Vec<PanelRow> {
    let mut rows = vec![PanelRow { label: "User".into(), kind: PanelRowKind::Folder { path: String::new(), root: true }, depth: 0 }];
    let mut folders: Vec<&str> = Vec::new();
    for (i, f) in info.files.iter().enumerate() {
        match f.path.rsplit_once('/') {
            None => rows.push(PanelRow { label: f.path.clone(), kind: PanelRowKind::File(i as u16), depth: 1 }),
            Some((dir, _)) if !folders.contains(&dir) => folders.push(dir),
            _ => {}
        }
    }
    for dir in folders {
        rows.push(PanelRow { label: dir.rsplit('/').next().unwrap_or(dir).into(), kind: PanelRowKind::Folder { path: dir.into(), root: false }, depth: 1 });
        for (i, f) in info.files.iter().enumerate() {
            if let Some((d, name)) = f.path.rsplit_once('/') {
                if d == dir {
                    rows.push(PanelRow { label: name.into(), kind: PanelRowKind::File(i as u16), depth: 2 });
                }
            }
        }
    }
    rows
}

// Geometry and colours measured on capture sess-panel (2 px per pt), relative to the column.
// Colours read from the drive captures converted to sRGB (their ICC profile, ASUS PG32).
const PANEL_BG: Rgba = Rgba::rgb8(115, 123, 128);
const PANEL_TITLE_H: f32 = 34.0;
const SEARCH: Rect = Rect { x: 4.0, y: 34.0, w: 337.0, h: 28.0 };
const SEARCH_EDGE: Rgba = Rgba::rgb8(18, 20, 20);
const SEARCH_FILL: Rgba = Rgba::rgb8(35, 38, 42);
const BOX: Rect = Rect { x: 5.0, y: 66.0, w: 335.0, h: 66.0 };
const BOX_EDGE: Rgba = Rgba::rgb8(37, 40, 43);
const BOX_FILL: Rgba = Rgba::rgb8(48, 51, 54);
const CARD: Rect = Rect { x: 10.0, y: 71.0, w: 325.0, h: 56.0 };
const CARD_EDGE: Rgba = Rgba::rgb8(81, 84, 86);
const CARD_FILL: Rgba = Rgba::rgb8(68, 71, 73);
/// The ••• button: its menu opens at (10, 117) in the window (sess-dots), i.e. under a
/// component at the card's left edge, 20 pt high.
const DOTS: Rect = Rect { x: 10.0, y: 71.0, w: 30.0, h: 20.0 };
const BUTTON_Y: f32 = 99.0;
const BUTTON_H: f32 = 23.0;
const BUTTON_W: f32 = 102.0;
const BUTTON_X: [f32; 3] = [15.0, 121.0, 227.0];
const BUTTON_FILL: Rgba = Rgba::rgb8(90, 92, 94);
const BUTTON_EDGE: Rgba = Rgba::rgb8(108, 110, 112);
const TREE_Y: f32 = 132.0;
const TREE_ROW_H: f32 = 32.0;
const FOLDER_FILL: Rgba = Rgba::rgb8(67, 80, 86);
const ROW_SELECTED: Rgba = Rgba::rgb8(92, 92, 92);
const TREE_LINE: Rgba = Rgba::rgb8(78, 80, 80);
/// A tree row's menu opens under the row at x 33 in the column (sess-row, browser-cat2).
pub const ROW_MENU_X: f32 = 33.0;

fn panel_id(control: Control) -> ControlId {
    ControlId { strip: Arc::from(SESSION_STRIP), control }
}

/// The CURRENT SESSION panel over the browser column `r` (`Rack_MixerPresets` view; capture
/// sess-panel): title and close, search, the open session's card (•••, name, NEW / OPEN... /
/// SAVE) and the Sessions tree, the open session selected.
pub fn draw_session_panel(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let info = model.session();
    let rows = panel_rows(&info);
    ui.fill(r, PANEL_BG);
    ui.at(r.x, r.y, |ui| {
        let w = r.w;
        let title = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(226, 239, 250));
        ui.text("CURRENT SESSION", title, Rect::new(0.0, 2.5, w, PANEL_TITLE_H - 1.0), Align::Center);
        let close = Rect::new(w - 33.0, 5.0, 24.0, 24.0);
        if let Some(x) = ui.skin("views", "Browser-Modal-Close-White").and_then(|s| s.image("image")) {
            let x = x.to_string();
            ui.image(&x, Rect::new(w - 27.0, 12.0, 13.0, 13.0), None, None, 1.0);
        }
        ui.hit(close, panel_id(Control::SessionMenu), HitKind::Tap);

        ui.fill(SEARCH, SEARCH_EDGE);
        ui.fill(SEARCH.inset(1.0, 1.0, 1.0, 1.0), SEARCH_FILL);
        if let Some(icon) = ui.skin("views", "Browser-Search-Icon").and_then(|s| s.image("image")) {
            let icon = icon.to_string();
            ui.image(&icon, Rect::new(SEARCH.x + 9.0, SEARCH.y + 6.0, 16.0, 16.0), None, None, 1.0);
        }

        ui.fill(BOX, BOX_EDGE);
        ui.fill(BOX.inset(1.0, 1.0, 1.0, 1.0), BOX_FILL);
        ui.fill(CARD, CARD_EDGE);
        ui.fill(CARD.inset(1.0, 1.0, 1.0, 1.0), CARD_FILL);
        // Three 4-pt dots 5 pt apart from x 18 (sess-panel).
        let dots = Ui::plain_style("Helvetica", 14.0, true, Rgba::rgb8(216, 217, 217));
        for k in 0..3 {
            ui.text("•", dots.clone(), Rect::new(16.0 + k as f32 * 5.0, DOTS.y + 2.0, 8.0, DOTS.h), Align::Center);
        }
        ui.hit(DOTS, panel_id(Control::SessionPanelDots), HitKind::Tap);
        let name = if info.progress.is_empty() { info.name.clone() } else { info.progress.clone() };
        let name_style = Ui::plain_style("Helvetica", 12.0, true, Rgba::rgb8(227, 228, 228));
        ui.text(&name, name_style, Rect::new(CARD.x + 34.0, CARD.y, CARD.w - 68.0, 26.0), Align::Center);
        let label = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(230, 231, 231));
        for (x, (text, op)) in BUTTON_X.iter().zip([("NEW", SessionOp::New), ("OPEN...", SessionOp::Open), ("SAVE", SessionOp::Save)]) {
            let b = Rect::new(*x, BUTTON_Y, BUTTON_W, BUTTON_H);
            ui.fill(b, BUTTON_EDGE);
            ui.fill(b.inset(1.0, 1.0, 1.0, 1.0), BUTTON_FILL);
            ui.text(text, label.clone(), b, Align::Center);
            ui.hit(b, sid(op), HitKind::Tap);
        }

        let tree = Rect::new(BOX.x, TREE_Y, BOX.w, r.h - TREE_Y);
        ui.fill(tree, BOX_EDGE);
        ui.fill(tree.inset(1.0, 0.0, 1.0, 0.0), BOX_FILL);
        ui.clip(Some(tree));
        let folder_text = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(147, 153, 157));
        let file_text = Ui::plain_style("Helvetica", 11.0, true, Rgba::rgb8(238, 238, 238));
        let current = info.files.iter().position(|f| f.path == info.current).map(|i| i as u16);
        for (k, row) in rows.iter().enumerate() {
            let y = TREE_Y + k as f32 * TREE_ROW_H;
            let rr = Rect::new(tree.x, y, tree.w, TREE_ROW_H);
            let indent = row.depth as f32 * 24.0;
            match &row.kind {
                PanelRowKind::Folder { .. } => {
                    ui.fill(rr, FOLDER_FILL);
                    // The open folder's reveal chevron (`Browser-Reveal-Toggle`), as the
                    // browser's preset tree draws it.
                    if let Some(img) = ui.skin("views", "Browser-Reveal-Toggle").and_then(|s| s.image("image-on")) {
                        let img = img.to_string();
                        ui.image(&img, Rect::new(rr.x + 8.0 + indent, y + 10.0, 12.0, 12.0), None, None, 1.0);
                    }
                    ui.text(&row.label, folder_text.clone(), Rect::new(rr.x + 33.0 + indent, y + 1.25, rr.w - 38.0 - indent, 30.0), Align::Left);
                }
                PanelRowKind::File(i) => {
                    let sel = view.session_panel.selected.map(|s| s as usize == k).unwrap_or(current == Some(*i));
                    if sel {
                        ui.fill(rr, ROW_SELECTED);
                    }
                    // Tree connector: down from the folder above, across to the label.
                    let lx = rr.x + 14.0 + (row.depth as f32 - 1.0) * 24.0;
                    ui.fill(Rect::new(lx, y - (TREE_ROW_H / 2.0) * 0.0, 1.0, TREE_ROW_H / 2.0), TREE_LINE);
                    ui.fill(Rect::new(lx, y + TREE_ROW_H / 2.0, 14.0, 1.0), TREE_LINE);
                    ui.text(&row.label, file_text.clone(), Rect::new(rr.x + 33.0 + indent - 24.0, y + 1.25, rr.w - 38.0, 30.0), Align::Left);
                }
            }
            ui.hit(rr, panel_id(Control::SessionPanelRow(k as u16)), HitKind::Tap);
        }
        ui.clip(None);
    });
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
        // The SESSION box toggles the CURRENT SESSION panel (Console 1.3.1, capture sess-panel).
        tap(&mut v, &mut m, Control::SessionMenu);
        assert!(v.session_panel.open);
        tap(&mut v, &mut m, Control::Session(SessionOp::Load(1)));
        assert!(v.popover.is_none());
        assert_eq!(m.got, ["Load(1)"]);
        tap(&mut v, &mut m, Control::SessionMenu);
        assert!(!v.session_panel.open);
    }

    #[test]
    fn panel_tree_and_row_menus() {
        let info = PreviewMixer::default().session();
        let rows = panel_rows(&info);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        // User › Session, Tracking; Mix › Rough Mix, Vocal Comp.
        assert_eq!(labels, ["User", "Session", "Tracking", "Mix", "Rough Mix", "Vocal Comp"]);
        let model = PreviewMixer::default();
        let file = crate::menu::session_row_menu(&model, 1);
        let items: Vec<String> = file.entries.iter().map(|e| match e {
            crate::menu::PopupEntry::Item(i) => format!("{}{}", i.label, if i.enabled { "" } else { " (off)" }),
            crate::menu::PopupEntry::Header(h) => format!("[{h}]"),
            crate::menu::PopupEntry::Separator => "-".into(),
            crate::menu::PopupEntry::Submenu { label, .. } => label.clone(),
        }).collect();
        // sess-row: the open session's Load is disabled. The preview has no local Sessions
        // folder, so Show In Finder is off (it reveals files on this computer only).
        assert_eq!(items, ["[Session]", "Load 'Session' (off)", "-", "Delete", "Rename", "Show In Finder (off)"]);
        let root = crate::menu::session_row_menu(&model, 0);
        assert!(root.find("Delete").is_some_and(|i| !i.enabled), "the root folder cannot be deleted (browser-cat2)");
        // Rename opens the name field; its commit renames inside the folder.
        let mut m = Rec::new(false, true);
        let mut v = ViewState::default();
        tap(&mut v, &mut m, Control::Session(SessionOp::Rename(1)));
        assert_eq!(v.popover.as_ref().map(|p| &p.kind), Some(&Popover::SessionRename));
        tap(&mut v, &mut m, Control::Session(SessionOp::Delete(1)));
        assert_eq!(m.got.last().map(String::as_str), Some("Delete(1)"));
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
