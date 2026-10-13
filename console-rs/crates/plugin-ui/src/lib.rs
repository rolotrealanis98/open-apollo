//! Plug-in windows for console-rs: tapping a loaded insert opens that plug-in's UI and drives its
//! parameters through the UA Mixer Engine.
//!
//! - [`spec`] / [`library`] / [`recipes`]: panel specs (`PluginSpec_*.json`; schema 2 names its
//!   art by recipe id and the importer regenerates it from the user's own install into the art
//!   root, see [`SpecLocator`] and [`ArtLocator`]), never bundled.
//! - [`widgets`]: the schema-2 code-drawn elements (shell toolbars, shapes, lamps).
//! - [`PluginPanel`]: a spec drawn into a `rack::Ui` with hit regions
//!   (`rack::Control::PluginParam/PluginElement/PluginStep`) and its own pointer gestures; a port
//!   of the Swift spec runtime. [`graph`] evaluates GraphValues and builds graph scenes,
//!   [`raster`] draws them (and rotated sprites), [`readout`] formats readouts.
//! - [`GenericPanel`]: a parameter list for plug-ins without a spec.
//! - [`ParamSource`]: where values come from; [`MockParams`] for renders and tests. The engine
//!   adapter maps it onto [`paths`].
//! - [`meters`]: VendorSpecific meter queries, decoding, calibration and ballistics.
//! - [`web`]: UA's own HTML/JS plug-in GUIs (lunacomponents): discovery and the URL to load;
//!   [`content`]: the `ua-content://` scheme Console 1.3.1 serves them on; [`webview`] (feature
//!   `webview`) hosts them in WebKitGTK.

#[cfg(feature = "bridge")]
pub mod bridge;
pub mod content;
pub mod engine;
pub mod generic;
pub mod graph;
pub mod library;
pub mod meters;
pub mod panel;
pub mod params;
pub mod paths;
pub mod raster;
pub mod readout;
pub mod recipes;
pub mod scales;
pub mod spec;
pub mod uanet;
pub mod web;
pub mod widgets;
pub mod window;
#[cfg(all(feature = "webview", target_os = "linux"))]
pub mod webview;

pub use generic::GenericPanel;
pub use library::{ArtLocator, Availability, Entry, PluginSpecLibrary, SpecLocator, TextValue};
pub use recipes::{RecipeStatus, Recipes};
pub use meters::MeterFeed;
pub use panel::PluginPanel;
pub use engine::{EngineSlot, MeterPoller};
pub use scales::PluginScales;
pub use params::{MockEvent, MockParam, MockParams, ParamSnapshot, ParamSource};
pub use paths::{ParamPaths, SlotPath};
pub use spec::PluginSpec;
pub use content::{ContentResponse, ContentRoots};
pub use web::{GuiQuery, LunaComponent, LunaLibrary, LunaLocator};
pub use window::{ChromeState, Content, PluginWindow, WindowEvent, WindowLayout};
