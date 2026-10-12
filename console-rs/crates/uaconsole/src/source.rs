//! Where the mixer model comes from. Everything above this (window, input, overlays) only sees
//! `Box<dyn MixerModel>`.

use std::path::PathBuf;

use rack::{MixerModel, PreviewMixer};

use crate::cli::EngineAddr;

#[derive(Clone, Debug, PartialEq)]
pub enum ModelSource {
    /// `rack::PreviewMixer`: offline data reproducing Console's default session.
    Preview,
    /// A recorded engine snapshot (`ua-engine` snapshot JSON).
    Mock(PathBuf),
    /// A live UA Mixer Engine (open-apollo daemon or UA's). Read-only unless `writes`.
    Engine { addr: EngineAddr, writes: bool },
}

pub struct OpenedModel {
    pub model: Box<dyn MixerModel>,
    /// The model's meters move on their own: redraw at 30 Hz.
    pub live_meters: bool,
    /// Device or session description for the title bar.
    pub label: String,
}

impl ModelSource {
    /// Opens the model. Callers fall back to [`ModelSource::Preview`] on error.
    pub fn open(&self) -> Result<OpenedModel, String> {
        Ok(match self {
            ModelSource::Preview => preview("preview"),
            ModelSource::Mock(path) => {
                // TODO(engine-model): one line —
                //   return Ok(OpenedModel { model: Box::new(engine_model::EngineMixer::from_snapshot(path).map_err(|e| e.to_string())?), live_meters: false, label: file_name(path) });
                eprintln!(
                    "uaconsole: --mock {}: snapshot models need the engine-model crate; showing preview data",
                    path.display()
                );
                preview(&format!("mock {} (preview data)", file_name(path)))
            }
            ModelSource::Engine { addr, writes } => {
                // TODO(engine-model): one line —
                //   return Ok(OpenedModel { model: Box::new(engine_model::EngineMixer::connect(&addr.host, addr.port, *writes).map_err(|e| e.to_string())?), live_meters: true, label: addr.to_string() });
                // Until then nothing connects to the engine (and nothing can write to it).
                let _ = writes;
                eprintln!(
                    "uaconsole: engine {addr}: the engine-model crate is not wired in yet; showing preview data"
                );
                preview(&format!("{addr} (preview data)"))
            }
        })
    }
}

pub fn preview(label: &str) -> OpenedModel {
    OpenedModel {
        model: Box::new(PreviewMixer::default()),
        live_meters: false,
        label: label.to_string(),
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
