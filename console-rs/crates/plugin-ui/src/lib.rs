//! Plug-in windows for console-rs: tapping a loaded insert opens that plug-in's UI and drives its
//! parameters through the UA Mixer Engine.
//!
//! - [`spec`] / [`library`]: ConsoleLink's generated panel specs (`PluginSpec_*.json` plus
//!   `Images/*@2x.png`), found at runtime (never bundled; see [`SpecLocator`]).
//! - [`PluginPanel`]: a spec drawn into a `rack::Ui` with hit regions
//!   (`rack::Control::PluginParam/PluginElement/PluginStep`).
//! - [`GenericPanel`]: a parameter list for plug-ins without a spec.
//! - [`ParamSource`]: where values come from; [`MockParams`] for renders and tests. The engine
//!   adapter maps it onto [`paths`].
//! - [`meters`]: VendorSpecific meter queries, decoding, calibration and ballistics.

pub mod generic;
pub mod library;
pub mod meters;
pub mod panel;
pub mod params;
pub mod paths;
pub mod spec;

pub use generic::GenericPanel;
pub use library::{Entry, PluginSpecLibrary, SpecLocator};
pub use meters::MeterFeed;
pub use panel::PluginPanel;
pub use params::{MockEvent, MockParam, MockParams, ParamSource};
pub use paths::{ParamPaths, SlotPath};
pub use spec::PluginSpec;
