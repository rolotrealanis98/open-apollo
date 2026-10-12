//! Render-agnostic UAD Console UI. Each frame the window is rebuilt into a [`DrawList`]
//! (textured quads, fills, text, clips) plus hit regions; `gfx` draws the list and
//! [`input::Interaction`] turns pointer events on hit regions into [`input::Action`]s that a
//! [`model::MixerModel`] applies.

pub mod dnd;
pub mod draw;
pub mod geometry;
pub mod input;
pub mod layers;
pub mod menu;
mod menu_metrics;
pub mod model;
pub mod preamp;
pub mod presets;
pub mod scales;
pub mod settings;
pub mod surfaces;
pub mod ui;
pub mod widgets;
pub mod window;

pub use draw::{Align, Draw, DrawList, Frame, TextStyle};
pub use houston::{Insets, Rect, Rgba};
pub use layers::{Layer, LayerKind, LayerMark, LayerPart, Windowing};
pub use input::{Action, Control, ControlId, Hit, HitKind, Interaction, PluginWindowButton, ScrollRegion, ScrollTarget};
pub use model::{ConsoleModel, CueOutput, EngineSettings, HeadphoneOutput, InsertLabel, MixerModel, MonitorOptions, PluginBadge, PluginEntry, PresetNode, PreviewMixer, StatusInfo};
pub use menu::{Command, MenuCmd, PopupMenu};
pub use surfaces::{Effect, SessionInfo, SessionOp, ViewState};
pub use ui::{Assets, Ui};
pub use window::{WindowLayout, build_window, build_window_with};
