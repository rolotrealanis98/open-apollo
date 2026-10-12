//! # ua-engine
//!
//! Protocol and model for the UA Mixer Engine on TCP 4710 (NUL-framed JSON text), shared by the
//! open-apollo daemon and UA's own engine. A port of ConsoleLab's Swift `UAEngineKit`.
//!
//! ```no_run
//! use ua_engine::{Endpoint, Options, Store, tapers};
//!
//! let mut store = Store::new();
//! store.connect(Endpoint::parse("127.0.0.1:4710"), Options::default()); // read-only
//! loop {
//!     if store.drain() {                                  // once per frame, on the UI thread
//!         let strip = &store.topology().inputs[0];
//!         let db = store.value(&strip.fader_db()).and_then(|v| v.as_f64()).unwrap_or(-144.0);
//!         println!("{} {}", strip.name, tapers::format_db(db));
//!         let meter = store.meters().reading(&strip.meters[0].base); // lock-free atomics
//!         let _ = meter.level_db;
//!     }
//!     # break;
//! }
//! // Writes are gated twice: Options::allow_writes on the connection, and the writer gate.
//! store.set_writes_enabled(true);
//! let fader = store.topology().inputs[0].fader();
//! store.begin_gesture();                 // set /UndoRecording/value true
//! store.set(&fader, 0.5);                // local value moves now; wire coalesced at ~60 Hz
//! store.end_gesture();                   // flush, then /UndoRecording/value false
//! ```
//!
//! **Safety.** The connection refuses `set`/`post` unless [`Options::allow_writes`]; the
//! [`Writer`] gate (`writes_enabled`) is off by default too.

pub mod command;
pub mod connection;
#[cfg(feature = "mdns")]
pub mod discovery;
pub mod error;
pub mod framing;
pub mod message;
pub mod meters;
pub mod mock;
pub mod path;
pub mod store;
pub mod subscription;
pub mod tapers;
pub mod topology;
pub mod tree;
pub mod value;
pub mod writer;

pub use command::Command;
pub use connection::{
    CommandSink, ConnectionState, Counters, DEFAULT_PORT, Endpoint, Engine, EngineHandle, Options,
    PendingReply, Update, random_network_id,
};
pub use error::{Error, Result};
pub use framing::NulFramer;
pub use message::Message;
pub use meters::{MeterHandle, MeterProperty, MeterReading, MeterStats, MeterStore};
pub use mock::{MockEngine, MockOptions};
pub use store::{Changes, Status, Store};
pub use subscription::{Batch, SubscriptionPlan};
pub use topology::{
    AuxStrip, CueOutput, HeadphoneOutput, InputKind, InputStrip, InsertPaths, MeterPaths,
    MonitorOutput, PluginParamPaths, PluginParameter, PreampPaths, SendPaths, TalkbackPaths,
    Topology,
};
pub use tree::{Meta, Node, Property, Snapshot, SnapshotInfo, Tree};
pub use value::{Json, Value};
pub use writer::{UNDO_RECORDING_PATH, Writer};

/// The Bonjour service type UA Mixer Engines (and open-apollo daemons) advertise.
pub const SERVICE_TYPE: &str = "_uamixer._tcp";

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
