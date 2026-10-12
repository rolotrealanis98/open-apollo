//! GetCorrectionImage is standard base64 SVG (Rack handler 0x1811685c0,
//! decoder 0x181c02a30, HTML/SVG wrapper 0x18116b9e0), not numeric curve samples.
use crate::EngineMixer;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::time::{Duration, Instant};
use ua_engine::{PendingReply, Status};

pub(crate) const SLOTS: [&str; 3] = ["speaker", "headphone", "headphone2"];
#[derive(Default)]
struct Slot {
    key: String,
    svg: Option<String>,
    pending: Option<PendingReply>,
    done: bool,
    next: Option<Instant>,
}
#[derive(Default)]
pub(crate) struct CorrectionState {
    slots: [Slot; 3],
}

fn decode(data: &ua_engine::Json) -> ua_engine::Result<Option<String>> {
    let text = data
        .get("image")
        .and_then(ua_engine::Json::as_str)
        .ok_or_else(|| ua_engine::Error::Other("correction reply has no image".into()))?;
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > 700 * 1024 {
        return Err(ua_engine::Error::Other("correction image too large".into()));
    }
    let bytes = STANDARD
        .decode(text)
        .map_err(|_| ua_engine::Error::Other("invalid correction base64".into()))?;
    if bytes.len() > 512 * 1024 {
        return Err(ua_engine::Error::Other("correction SVG too large".into()));
    }
    let svg = String::from_utf8(bytes)
        .map_err(|_| ua_engine::Error::Other("invalid correction UTF-8".into()))?;
    if !svg.trim_start().starts_with('<') {
        return Err(ua_engine::Error::Other(
            "correction image is not XML".into(),
        ));
    }
    Ok(Some(svg))
}

impl EngineMixer {
    pub(crate) fn correction_deadline(&self) -> Option<Instant> {
        self.correction.slots.iter().filter_map(|s| s.next).min()
    }
    pub(crate) fn correction_svg(&self, device: usize, slot: &str) -> Option<&str> {
        // RPC has no device argument. Do not display the primary unit's image on another unit.
        if device != self.store.topology().device {
            return None;
        }
        self.correction
            .slots
            .get(SLOTS.iter().position(|s| *s == slot)?)?
            .svg
            .as_deref()
    }
    pub(crate) fn poll_correction(&mut self) -> bool {
        if *self.store.status() != Status::Live {
            self.correction = CorrectionState::default();
            return false;
        }
        let Some(handle) = self.store.connection() else {
            return false;
        };
        let generation = handle.generation();
        let device = self.store.topology().device;
        let now = Instant::now();
        let mut changed = false;
        for (i, name) in SLOTS.iter().enumerate() {
            let support = match i {
                0 => "SupportsSpeakerCorrection",
                1 => "SupportsHeadphoneCorrection",
                _ => "SupportsHeadphone2Correction",
            };
            let supported = self.bool_at(&format!("/devices/{device}/{support}")) == Some(true);
            let count = if i == 0 {
                "SpeakerCorrectionImageUpdateCount"
            } else {
                "HeadphoneCorrectionImageUpdateCount"
            };
            let count = self
                .store
                .value(&format!("/devices/{device}/{count}"))
                .map(|v| format!("{v:?}"))
                .unwrap_or_default();
            let s = &mut self.correction.slots[i];
            if !supported {
                changed |= s.svg.is_some();
                *s = Slot::default();
                continue;
            }
            let key = format!("{generation}:{device}:{count}");
            if s.key != key {
                changed |= s.svg.is_some();
                *s = Slot {
                    key,
                    ..Slot::default()
                };
            }
            if let Some(reply) = s.pending.as_ref().and_then(PendingReply::try_take) {
                s.pending = None;
                let result = reply.and_then(|m| decode(&m.data.unwrap_or(ua_engine::Json::Null)));
                match result {
                    Ok(svg) => {
                        s.svg = svg;
                        s.done = true;
                        s.next = None;
                        changed = true;
                    }
                    Err(e) => {
                        log::debug!("correction {name}: {e}");
                        s.next = Some(now + Duration::from_secs(1));
                    }
                }
            }
            if !s.done && s.pending.is_none() && s.next.is_none_or(|t| t <= now) {
                s.pending = handle.request_correction_image(name).ok();
                s.next = Some(now + Duration::from_millis(50));
            } else if s.pending.is_some() && s.next.is_some_and(|t| t <= now) {
                s.next = Some(now + Duration::from_millis(50));
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_base64_svg_and_empty_profile() {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0L1 1\"/></svg>";
        assert_eq!(
            decode(&serde_json::json!({"image":STANDARD.encode(svg)}))
                .unwrap()
                .as_deref(),
            Some(svg)
        );
        assert_eq!(decode(&serde_json::json!({"image":""})).unwrap(), None);
        for data in [
            serde_json::json!({"image":"1..."}),
            serde_json::json!({"image":STANDARD.encode([255])}),
            serde_json::json!({"image":STANDARD.encode("samples:0,1")}),
            serde_json::json!({}),
        ] {
            assert!(decode(&data).is_err());
        }
    }
    #[test]
    fn loopback_image_is_cached_and_counter_refreshes_without_writes() {
        let tree=ua_engine::Snapshot::from_json(&serde_json::json!({"children":{"devices":{"children":{"0":{"properties":{
            "DeviceName":{"value":"Fixture"},"SupportsSpeakerCorrection":{"value":true},"SpeakerCorrectionImageUpdateCount":{"value":0}
        }}}}}})).unwrap().tree;
        let server = ua_engine::MockEngine::start(tree, ua_engine::MockOptions::default()).unwrap();
        let first = "<svg xmlns=\"http://www.w3.org/2000/svg\"><path id=\"first\"/></svg>";
        server.set_read_rpc_reply(
            "/GetCorrectionImage",
            serde_json::json!({"image":STANDARD.encode(first)}),
        );
        let mut m = EngineMixer::connect(server.endpoint(), false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while m.correction_svg(0, "speaker") != Some(first) {
            assert!(Instant::now() < deadline);
            m.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(m.correction_svg(7, "speaker").is_none());
        for _ in 0..30 {
            m.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            server
                .received_commands()
                .iter()
                .filter(|s| s.starts_with("post /GetCorrectionImage"))
                .count(),
            1
        );
        let second = first.replace("first", "second");
        server.set_read_rpc_reply(
            "/GetCorrectionImage",
            serde_json::json!({"image":STANDARD.encode(&second)}),
        );
        server.push("/devices/0/SpeakerCorrectionImageUpdateCount", 1);
        while m.correction_svg(0, "speaker") != Some(second.as_str()) {
            assert!(Instant::now() < deadline);
            m.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            server
                .write_commands()
                .iter()
                .all(|s| s.starts_with("post /GetCorrectionImage"))
        );
    }
}
