//! The Console window's surfaces around the strips: side navigator, plug-in browser, session
//! navigator, status bar, monitor pages, cells (inserts / sends), popovers and the settings
//! sheet. UI-only state lives in [`ViewState`], which the app shell owns and feeds to
//! [`crate::window::build_window_with`]; [`ViewState::dispatch`] routes pointer actions
//! between it and the model.

pub mod browser;
pub mod cells;
pub mod channels;
pub mod control_room;
pub mod monitor;
pub mod navigator;
pub mod panels;
pub mod popover;
pub mod session;
pub mod session_nav;
pub mod settings;
pub mod snapshots;
pub mod status;
pub mod view;

pub use popover::{MenuItem, Placement};
pub use session::{SaveChoice, SessionFile, SessionInfo, SessionOp};
pub use snapshots::{SnapshotFolder, SnapshotInfo, SnapshotRequest};
pub use view::{
    BrowserTab, BrowserView, Effect, InsertSize, Modifier, MonitorPage, Popover, PopoverState, Section, Sections, SettingsView, Sheet, TextEdit, TextTarget, ViewState,
    is_view_control,
};
