//! Sessions for rack's SESSION menu: the engine's open session and lists
//! ([`ua_engine::SessionState`]) as a [`rack::SessionInfo`], and the session requests the menu
//! makes, sent as [`ua_engine::SessionCommand`]s through the store's write gates.

use rack::surfaces::{SessionFile, SessionInfo, SessionOp};
use rack::{Action, Control, ControlId};
use ua_engine::{SessionCommand, SessionEntry, SessionState};

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
        }
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
            // The view turns these into the name / path fields (`set_text` below).
            SessionOp::Open | SessionOp::SaveAs => None,
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
            _ => return false,
        };
        self.session_command(&command);
        true
    }
}
