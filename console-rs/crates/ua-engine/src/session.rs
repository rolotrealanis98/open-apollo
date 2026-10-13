//! Console sessions (`.uadmix` files the engine keeps) on TCP 4710: the root properties that
//! describe the open session and the commands that load, save and manage sessions.
//!
//! Sources (details and addresses in `console-port/surfaces/sessions.md`):
//! - UA Mixer Engine 12.0.0's root command catalogue (`get /?commands=1`), which names every
//!   command and its argument keys.
//! - UAD Console 1.2.9 `Rack.dll`, which shows how Console builds each request:
//!   `Rack_MixerPresets` (the navigator SESSION box and its menu), `Rack_Window` (New, Open,
//!   Save, Save As, the save-changes question) and `MxClnt_UAD2MixerPresets` (delete, rename,
//!   folders).
//! - Traces of Console against the open-apollo daemon (`get /Session/values`,
//!   `post /SaveLastUsedSession {}`).
//!
//! Loading a session from the list is a `set` of `/Session` (a `file`-typed enum whose `values`
//! is the Sessions folder tree). Restoring a modified (auto-saved) session is a `set` of
//! `/ModifiedSessions`. Everything else is a `post`.

use crate::command::Command;
use crate::path;
use crate::value::{Json, Value};

/// The open session: its name relative to the Sessions folder (`[folder/]name`, no extension;
/// `../../../Desktop/x/Name` for a file opened from elsewhere). `values` is the folder tree.
pub const SESSION: &str = "/Session";
/// Sessions with unsaved changes the engine kept: `values` = `[{value, time}]`.
pub const MODIFIED_SESSIONS: &str = "/ModifiedSessions";
/// The open session has unsaved changes (Console shows `Name*`).
pub const DIRTY: &str = "/Dirty";
/// Text of a running engine operation (empty when idle).
pub const PROGRESS: &str = "/Progress";
/// Console snapshots (Apollo). `volumes` lists the import/export targets.
pub const SNAPSHOT: &str = "/Snapshot";
pub const PRIMED_SNAPSHOT: &str = "/PrimedSnapshot";
/// The session file extension Console appends on Save As.
pub const EXTENSION: &str = ".uadmix";

/// `ModifiedSessions` value of a never-saved session.
pub const NEW_SESSION_VALUE: &str = "newSession";
/// `ModifiedSessions` value of a session synced from an external app.
pub const SYNC_SESSION_VALUE: &str = "extAppSyncSession";
/// How Console labels those two (`Rack_MixerPresets`, Rack.dll 0x180dafed0).
pub const NEW_SESSION_LABEL: &str = "- New Session -";
pub const SYNC_SESSION_LABEL: &str = "- Sync Session -";

/// Root properties that describe the session; the store re-reads the `values` lists of
/// [`SESSION`] and [`MODIFIED_SESSIONS`] when one of these changes.
pub const IDENTITY_PATHS: [&str; 4] = [SESSION, MODIFIED_SESSIONS, DIRTY, PROGRESS];
/// Properties whose `values` list is the session menu's content.
pub const LIST_PATHS: [&str; 2] = [SESSION, MODIFIED_SESSIONS];

/// One request that changes the engine's session. Every variant is a write: send it through
/// [`Store::session`](crate::Store::session), which honours both write gates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    /// Open a session from the Sessions folder: `set /Session/value "<[folder/]name>"`
    /// (Rack.dll 0x18172a210 → `UAClnt_Values::Set` on the `Session` values object).
    Load(String),
    /// Restore a modified session: `set /ModifiedSessions/value "<value>"` (0x180db1bc0, items
    /// from 0x2717).
    RestoreModified(String),
    /// `post /NewSession {}` (0x1817e69d0; Console sends no `type`).
    New,
    /// Open a session file by path on the engine host: `post /OpenSession {"path":"…"}`
    /// (0x1817e6db0).
    Open { path: String },
    /// `post /SaveSession {}` (0x1817ea2e0).
    Save,
    /// `post /SaveSessionAs {"name":"…"}` (0x1817ea690). `name` is a path or a name relative to
    /// the Sessions folder; [`SessionCommand::save_as`] appends `.uadmix` like Console.
    SaveAs { name: String },
    /// `post /DeleteSession {"name":"…"}` (`MxClnt_UAD2MixerPresets::DeletePreset`).
    Delete { name: String },
    /// `post /RenameSession {"origName":"…","newName":"…"}` (`…::RenamePreset`).
    Rename { from: String, to: String },
    /// `post /NewFolder {"folder":"…","type":"session"}`. The engine's `CMxNewFolder::execute`
    /// takes `type: "session"` for session folders; a `path` is for preset folders only
    /// (`<effect or channel>/Preset`) and anything else fails (protocol/v131/MAP.md).
    NewFolder { folder: String },
    /// `post /DeleteFolder {"folder":"…","type":"session"}` (same handler shape).
    DeleteFolder { folder: String },
    /// `post /SaveLastUsedSession {}`: Console sends it after each edit (T6:33331).
    SaveLastUsed,
}

impl SessionCommand {
    /// Save As with Console's naming: `.uadmix` appended unless present.
    pub fn save_as(name: &str) -> SessionCommand {
        let name = name.trim();
        let name = if name.to_ascii_lowercase().ends_with(EXTENSION) {
            name.to_owned()
        } else {
            format!("{name}{EXTENSION}")
        };
        SessionCommand::SaveAs { name }
    }

    /// The wire command.
    pub fn command(&self) -> Command {
        let post = |cmd: &str, body: Json| Command::post(format!("/{cmd}"), Some(body.to_string()));
        match self {
            SessionCommand::Load(name) => Command::set(SESSION, &Value::String(name.clone()), false),
            SessionCommand::RestoreModified(v) => {
                Command::set(MODIFIED_SESSIONS, &Value::String(v.clone()), false)
            }
            SessionCommand::New => post("NewSession", serde_json::json!({})),
            SessionCommand::Open { path } => post("OpenSession", serde_json::json!({ "path": path })),
            SessionCommand::Save => post("SaveSession", serde_json::json!({})),
            SessionCommand::SaveAs { name } => post("SaveSessionAs", serde_json::json!({ "name": name })),
            SessionCommand::Delete { name } => post("DeleteSession", serde_json::json!({ "name": name })),
            SessionCommand::Rename { from, to } => {
                post("RenameSession", serde_json::json!({ "origName": from, "newName": to }))
            }
            SessionCommand::NewFolder { folder } => {
                post("NewFolder", serde_json::json!({ "folder": folder, "type": "session" }))
            }
            SessionCommand::DeleteFolder { folder } => {
                post("DeleteFolder", serde_json::json!({ "folder": folder, "type": "session" }))
            }
            SessionCommand::SaveLastUsed => post("SaveLastUsedSession", serde_json::json!({})),
        }
    }

    /// False for a request the engine must never get: an empty Save As name (only `.uadmix`) or
    /// Open path, and empty names or folders elsewhere. UA Mixer Engine 12 answers an empty
    /// `SaveSessionAs` / a missing `OpenSession` path with a native file dialog on the engine
    /// host (MAP.md, "Sessions").
    pub fn is_sendable(&self) -> bool {
        let blank = |s: &str| s.trim().is_empty();
        match self {
            SessionCommand::SaveAs { name } => {
                let stem = name.trim();
                let stem = if stem.to_ascii_lowercase().ends_with(EXTENSION) { &stem[..stem.len() - EXTENSION.len()] } else { stem };
                !blank(stem)
            }
            SessionCommand::Open { path } => !blank(path),
            SessionCommand::Load(n) | SessionCommand::Delete { name: n } => !blank(n),
            SessionCommand::Rename { from, to } => !blank(from) && !blank(to),
            SessionCommand::NewFolder { folder } | SessionCommand::DeleteFolder { folder } => !blank(folder),
            _ => true,
        }
    }

    /// Commands that replace the open session (Console asks to save a dirty session first).
    pub fn replaces_session(&self) -> bool {
        matches!(
            self,
            SessionCommand::Load(_)
                | SessionCommand::RestoreModified(_)
                | SessionCommand::New
                | SessionCommand::Open { .. }
        )
    }
}

/// Console snapshots (`MxClnt_Snapshots`, the `Rack_SnapshotsSection` panel: FILE SAVE /
/// SAVE AS... / NEW / RENAME / DELETE / MOVE / IMPORT ALL / EXPORT ALL, FOLDERS, SNAPSHOTS,
/// CURRENT, PRIMED, RECALL). Argument keys from Rack.dll 0x181972140…0x181970c00; recall and
/// prime are assumed to be `set`s of `/Snapshot` and `/PrimedSnapshot` (not observed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotCommand {
    /// `set /Snapshot/value "<[folder/]name>"` (assumed).
    Recall(String),
    /// `set /PrimedSnapshot/value "<[folder/]name>"` (assumed).
    Prime(String),
    /// `post /SaveAsSnapshot {"snapshotName":…,"isNew":…}`.
    SaveAs { name: String, is_new: bool },
    /// `post /DeleteSnapshot {"snapshotName":…}`.
    Delete { name: String },
    /// `post /MoveSnapshot {"origName":…,"newName":…,"shuffle":…}` (rename and reorder).
    Move { from: String, to: String, shuffle: bool },
    /// `post /ExportSnapshots {"volume":…,"folder":…}` (`volume` from `/Snapshot`'s `volumes`).
    Export { volume: String, folder: String },
    /// `post /ImportSnapshots {"volume":…,"folder":…,"append":…}`.
    Import { volume: String, folder: String, append: bool },
}

impl SnapshotCommand {
    pub fn command(&self) -> Command {
        let post = |cmd: &str, body: Json| Command::post(format!("/{cmd}"), Some(body.to_string()));
        match self {
            SnapshotCommand::Recall(n) => Command::set(SNAPSHOT, &Value::String(n.clone()), false),
            SnapshotCommand::Prime(n) => Command::set(PRIMED_SNAPSHOT, &Value::String(n.clone()), false),
            SnapshotCommand::SaveAs { name, is_new } => {
                post("SaveAsSnapshot", serde_json::json!({ "snapshotName": name, "isNew": is_new }))
            }
            SnapshotCommand::Delete { name } => post("DeleteSnapshot", serde_json::json!({ "snapshotName": name })),
            SnapshotCommand::Move { from, to, shuffle } => post(
                "MoveSnapshot",
                serde_json::json!({ "origName": from, "newName": to, "shuffle": shuffle }),
            ),
            SnapshotCommand::Export { volume, folder } => {
                post("ExportSnapshots", serde_json::json!({ "volume": volume, "folder": folder }))
            }
            SnapshotCommand::Import { volume, folder, append } => post(
                "ImportSnapshots",
                serde_json::json!({ "volume": volume, "folder": folder, "append": append }),
            ),
        }
    }
}

/// One entry of the `/Session` values tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEntry {
    File { name: String, path: String, locked: bool },
    Folder { name: String, path: String, locked: bool, children: Vec<SessionEntry> },
}

impl SessionEntry {
    pub fn name(&self) -> &str {
        match self {
            SessionEntry::File { name, .. } | SessionEntry::Folder { name, .. } => name,
        }
    }

    /// `[folder/]name`, the value [`SessionCommand::Load`] sets.
    pub fn path(&self) -> &str {
        match self {
            SessionEntry::File { path, .. } | SessionEntry::Folder { path, .. } => path,
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self, SessionEntry::Folder { .. })
    }

    /// Every file below this entry (itself for a file), in order.
    pub fn files(&self) -> Vec<&SessionEntry> {
        match self {
            SessionEntry::File { .. } => vec![self],
            SessionEntry::Folder { children, .. } => children.iter().flat_map(SessionEntry::files).collect(),
        }
    }
}

/// Parses a `values` list: `{type:"file",value,locked}` and
/// `{type:"folder",value,locked,children:[…]}` (the shape `Preset/values` uses too). Plain strings
/// are files.
pub fn parse_entries(values: &Json) -> Vec<SessionEntry> {
    fn walk(list: &[Json], prefix: &str) -> Vec<SessionEntry> {
        list.iter()
            .filter_map(|item| {
                let (name, ty, locked, children) = match item {
                    Json::String(s) => (s.clone(), "file", false, None),
                    Json::Object(o) => (
                        o.get("value")?.as_str()?.to_owned(),
                        o.get("type").and_then(Json::as_str).unwrap_or("file"),
                        o.get("locked").and_then(Json::as_bool).unwrap_or(false),
                        o.get("children").and_then(Json::as_array),
                    ),
                    _ => return None,
                };
                let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                Some(if ty == "folder" {
                    let children = children.map(|c| walk(c, &path)).unwrap_or_default();
                    SessionEntry::Folder { name, path, locked, children }
                } else {
                    SessionEntry::File { name, path, locked }
                })
            })
            .collect()
    }
    match values {
        Json::Array(list) => walk(list, ""),
        _ => Vec::new(),
    }
}

/// A `/ModifiedSessions` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModifiedSession {
    /// The value to set to restore it (`newSession`, `extAppSyncSession` or a session name).
    pub value: String,
    /// When the engine kept it, as the engine formats it (`Sep 18, 2026 05:14:34 AM`).
    pub time: String,
}

impl ModifiedSession {
    /// Console's menu label: `[<time>] - <name>`, the two special values shown as
    /// `- New Session -` / `- Sync Session -` (Rack.dll 0x180dafed0).
    pub fn label(&self) -> String {
        let name = display_name(&self.value);
        if self.time.is_empty() { name } else { format!("[{}] - {name}", self.time) }
    }
}

pub fn parse_modified(values: &Json) -> Vec<ModifiedSession> {
    let Json::Array(list) = values else { return Vec::new() };
    list.iter()
        .filter_map(|item| match item {
            Json::String(s) => Some(ModifiedSession { value: s.clone(), time: String::new() }),
            Json::Object(o) => Some(ModifiedSession {
                value: o.get("value")?.as_str()?.to_owned(),
                time: o.get("time").and_then(Json::as_str).unwrap_or_default().to_owned(),
            }),
            _ => None,
        })
        .collect()
}

/// How Console names a session value: empty and `newSession` → `- New Session -`,
/// `extAppSyncSession` → `- Sync Session -`, else the last path component.
pub fn display_name(value: &str) -> String {
    match value {
        "" | NEW_SESSION_VALUE => NEW_SESSION_LABEL.to_owned(),
        SYNC_SESSION_VALUE => SYNC_SESSION_LABEL.to_owned(),
        v => v.rsplit('/').next().unwrap_or(v).to_owned(),
    }
}

/// Everything the UI shows about sessions, read from a tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionState {
    /// `/Session` value (`[folder/]name`, or a relative path for a file opened from elsewhere).
    pub current: String,
    pub dirty: bool,
    pub progress: String,
    pub entries: Vec<SessionEntry>,
    pub modified: Vec<ModifiedSession>,
    /// `/Snapshot` and `/PrimedSnapshot` values (Console snapshots).
    pub snapshot: String,
    pub primed_snapshot: String,
}

impl SessionState {
    pub fn from_tree(tree: &crate::Tree) -> SessionState {
        let s = |p: &str| tree.value(p).and_then(Value::as_str).unwrap_or_default().to_owned();
        let list = |p: &str| tree.meta(p).and_then(|m| m.values.clone()).unwrap_or(Json::Null);
        SessionState {
            current: s(SESSION),
            dirty: tree.value(DIRTY).and_then(Value::as_bool).unwrap_or(false),
            progress: s(PROGRESS),
            entries: parse_entries(&list(SESSION)),
            modified: parse_modified(&list(MODIFIED_SESSIONS)),
            snapshot: s(SNAPSHOT),
            primed_snapshot: s(PRIMED_SNAPSHOT),
        }
    }

    /// The name Console shows: [`display_name`] of the current value.
    pub fn name(&self) -> String {
        display_name(&self.current)
    }

    /// The `/Session` value is a file in the Sessions folder (so Save does not need a name).
    pub fn has_file(&self) -> bool {
        !self.current.is_empty() && self.current != NEW_SESSION_VALUE
    }
}

/// `<property>/values`, the selector [`Store`](crate::Store) reads to refresh a list.
pub fn values_path(property: &str) -> String {
    path::join(path::property_path(property), "values")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes_match_console() {
        let t = |c: SessionCommand| c.command().text();
        assert_eq!(t(SessionCommand::Load("Live/Band".into())), r#"set /Session/value "Live/Band""#);
        assert_eq!(t(SessionCommand::RestoreModified("newSession".into())), r#"set /ModifiedSessions/value "newSession""#);
        assert_eq!(t(SessionCommand::New), "post /NewSession {}");
        assert_eq!(t(SessionCommand::Open { path: "/Users/x/A.uadmix".into() }), r#"post /OpenSession {"path":"/Users/x/A.uadmix"}"#);
        assert_eq!(t(SessionCommand::Save), "post /SaveSession {}");
        assert_eq!(t(SessionCommand::save_as("Tracking")), r#"post /SaveSessionAs {"name":"Tracking.uadmix"}"#);
        assert_eq!(t(SessionCommand::save_as("Mix.UADMIX")), r#"post /SaveSessionAs {"name":"Mix.UADMIX"}"#);
        assert_eq!(t(SessionCommand::Delete { name: "Old".into() }), r#"post /DeleteSession {"name":"Old"}"#);
        assert_eq!(
            t(SessionCommand::Rename { from: "A".into(), to: "B".into() }),
            r#"post /RenameSession {"newName":"B","origName":"A"}"#
        );
        assert_eq!(t(SessionCommand::SaveLastUsed), "post /SaveLastUsedSession {}");
        assert!(SessionCommand::Save.command().is_write());
        let snap = |c: SnapshotCommand| c.command().text();
        assert_eq!(snap(SnapshotCommand::Recall("A".into())), r#"set /Snapshot/value "A""#);
        assert!(snap(SnapshotCommand::SaveAs { name: "A".into(), is_new: true }).starts_with("post /SaveAsSnapshot {"));
        assert!(snap(SnapshotCommand::Import { volume: "V".into(), folder: "F".into(), append: false }).contains(r#""append":false"#));
        assert!(SessionCommand::Load("x".into()).replaces_session());
        assert!(!SessionCommand::Save.replaces_session());
    }

    #[test]
    fn parses_folder_trees_and_modified_lists() {
        let v: Json = serde_json::json!([
            {"value": "Session", "type": "file", "locked": false},
            {"value": "Live", "type": "folder", "locked": false, "children": [
                {"value": "Band", "type": "file", "locked": true}
            ]},
            "Loose"
        ]);
        let e = parse_entries(&v);
        assert_eq!(e.len(), 3);
        assert_eq!(e[1].path(), "Live");
        let files: Vec<&str> = e.iter().flat_map(SessionEntry::files).map(SessionEntry::path).collect();
        assert_eq!(files, ["Session", "Live/Band", "Loose"]);
        let m = parse_modified(&serde_json::json!([
            {"value": "newSession", "time": "Sep 18, 2026 05:14:34 AM"},
            {"value": "Live/Band", "time": ""}
        ]));
        assert_eq!(m[0].label(), "[Sep 18, 2026 05:14:34 AM] - - New Session -");
        assert_eq!(m[1].label(), "Band");
        assert_eq!(display_name("../../../Desktop/Example/My Session"), "My Session");
        assert_eq!(display_name(""), NEW_SESSION_LABEL);
        assert_eq!(values_path("/Session"), "/Session/values");
    }
}
