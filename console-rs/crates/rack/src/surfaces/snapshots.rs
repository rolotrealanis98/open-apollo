//! The Snapshots section (`Rack_SnapshotsSection`; Windows Rack 1.2.9 0x180e9dbf0, Mac Rack
//! 1.3.1 ctor 0xbf2cf8, `resized` 0xbf4fd0, `buttonClicked` 0xbf52ec): a window column of
//! [`SNAPSHOTS_W`] after the PLUG-IN SCENES column (`Rack_WindowLayouts::Resized_SnapshotsSection`
//! 0x17e13a0), with the FILE buttons, the FOLDERS / SNAPSHOTS lists, CURRENT / PRIMED, the
//! arrows and RECALL. Console 1.3.1 gates it behind a feature flag (no View entry on an Apollo),
//! so View › Show Snapshots appears here only with the `show_snapshots` preference.
//!
//! What the buttons send (Mac Rack 1.3.1, as [`SnapshotRequest`]s the model turns into the
//! engine's snapshot commands):
//! - SAVE: `SaveAsSnapshot {snapshotName: current}`; SAVE AS...: a name field, then the same
//!   (asking first when the name exists); NEW: `SaveAsSnapshot {snapshotName:
//!   "<folder>/NNN - Untitled", isNew: true}`, then the Snapshot is set to it.
//! - RENAME / DELETE / MOVE act on the focused list's selection. Folders have no commands of
//!   their own: `MoveSnapshot {origName, newName, shuffle}` and `DeleteSnapshot {snapshotName}`
//!   take the folder's name.
//! - IMPORT ALL / EXPORT ALL: `ImportSnapshots {volume, folder, append}` / `ExportSnapshots
//!   {volume, folder}`.
//! - Selecting a SNAPSHOTS row primes it (`set /PrimedSnapshot`), a FOLDERS row clears the prime;
//!   the arrows step the selection; RECALL sets `/Snapshot` to the primed one.
//!
//! Differences from Console (ponytail): IMPORT / EXPORT take a folder name on the engine's first
//! volume instead of Console's volume chooser, and IMPORT appends (Console asks Append or
//! Replace); MOVE takes the destination `[folder/]name` instead of Console's index dialog, and
//! offers Overwrite but not Shuffle; RECALL doesn't ask about a modified snapshot; the lists
//! don't scroll.

use houston::{Rect, Rgba};

use crate::Action;
use crate::draw::Align;
use crate::input::{Control, ControlId, HitKind};
use crate::model::MixerModel;
use crate::surfaces::view::{Effect, Popover, PopoverState, TextEdit, TextTarget, ViewState};
use crate::ui::Ui;

/// The section's width (`Resized_SnapshotsSection`: 0x21c).
pub const SNAPSHOTS_W: f32 = 540.0;
/// Strip id of the section's controls.
pub const SNAPSHOTS_STRIP: &str = "snapshots";
/// `Rack_ListBox` row height (0x1e4b08).
pub const ROW_H: f32 = 40.0;
/// The folder NEW and SAVE AS... use when none is selected.
pub const DEFAULT_FOLDER: &str = "000 - Untitled";

/// A snapshot folder and its snapshot names (without the folder). Snapshots outside any folder
/// sit in a folder named `""`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotFolder {
    pub name: String,
    pub snapshots: Vec<String>,
}

/// What the section shows ([`MixerModel::snapshots`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// `/Snapshot`: `[folder/]name` of the recalled snapshot.
    pub current: String,
    /// `/PrimedSnapshot`.
    pub primed: String,
    pub folders: Vec<SnapshotFolder>,
    /// The engine's IMPORT / EXPORT volumes (`/Snapshot` `volumes`).
    pub volumes: Vec<String>,
}

impl SnapshotInfo {
    /// `folder/name` (`name` alone outside a folder).
    pub fn full(folder: &str, name: &str) -> String {
        if folder.is_empty() { name.to_owned() } else { format!("{folder}/{name}") }
    }

    /// `(folder, name)` of a `[folder/]name`.
    pub fn split(full: &str) -> (&str, &str) {
        full.rsplit_once('/').unwrap_or(("", full))
    }

    pub fn contains(&self, full: &str) -> bool {
        let (f, n) = Self::split(full);
        self.folders.iter().any(|x| x.name == f && x.snapshots.iter().any(|s| s == n))
    }

    pub fn folder_index(&self, name: &str) -> Option<usize> {
        self.folders.iter().position(|f| f.name == name)
    }

    /// NEW's name in `folder`: `NNN - Untitled`, one past the highest `NNN - ` prefix there
    /// (Console numbers snapshots and folders with that prefix).
    pub fn next_untitled(&self, folder: &str) -> String {
        let used = self.folder_index(folder).map(|i| &self.folders[i].snapshots[..]).unwrap_or_default();
        let next = used.iter().filter_map(|n| n.split_once(" - ")?.0.parse::<u32>().ok()).max().map_or(1, |n| n + 1);
        format!("{next:03} - Untitled")
    }

    /// A request's effect on this list, as the engine applies it (offline models).
    pub fn apply(&mut self, r: &SnapshotRequest) {
        match r {
            SnapshotRequest::Recall(n) => self.current = n.clone(),
            SnapshotRequest::Prime(n) => self.primed = n.clone(),
            SnapshotRequest::Save { name, .. } => {
                if !self.contains(name) {
                    let (f, n) = Self::split(name);
                    match self.folder_index(f) {
                        Some(i) => self.folders[i].snapshots.push(n.to_owned()),
                        None => self.folders.push(SnapshotFolder { name: f.to_owned(), snapshots: vec![n.to_owned()] }),
                    }
                }
            }
            SnapshotRequest::Delete(name) => {
                if let Some(i) = self.folder_index(name).filter(|_| !self.contains(name)) {
                    self.folders.remove(i);
                } else {
                    let (f, n) = Self::split(name);
                    if let Some(i) = self.folder_index(f) {
                        self.folders[i].snapshots.retain(|s| s != n);
                    }
                }
            }
            SnapshotRequest::Move { from, to, .. } => {
                if let Some(i) = self.folder_index(from).filter(|_| !self.contains(from)) {
                    self.folders[i].name = to.clone();
                } else if self.contains(from) {
                    self.apply(&SnapshotRequest::Delete(to.clone()));
                    self.apply(&SnapshotRequest::Delete(from.clone()));
                    self.apply(&SnapshotRequest::Save { name: to.clone(), is_new: false });
                    for p in [&mut self.current, &mut self.primed] {
                        if p == from {
                            *p = to.clone();
                        }
                    }
                }
            }
            SnapshotRequest::Export { .. } | SnapshotRequest::Import { .. } => {}
        }
    }
}

/// [model] One snapshot request (`ua_engine::SnapshotCommand`'s shapes), carried by
/// [`Control::SnapshotRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotRequest {
    Recall(String),
    Prime(String),
    Save {
        name: String,
        is_new: bool,
    },
    /// A snapshot (`folder/name`) or a whole folder (its name).
    Delete(String),
    /// A rename or move of a snapshot or a folder.
    Move {
        from: String,
        to: String,
        shuffle: bool,
    },
    Export {
        volume: String,
        folder: String,
    },
    Import {
        volume: String,
        folder: String,
        append: bool,
    },
}

/// [view] The section's buttons and list rows ([`Control::Snapshots`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotOp {
    Save,
    SaveAs,
    New,
    Rename,
    Delete,
    Move,
    ImportAll,
    ExportAll,
    Recall,
    /// The ▲ / ▼ arrows (`true` = up).
    Step(bool),
    /// A FOLDERS row / a SNAPSHOTS row of the selected folder.
    Folder(u16),
    Snapshot(u16),
}

/// Which name field is open ([`TextTarget::Snapshot`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotField {
    SaveAs,
    Rename,
    Move,
    Import,
    Export,
}

/// The section's view state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapshotsView {
    /// The FOLDERS row picked by hand (else the primed / current snapshot's folder, else the
    /// first).
    pub folder: Option<u16>,
    /// RENAME / DELETE / MOVE act on the FOLDERS list (else on the SNAPSHOTS list).
    pub folders_focused: bool,
}

pub(crate) fn sid(op: SnapshotOp) -> ControlId {
    ControlId { strip: SNAPSHOTS_STRIP.into(), control: Control::Snapshots(op) }
}

pub fn request_id(r: SnapshotRequest) -> ControlId {
    ControlId { strip: SNAPSHOTS_STRIP.into(), control: Control::SnapshotRequest(r) }
}

fn req(r: SnapshotRequest) -> Effect {
    Effect::Model(Action::Tap(request_id(r)))
}

impl ViewState {
    /// The FOLDERS row shown selected.
    pub fn snapshot_folder(&self, info: &SnapshotInfo) -> Option<usize> {
        let of = |full: &str| info.folder_index(SnapshotInfo::split(full).0).filter(|_| info.contains(full));
        self.snapshots
            .folder
            .map(usize::from)
            .filter(|&i| i < info.folders.len())
            .or_else(|| of(&info.primed))
            .or_else(|| of(&info.current))
            .or_else(|| (!info.folders.is_empty()).then_some(0))
    }

    fn snapshot_field(
        &mut self,
        title: &str,
        field: SnapshotField,
        from: String,
        folder: bool,
        text: String,
        anchor: ControlId,
    ) {
        self.text_edit = Some(TextEdit { target: TextTarget::Snapshot { field, from, folder }, text });
        self.popover = Some(PopoverState { kind: Popover::SnapshotText { title: title.into() }, anchor });
    }

    fn snapshot_confirm(&mut self, title: &str, message: String, ok: SnapshotRequest, anchor: ControlId) {
        let ok = request_id(ok);
        self.popover = Some(PopoverState {
            kind: Popover::Confirm { title: title.into(), message, ok, ok_label: "OK".into() },
            anchor,
        });
    }

    /// A button or row of the section.
    pub(crate) fn route_snapshots(&mut self, op: SnapshotOp, model: &dyn MixerModel) -> Vec<Effect> {
        use SnapshotRequest as R;
        self.popover = None;
        let info = model.snapshots();
        let anchor = sid(op);
        let fsel = self.snapshot_folder(&info);
        let folder = fsel.map(|i| info.folders[i].name.clone());
        let primed = Some(info.primed.clone()).filter(|p| info.contains(p));
        // The focused list's selection: (name, is a folder).
        let target = if self.snapshots.folders_focused {
            folder.clone().map(|f| (f, true))
        } else {
            primed.clone().map(|p| (p, false))
        };
        match op {
            SnapshotOp::Folder(i) => {
                self.snapshots.folders_focused = true;
                let changed = fsel != Some(i as usize);
                self.snapshots.folder = Some(i);
                // A folder row change clears the prime (0xbf7c88).
                if changed && !info.primed.is_empty() { vec![req(R::Prime(String::new()))] } else { Vec::new() }
            }
            SnapshotOp::Snapshot(i) => {
                self.snapshots.folders_focused = false;
                let Some(f) = fsel.map(|k| &info.folders[k]) else { return Vec::new() };
                f.snapshots
                    .get(i as usize)
                    .map(|n| vec![req(R::Prime(SnapshotInfo::full(&f.name, n)))])
                    .unwrap_or_default()
            }
            SnapshotOp::Step(up) => self.step_snapshot(&info, fsel, up),
            SnapshotOp::Save if !info.current.is_empty() => {
                vec![req(R::Save { name: info.current.clone(), is_new: false })]
            }
            SnapshotOp::SaveAs => {
                let name = SnapshotInfo::split(&info.current).1.to_owned();
                self.snapshot_field(
                    "SAVE SNAPSHOT AS",
                    SnapshotField::SaveAs,
                    folder.unwrap_or_else(|| DEFAULT_FOLDER.into()),
                    false,
                    name,
                    anchor,
                );
                Vec::new()
            }
            SnapshotOp::New => {
                let f = folder.unwrap_or_else(|| DEFAULT_FOLDER.into());
                let name = SnapshotInfo::full(&f, &info.next_untitled(&f));
                vec![req(R::Save { name: name.clone(), is_new: true }), req(R::Recall(name))]
            }
            SnapshotOp::Rename => {
                if let Some((from, is_folder)) = target {
                    let title = if is_folder { "RENAME SNAPSHOT FOLDER" } else { "RENAME SNAPSHOT" };
                    let text = if is_folder { from.clone() } else { SnapshotInfo::split(&from).1.to_owned() };
                    self.snapshot_field(title, SnapshotField::Rename, from, is_folder, text, anchor);
                }
                Vec::new()
            }
            SnapshotOp::Move => {
                if let Some((from, is_folder)) = target {
                    let title = if is_folder { "MOVE SNAPSHOT FOLDER" } else { "MOVE SNAPSHOT" };
                    self.snapshot_field(title, SnapshotField::Move, from.clone(), is_folder, from, anchor);
                }
                Vec::new()
            }
            SnapshotOp::Delete => {
                if let Some((name, is_folder)) = target {
                    let (title, kind) =
                        if is_folder { ("Delete Snapshot Folder", "folder") } else { ("Delete Snapshot", "snapshot") };
                    let shown = if is_folder { name.clone() } else { SnapshotInfo::split(&name).1.to_owned() };
                    self.snapshot_confirm(
                        title,
                        format!("Are you sure you want to delete the \"{shown}\" {kind}?"),
                        R::Delete(name),
                        anchor,
                    );
                }
                Vec::new()
            }
            SnapshotOp::ImportAll => {
                self.snapshot_field(
                    "IMPORT ALL SNAPSHOTS",
                    SnapshotField::Import,
                    String::new(),
                    false,
                    String::new(),
                    anchor,
                );
                Vec::new()
            }
            SnapshotOp::ExportAll if !info.folders.is_empty() => {
                self.snapshot_field(
                    "EXPORT ALL SNAPSHOTS",
                    SnapshotField::Export,
                    String::new(),
                    false,
                    String::new(),
                    anchor,
                );
                Vec::new()
            }
            SnapshotOp::Recall => primed.map(|p| vec![req(R::Recall(p))]).unwrap_or_default(),
            SnapshotOp::Save | SnapshotOp::ExportAll => Vec::new(),
        }
    }

    /// ▲ / ▼ (0xbf6524 / 0xbf6400): the previous / next SNAPSHOTS row, wrapping into the
    /// neighbouring folder's first row; the new row is primed.
    fn step_snapshot(&mut self, info: &SnapshotInfo, fsel: Option<usize>, up: bool) -> Vec<Effect> {
        let Some(fi) = fsel else { return Vec::new() };
        self.snapshots.folders_focused = false;
        let f = &info.folders[fi];
        let row = f.snapshots.iter().position(|n| SnapshotInfo::full(&f.name, n) == info.primed);
        let next = match (row, up) {
            (None, _) => Some(0),
            (Some(r), true) => r.checked_sub(1),
            (Some(r), false) => Some(r + 1).filter(|&r| r < f.snapshots.len()),
        };
        let (fi, ri) = match next {
            Some(r) if r < f.snapshots.len() => (fi, r),
            _ => {
                let n = info.folders.len();
                let to = if up { (fi + n - 1) % n } else { (fi + 1) % n };
                self.snapshots.folder = Some(to as u16);
                (to, 0)
            }
        };
        let f = &info.folders[fi];
        f.snapshots
            .get(ri)
            .map(|n| vec![req(SnapshotRequest::Prime(SnapshotInfo::full(&f.name, n)))])
            .unwrap_or_default()
    }

    /// A name field's commit.
    pub(crate) fn commit_snapshot_text(
        &mut self,
        field: SnapshotField,
        from: String,
        folder: bool,
        text: String,
        model: &dyn MixerModel,
    ) -> Vec<Effect> {
        use SnapshotRequest as R;
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Vec::new();
        }
        let info = model.snapshots();
        let anchor = sid(SnapshotOp::SaveAs);
        let volume = info.volumes.first().cloned().unwrap_or_default();
        match field {
            SnapshotField::SaveAs => {
                let name = if text.contains('/') { text } else { SnapshotInfo::full(&from, &text) };
                let save = R::Save { name: name.clone(), is_new: false };
                if info.contains(&name) {
                    let shown = SnapshotInfo::split(&name).1;
                    self.snapshot_confirm(
                        "Snapshot Exists",
                        format!("The snapshot \"{shown}\" already exists.  Would you like to overwrite it?"),
                        save,
                        anchor,
                    );
                    return Vec::new();
                }
                vec![req(save)]
            }
            SnapshotField::Rename | SnapshotField::Move => {
                let to = if field == SnapshotField::Rename && !folder {
                    SnapshotInfo::full(SnapshotInfo::split(&from).0, &text)
                } else {
                    text
                };
                if to == from {
                    return Vec::new();
                }
                let mv = R::Move { from, to: to.clone(), shuffle: false };
                let taken = if folder { info.folder_index(&to).is_some() } else { info.contains(&to) };
                if taken {
                    let title = if folder { "Folder Exists" } else { "Snapshot Exists" };
                    self.snapshot_confirm(
                        title,
                        format!("\"{to}\" exists at the new location.  Would you like to overwrite it?"),
                        mv,
                        anchor,
                    );
                    return Vec::new();
                }
                // A renamed or moved snapshot stays primed (0xbf8a38, 0xbf9f50).
                let mut out = vec![req(mv)];
                if !folder {
                    out.push(req(R::Prime(to)));
                }
                out
            }
            SnapshotField::Import => vec![req(R::Import { volume, folder: text, append: true })],
            SnapshotField::Export => vec![req(R::Export { volume, folder: text })],
        }
    }
}

/// The section's rects at `r` (`resized` 0xbf4fd0; the lists container 0x1db9f4).
pub struct SnapshotsLayout {
    pub file: [Rect; 3],
    pub edit: [Rect; 3],
    pub transfer: [Rect; 2],
    pub file_label: Rect,
    pub edit_label: Rect,
    pub lists: [Rect; 2],
    pub headers: [Rect; 2],
    pub current: [Rect; 2],
    pub primed: [Rect; 2],
    pub arrows: [Rect; 2],
    pub recall: Rect,
}

impl SnapshotsLayout {
    pub fn new(r: Rect) -> Self {
        let (x, y, w, h) = (r.x, r.y, r.w, r.h);
        // Operations column (0, 0, 80, H): buttons 78 wide.
        let op = |dy: f32| Rect::new(x, y + dy, 78.0, 40.0);
        // Lists container (80, 0, W − 200, H): two columns of (W' − 2) / 2 with a 1-pt divider.
        let lw = w - 200.0;
        let col = ((lw - 2.0) / 2.0).floor();
        let lx = [x + 80.0, x + 80.0 + col + 1.0];
        // Actions column (W − 120, 0, 120, H).
        let ax = x + w - 120.0;
        let field = |dy: f32| Rect::new(ax + 8.0, y + dy, 106.0, 24.0);
        let act = |dy: f32| Rect::new(ax + 2.0, y + dy, 118.0, 40.0);
        SnapshotsLayout {
            file_label: Rect::new(x, y + 4.0, 78.0, 16.0),
            file: [op(20.0), op(60.0), op(100.0)],
            edit_label: Rect::new(x, y + 146.0, 78.0, 16.0),
            edit: [op(162.0), op(202.0), op(242.0)],
            transfer: [op(h - 86.0), op(h - 46.0)],
            headers: lx.map(|cx| Rect::new(cx, y, col, 39.0)),
            lists: lx.map(|cx| Rect::new(cx, y + 40.0, col, (h - 40.0).max(0.0))),
            current: [field(6.0), field(30.0)],
            primed: [field(60.0), field(84.0)],
            arrows: [act(108.0), act(148.0)],
            recall: act(188.0),
        }
    }
}

/// A section button: `Popover-Text-Button` art (3-pt inset; the skin's bevelled black text is
/// drawn white), dimmed and inert when disabled.
fn button(ui: &mut Ui, label: &str, enabled: bool, r: Rect, op: SnapshotOp) {
    let b = r.inset(3.0, 3.0, 3.0, 3.0);
    ui.toggle("Popover-Text-Button", Some(label), false, enabled, b, None, Some(Rgba::WHITE.with_alpha(0.85)));
    if ui.skin("views", "Popover-Text-Button").is_none() {
        ui.flat_button(label, false, b, sid(op));
    } else if enabled {
        ui.hit(b, sid(op), HitKind::Tap);
    }
}

/// Draws the section into `r` (the window column).
pub fn draw_snapshots_section(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, r: Rect) {
    let info = model.snapshots();
    let l = SnapshotsLayout::new(r);
    ui.fill(r, Rgba::rgb8(72, 76, 78));
    ui.fill(Rect::new(r.max_x(), r.y, 1.0, r.h), Rgba::BLACK);
    let label = Ui::plain_style("Helvetica", 10.0, true, Rgba::WHITE.with_alpha(0.6));
    let fsel = view.snapshot_folder(&info);
    let folder = fsel.map(|i| &info.folders[i]);
    let primed_ok = info.contains(&info.primed);
    let has_target = if view.snapshots.folders_focused { folder.is_some() } else { primed_ok };

    // FILE: SAVE / SAVE AS... / NEW; the focused list's RENAME / DELETE / MOVE; IMPORT / EXPORT.
    ui.text("FILE", label.clone(), l.file_label, Align::Center);
    let file = [
        ("SAVE", !info.current.is_empty(), SnapshotOp::Save),
        ("SAVE AS...", true, SnapshotOp::SaveAs),
        ("NEW", true, SnapshotOp::New),
    ];
    for ((text, on, op), b) in file.into_iter().zip(l.file) {
        button(ui, text, on, b, op);
    }
    ui.text(
        if view.snapshots.folders_focused { "FOLDER" } else { "SNAPSHOT" },
        label.clone(),
        l.edit_label,
        Align::Center,
    );
    for ((text, op), b) in [("RENAME", SnapshotOp::Rename), ("DELETE", SnapshotOp::Delete), ("MOVE", SnapshotOp::Move)]
        .into_iter()
        .zip(l.edit)
    {
        button(ui, text, has_target, b, op);
    }
    button(ui, "IMPORT ALL", true, l.transfer[0], SnapshotOp::ImportAll);
    button(ui, "EXPORT ALL", !info.folders.is_empty(), l.transfer[1], SnapshotOp::ExportAll);

    // FOLDERS | SNAPSHOTS: headers over a 1-pt rule, a divider between the columns.
    let header = Ui::plain_style("Helvetica", 11.0, true, Rgba::WHITE.with_alpha(0.7));
    for (t, h) in ["FOLDERS", "SNAPSHOTS"].into_iter().zip(l.headers) {
        ui.text(t, header.clone(), h, Align::Center);
    }
    let lists_w = l.lists[1].max_x() - l.lists[0].x;
    ui.fill(Rect::new(l.lists[0].x, r.y + 39.0, lists_w, 1.0), Rgba::rgb8(33, 34, 35));
    ui.fill(Rect::new(l.lists[0].max_x(), r.y + 10.0, 1.0, r.h - 10.0), Rgba::rgb8(33, 34, 35));
    let mut row_style = ui.style("views", "List-View-Row");
    let rows = |ui: &mut Ui,
                list: Rect,
                names: Vec<(String, bool, bool)>,
                op: fn(u16) -> SnapshotOp,
                style: &mut crate::draw::TextStyle| {
        ui.background("List-Background", list);
        // ponytail: no scrolling; rows past the list's bottom are cut (a scroll region when
        // snapshot lists outgrow ~23 rows).
        ui.clip(Some(list));
        for (i, (name, selected, current)) in names.into_iter().enumerate() {
            let row = Rect::new(list.x, list.y + i as f32 * ROW_H, list.w, ROW_H);
            if row.y >= list.max_y() {
                break;
            }
            if selected {
                ui.fill(row, Rgba::rgb8(64, 58, 40));
            } else if current {
                ui.fill(row, Rgba::WHITE.with_alpha(0.06));
            }
            style.color = Rgba::WHITE.with_alpha(if selected { 0.9 } else { 0.7 });
            ui.text(
                if name.is_empty() { "-" } else { &name },
                style.clone(),
                row.inset(10.0, 0.0, 10.0, 0.0),
                Align::Left,
            );
            ui.hit(row, sid(op(i as u16)), HitKind::Tap);
        }
        ui.clip(None);
    };
    let cur_folder = SnapshotInfo::split(&info.current).0;
    let folders = info
        .folders
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), Some(i) == fsel, f.name == cur_folder && info.contains(&info.current)))
        .collect();
    rows(ui, l.lists[0], folders, SnapshotOp::Folder, &mut row_style);
    let snaps = folder
        .map(|f| {
            f.snapshots
                .iter()
                .map(|n| {
                    let full = SnapshotInfo::full(&f.name, n);
                    (n.clone(), full == info.primed, full == info.current)
                })
                .collect()
        })
        .unwrap_or_default();
    rows(ui, l.lists[1], snaps, SnapshotOp::Snapshot, &mut row_style);

    // CURRENT / PRIMED (name part, "-" when empty) on their panels, ▲ ▼, RECALL.
    let value = Ui::plain_style("Helvetica", 12.0, true, Rgba::WHITE.with_alpha(0.9));
    for ((caption, v), [cap, val]) in
        [("CURRENT", &info.current), ("PRIMED", &info.primed)].into_iter().zip([l.current, l.primed])
    {
        let panel = Rect::new(cap.x, cap.y, cap.w, val.max_y() - cap.y);
        ui.fill(panel, Rgba::rgb8(83, 86, 88));
        ui.fill(panel.inset(1.0, 1.0, 1.0, 1.0), Rgba::rgb8(46, 49, 52));
        ui.text(caption, label.clone(), cap, Align::Center);
        let shown = SnapshotInfo::split(v).1;
        ui.text(if shown.is_empty() { "-" } else { shown }, value.clone(), val, Align::Center);
    }
    for (up, b) in [true, false].into_iter().zip(l.arrows) {
        button(ui, "", true, b, SnapshotOp::Step(up));
        if let Some(img) =
            ui.skin("views", if up { "Popover-Up" } else { "Popover-Down" }).and_then(|s| s.image("image"))
        {
            let img = img.to_string();
            ui.image(&img, b.centered(12.0, 12.0), None, None, 0.6);
        }
    }
    button(ui, "RECALL", primed_ok, l.recall, SnapshotOp::Recall);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PreviewMixer;
    use crate::window::{SCENES_W, WindowLayout};

    fn tap(v: &mut ViewState, m: &mut PreviewMixer, op: SnapshotOp) {
        v.dispatch(Action::Tap(sid(op)), m);
    }

    #[test]
    fn layout_follows_the_rack_geometry() {
        let l = SnapshotsLayout::new(Rect::new(0.0, 26.0, SNAPSHOTS_W, 984.0));
        assert_eq!(l.file[0], Rect::new(0.0, 46.0, 78.0, 40.0));
        assert_eq!(l.edit[2], Rect::new(0.0, 268.0, 78.0, 40.0));
        assert_eq!(l.transfer, [Rect::new(0.0, 924.0, 78.0, 40.0), Rect::new(0.0, 964.0, 78.0, 40.0)]);
        assert_eq!(l.lists[0], Rect::new(80.0, 66.0, 169.0, 944.0));
        assert_eq!(l.lists[1].x, 250.0);
        assert_eq!(l.current, [Rect::new(428.0, 32.0, 106.0, 24.0), Rect::new(428.0, 56.0, 106.0, 24.0)]);
        assert_eq!(l.recall, Rect::new(422.0, 214.0, 118.0, 40.0));
        // The column sits after PLUG-IN SCENES and pushes the browser right.
        let mut v = ViewState { show_snapshots: true, show_scenes: true, ..Default::default() };
        assert_eq!(WindowLayout::new(&v, 8, 2).browser_x(), SCENES_W + SNAPSHOTS_W);
        assert_eq!(WindowLayout::new(&v, 8, 2).snapshots_x(), SCENES_W);
        v.show_scenes = false;
        assert_eq!(WindowLayout::new(&v, 8, 2).snapshots_x(), 0.0);
        assert!(v.open_named("snapshots") && v.show_snapshots);
    }

    #[test]
    fn hits_route_to_requests() {
        let assets = crate::test_support::assets("{}");
        let mut m = PreviewMixer::default();
        let mut v = ViewState { show_snapshots: true, ..Default::default() };
        let mut ui = Ui::new(&assets);
        draw_snapshots_section(&mut ui, &m, &v, Rect::new(0.0, 26.0, SNAPSHOTS_W, 984.0));
        let hit = |c: SnapshotOp| ui.hit_rect(&sid(c)).unwrap_or_else(|| panic!("{c:?} not on screen"));
        assert_eq!(hit(SnapshotOp::New), Rect::new(3.0, 129.0, 72.0, 34.0));
        assert_eq!(hit(SnapshotOp::Folder(1)), Rect::new(80.0, 106.0, 169.0, 40.0));
        assert_eq!(hit(SnapshotOp::Snapshot(0)), Rect::new(250.0, 66.0, 169.0, 40.0));
        // Prime a row, recall it; NEW saves the next Untitled and recalls it.
        tap(&mut v, &mut m, SnapshotOp::Snapshot(1));
        assert_eq!(m.snapshots().primed, "000 - Untitled/002 - Verse");
        tap(&mut v, &mut m, SnapshotOp::Recall);
        assert_eq!(m.snapshots().current, "000 - Untitled/002 - Verse");
        tap(&mut v, &mut m, SnapshotOp::New);
        assert_eq!(m.snapshots().current, "000 - Untitled/003 - Untitled");
        // ▼ past the last row wraps into the next folder.
        tap(&mut v, &mut m, SnapshotOp::Snapshot(2));
        tap(&mut v, &mut m, SnapshotOp::Step(false));
        assert_eq!(m.snapshots().primed, "001 - Show/001 - Intro");
        // A folder row change clears the prime; RENAME then renames the focused folder.
        tap(&mut v, &mut m, SnapshotOp::Folder(0));
        assert_eq!(m.snapshots().primed, "");
        tap(&mut v, &mut m, SnapshotOp::Folder(1));
        tap(&mut v, &mut m, SnapshotOp::Rename);
        assert!(matches!(&v.popover, Some(PopoverState { kind: Popover::SnapshotText { .. }, .. })));
        v.text_edit.as_mut().unwrap().text = "002 - Live".into();
        v.key_enter(&mut m);
        assert_eq!(m.snapshots().folders[1].name, "002 - Live");
        // DELETE asks first.
        tap(&mut v, &mut m, SnapshotOp::Delete);
        assert!(matches!(&v.popover, Some(PopoverState { kind: Popover::Confirm { .. }, .. })));
        v.dispatch(Action::Tap(request_id(SnapshotRequest::Delete("002 - Live".into()))), &mut m);
        assert_eq!(m.snapshots().folders.len(), 1);
    }
}
