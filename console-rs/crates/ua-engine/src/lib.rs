//! # ua-engine
//!
//! Protocol and model for the UA Mixer Engine on TCP 4710 (NUL-framed JSON text), shared by the
//! open-apollo daemon and UA's own engine. A port of ConsoleLab's Swift `UAEngineKit`.

pub mod command;
pub mod error;
pub mod framing;
pub mod message;
pub mod path;
pub mod subscription;
pub mod tapers;
pub mod topology;
pub mod tree;
pub mod value;

pub use command::Command;
pub use error::{Error, Result};
pub use framing::NulFramer;
pub use message::Message;
pub use subscription::{Batch, SubscriptionPlan};
pub use topology::{
    AuxStrip, CueOutput, HeadphoneOutput, InputKind, InputStrip, InsertPaths, MeterPaths,
    MonitorOutput, PluginParamPaths, PluginParameter, PreampPaths, SendPaths, TalkbackPaths,
    Topology,
};
pub use tree::{Meta, Node, Property, Snapshot, SnapshotInfo, Tree};
pub use value::{Json, Value};
