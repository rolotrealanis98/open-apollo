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
            ModelSource::Mock(path) => OpenedModel {
                model: Box::new(engine_model::EngineMixer::from_snapshot(path).map_err(|e| e.to_string())?),
                live_meters: false,
                label: file_name(path),
            },
            ModelSource::Engine { addr, writes } => OpenedModel {
                // Read-only unless `writes` (both the writer gate and the connection refuse set/post).
                model: Box::new({
                    let mut m = engine_model::EngineMixer::connect(ua_engine::Endpoint::new(addr.host.clone(), addr.port), *writes);
                    // Console's own preferences (OPTIONS, MIDI), next to uaconsole.toml.
                    m.set_prefs_path(crate::settings::Settings::path().map(|p| p.with_file_name("console-prefs.json")));
                    m
                }),
                live_meters: true,
                label: addr.to_string(),
            },
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
