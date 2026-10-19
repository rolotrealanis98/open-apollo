//! Sessions for rack's SESSION menu: the engine's open session and lists
//! ([`ua_engine::SessionState`]) as a [`rack::SessionInfo`], and the session requests the menu
//! makes, sent as [`ua_engine::SessionCommand`]s through the store's write gates.

use rack::surfaces::session::{PanelRowKind, panel_rows};
use rack::surfaces::{SessionFile, SessionInfo, SessionOp, SnapshotFolder, SnapshotInfo, SnapshotRequest};
use rack::{Action, Control, ControlId};
use ua_engine::{SessionCommand, SessionEntry, SessionState, SnapshotCommand};

use crate::EngineMixer;

/// The menu's file rows: every file of the Sessions folder tree, folders flattened.
fn files(state: &SessionState) -> Vec<SessionFile> {
    state
        .entries
        .iter()
        .flat_map(SessionEntry::files)
        .map(|f| SessionFile { path: f.path().to_owned(), label: f.path().to_owned() })
        .collect()
}

impl EngineMixer {
    /// The engine's session state (name, dirty, lists).
    pub fn session_state(&self) -> SessionState {
        self.store.session_state()
    }

    pub(crate) fn session_info(&self) -> SessionInfo {
        let st = self.store.session_state();
        SessionInfo {
            name: st.name(),
            current: st.current.clone(),
            dirty: st.dirty,
            has_file: st.has_file(),
            progress: st.progress.clone(),
            files: files(&st),
            modified: st.modified.iter().map(|m| m.label()).collect(),
            local_dir: self.local_sessions_dir.clone(),
        }
    }

    /// The `[parent/]folder` path of CURRENT SESSION row `row`, when it is a (non-root) folder.
    fn folder_of_row(&self, row: u16) -> Option<String> {
        match panel_rows(&self.session_info()).get(row as usize)?.kind.clone() {
            PanelRowKind::Folder { path, root } if !root && !path.is_empty() => Some(path),
            _ => None,
        }
    }

    /// The Snapshots section's view of `/Snapshot`, `/PrimedSnapshot` and `/Snapshot/values`:
    /// top-level folders with their snapshots; loose snapshots in a folder named `""`.
    pub(crate) fn snapshot_info(&self) -> SnapshotInfo {
        let st = self.store.session_state();
        let mut folders: Vec<SnapshotFolder> = Vec::new();
        for e in &st.snapshots {
            match e {
                SessionEntry::Folder { name, .. } => folders.push(SnapshotFolder {
                    name: name.clone(),
                    snapshots: e.files().iter().map(|f| f.path()[name.len() + 1..].to_owned()).collect(),
                }),
                SessionEntry::File { name, .. } => match folders.iter_mut().find(|f| f.name.is_empty()) {
                    Some(f) => f.snapshots.push(name.clone()),
                    None => folders.push(SnapshotFolder { name: String::new(), snapshots: vec![name.clone()] }),
                },
            }
        }
        SnapshotInfo { current: st.snapshot, primed: st.primed_snapshot, folders, volumes: st.snapshot_volumes }
    }

    /// A Snapshots section request as the engine's snapshot command (through the write gates).
    pub(crate) fn apply_snapshot(&mut self, action: &Action) -> bool {
        let Action::Tap(ControlId { control: Control::SnapshotRequest(r), .. }) = action else { return false };
        let command = match r.clone() {
            SnapshotRequest::Recall(n) => SnapshotCommand::Recall(n),
            SnapshotRequest::Prime(n) => SnapshotCommand::Prime(n),
            SnapshotRequest::Save { name, is_new } => SnapshotCommand::SaveAs { name, is_new },
            SnapshotRequest::Delete(name) => SnapshotCommand::Delete { name },
            SnapshotRequest::Move { from, to, shuffle } => SnapshotCommand::Move { from, to, shuffle },
            SnapshotRequest::Export { volume, folder } => SnapshotCommand::Export { volume, folder },
            SnapshotRequest::Import { volume, folder, append } => SnapshotCommand::Import { volume, folder, append },
        };
        self.store.snapshot_command(&command);
        true
    }

    /// Sends a session command (gated: suppressed while writes are off).
    pub fn session_command(&mut self, command: &SessionCommand) {
        self.store.session(command);
    }

    /// Session taps from the menu. True when handled.
    pub(crate) fn apply_session(&mut self, action: &Action) -> bool {
        let Action::Tap(ControlId { control: Control::Session(op), .. }) = action else { return false };
        let st = self.store.session_state();
        let command = match *op {
            SessionOp::New => Some(SessionCommand::New),
            SessionOp::Save => st.has_file().then_some(SessionCommand::Save),
            SessionOp::Load(i) => files(&st).get(i as usize).map(|f| SessionCommand::Load(f.path.clone())),
            SessionOp::Restore(i) => st.modified.get(i as usize).map(|m| SessionCommand::RestoreModified(m.value.clone())),
            SessionOp::Delete(i) => files(&st).get(i as usize).map(|f| SessionCommand::Delete { name: f.path.clone() }),
            SessionOp::FolderDelete(row) => self.folder_of_row(row).map(|folder| SessionCommand::DeleteFolder { folder }),
            // The view turns these into the name / path fields (`set_text` below), or reveals a
            // file locally (Show In Finder sends nothing to the engine).
            SessionOp::Open
            | SessionOp::SaveAs
            | SessionOp::Rename(_)
            | SessionOp::NewFolder
            | SessionOp::FolderRename(_)
            | SessionOp::NewFolderIn(_)
            | SessionOp::Reveal(_) => None,
        };
        if let Some(c) = command {
            self.session_command(&c);
        }
        true
    }

    /// Save As name / Open path. True when handled.
    pub(crate) fn session_set_text(&mut self, id: &ControlId, text: &str) -> bool {
        let command = match id.control {
            Control::SessionSaveAsName => SessionCommand::save_as(text),
            Control::SessionOpenPath => SessionCommand::Open { path: text.trim().to_owned() },
            // Rename keeps the file in its folder (`RenameSession {origName, newName}`).
            Control::SessionRenameTo(i) => {
                let st = self.store.session_state();
                let Some(f) = files(&st).get(i as usize).map(|f| f.path.clone()) else { return true };
                let to = match f.rsplit_once('/') {
                    Some((dir, _)) => format!("{dir}/{}", text.trim()),
                    None => text.trim().to_owned(),
                };
                SessionCommand::Rename { from: f, to }
            }
            Control::SessionNewFolderName => SessionCommand::NewFolder { folder: text.trim().to_owned() },
            // A folder keeps its parent: `RenameSession {origName: "A/B", newName: "A/C"}`.
            Control::SessionFolderRenameTo(row) => {
                let Some(from) = self.folder_of_row(row) else { return true };
                let to = match from.rsplit_once('/') {
                    Some((parent, _)) => format!("{parent}/{}", text.trim()),
                    None => text.trim().to_owned(),
                };
                SessionCommand::Rename { from, to }
            }
            Control::SessionNewFolderIn(row) => {
                let Some(parent) = self.folder_of_row(row) else { return true };
                SessionCommand::NewFolder { folder: format!("{parent}/{}", text.trim()) }
            }
            _ => return false,
        };
        self.session_command(&command);
        true
    }
}
