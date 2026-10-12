//! Incoming MIDI. Native buffer positions/timestamps have unresolved time units;
//! tap tempo uses host receipt time, and does not invent timing inside a batch.
use crate::EngineMixer;
use rack::settings::PropValue;
use std::time::{Duration, Instant};
use ua_engine::{Json, PendingReply, Status};

#[derive(Default)]
pub(crate) struct MidiState {
    selected: String,
    generation: u64,
    watermark: Option<i64>,
    pending: Option<PendingReply>,
    next: Option<Instant>,
    taps: rack::menu::TapTempo,
}

impl EngineMixer {
    pub(crate) fn midi_deadline(&self) -> Option<Instant> {
        self.midi.next
    }

    pub(crate) fn poll_midi(&mut self) -> bool {
        let now = Instant::now();
        let selected = self.str_at("/MidiInputDevice").unwrap_or("").to_owned();
        if selected.is_empty() || *self.store.status() != Status::Live {
            self.midi = MidiState::default();
            return false;
        }
        let generation = self.store.connection().map_or(0, |h| h.generation());
        if selected != self.midi.selected || generation != self.midi.generation {
            self.midi = MidiState {
                selected,
                generation,
                ..MidiState::default()
            };
        }
        let mut changed = false;
        if let Some(result) = self.midi.pending.as_ref().and_then(PendingReply::try_take) {
            self.midi.pending = None;
            self.midi.next = Some(now + Duration::from_millis(50));
            match result {
                Ok(message) => {
                    if let Some(data) = message.data {
                        match self.apply_midi_reply(&data, now) {
                            Ok(did) => changed = did,
                            Err(e) => {
                                log::warn!("MIDI reply rejected: {e}");
                                self.midi.next = Some(now + Duration::from_secs(1));
                            }
                        }
                    }
                }
                Err(e) => {
                    log::debug!("MIDI polling: {e}");
                    self.midi.next = Some(now + Duration::from_secs(1));
                }
            }
        }
        if self.midi.pending.is_none() && self.midi.next.is_none_or(|t| t <= now) {
            if let Some(h) = self.store.connection() {
                self.midi.pending = Some(h.request_midi_buffers());
                self.midi.next = Some(now + Duration::from_millis(50));
            }
        } else if self.midi.pending.is_some() && self.midi.next.is_some_and(|t| t <= now) {
            self.midi.next = Some(now + Duration::from_millis(50));
        }
        changed
    }

    fn pref_text(&self, key: &str) -> String {
        match self.prefs.get(key) {
            Some(PropValue::Str(s)) => s,
            _ => String::new(),
        }
    }

    /// Rust policy: Program Change's MIDI program number is the zero-based scene index
    /// (native offset still needs traffic confirmation). Only an
    /// existing scene can be recalled. NOTE-to-scene mapping is deliberately pending
    /// native provenance. Every resulting command uses the ordinary writer gate.
    fn apply_midi_reply(&mut self, data: &Json, received: Instant) -> ua_engine::Result<bool> {
        let buffers = ua_engine::midi::decode_buffers(data)?;
        let tap_ch = self.pref_text("tap_tempo_channel").parse::<u8>().ok();
        let tap_no = self.pref_text("tap_tempo_number").parse::<u8>().ok();
        let tap_event = self.pref_text("tap_tempo_event");
        let scene_ch = self.pref_text("scenes_channel").parse::<u8>().ok();
        let scene_event = self.pref_text("scenes_event");
        let mut tapped = false;
        let mut changed = false;
        for buffer in buffers {
            if self.midi.watermark.is_some_and(|t| buffer.time_stamp <= t) {
                continue;
            }
            self.midi.watermark = Some(buffer.time_stamp);
            for event in buffer.events {
                let b = event.bytes;
                let Some(status) = b.first().copied() else {
                    continue;
                };
                let channel = Some((status & 15) + 1);
                let kind = status & 0xf0;
                // Only canonical complete channel messages; no running-status guessing.
                if channel == tap_ch
                    && b.len() == 3
                    && b[1] < 128
                    && b[2] > 0
                    && b[2] < 128
                    && Some(b[1]) == tap_no
                    && ((kind == 0x90 && tap_event == "NOTE")
                        || (kind == 0xb0 && tap_event == "CONTROLLER"))
                    && !tapped
                {
                    tapped = true;
                    if let Some(bpm) = self.midi.taps.tap(received) {
                        self.store.commit("/Tempo", bpm.clamp(1.0, 999.0));
                        changed = true;
                    }
                }
                if kind == 0xc0
                    && channel == scene_ch
                    && scene_event == "PROGRAM CHANGE"
                    && b.len() == 2
                    && b[1] < 128
                    && self
                        .store
                        .tree()
                        .indices("/pluginscenes/0/scenes")
                        .contains(&(b[1] as usize))
                {
                    self.scene_operation(rack::menu::SceneOp::Recall, b[1] as u16);
                    changed = true;
                }
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ua_engine::{Snapshot, Store};
    fn buffer(time: i64, bytes: &[u8]) -> Json {
        let mut raw = vec![0, 0, 0, 0];
        raw.extend((bytes.len() as u16).to_le_bytes());
        raw.extend(bytes);
        let alphabet = b".ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+";
        let mut text = format!("{}.", raw.len());
        for i in 0..(raw.len() * 8).div_ceil(6) {
            let mut n = 0;
            for bit in 0..6 {
                let k = i * 6 + bit;
                if k < raw.len() * 8 {
                    n |= ((raw[k / 8] >> (k % 8)) & 1) << bit;
                }
            }
            text.push(alphabet[n as usize] as char);
        }
        serde_json::json!([{"time_stamp":time,"midi_buffer":text}])
    }
    fn mixer() -> EngineMixer {
        EngineMixer::new(Store::from_snapshot(Snapshot::from_json(&serde_json::json!({
            "properties":{"Tempo":{"value":100.0},"MidiInputDevice":{"value":""}},
            "children":{"pluginscenes":{"children":{"0":{"children":{"scenes":{"children":{"7":{"properties":{"SceneName":{"value":"Fixture"}}}}}}}}}}
        })).unwrap()))
    }
    #[test]
    fn tap_filters_duplicates_release_and_other_channel_then_calculates_tempo() {
        let mut m = mixer();
        let t = Instant::now();
        assert!(!m.apply_midi_reply(&buffer(1, &[0x91, 64, 100]), t).unwrap());
        assert!(!m.apply_midi_reply(&buffer(2, &[0x90, 64, 0]), t).unwrap());
        for i in 0..4 {
            let data = buffer(i + 3, &[0x90, 64, 100]);
            assert_eq!(
                m.apply_midi_reply(&data, t + Duration::from_millis(i as u64 * 500))
                    .unwrap(),
                i == 3
            );
            assert!(!m.apply_midi_reply(&data, t).unwrap());
        }
        assert_eq!(m.f64_at("/Tempo"), Some(120.0));
        // Malformed later entry rejects the batch before applying an earlier tap.
        let mut data = buffer(20, &[0x90, 64, 100]);
        data.as_array_mut()
            .unwrap()
            .push(serde_json::json!({"time_stamp":21,"midi_buffer":"bad"}));
        assert!(m.apply_midi_reply(&data, t).is_err());
        assert_eq!(m.midi.watermark, Some(6));
    }

    #[test]
    fn scenes_filter_and_writer_gate() {
        let mut m = mixer();
        let t = Instant::now();
        assert!(!m.apply_midi_reply(&buffer(1, &[0xc1, 7]), t).unwrap());
        assert!(!m.apply_midi_reply(&buffer(2, &[0xc0, 6]), t).unwrap());
        assert!(m.apply_midi_reply(&buffer(3, &[0xc0, 7]), t).unwrap());
        assert_eq!(m.store.writer().suppressed_count(), 1);
        assert_eq!(
            m.store.writer().recent_suppressed().last(),
            Some("post /SceneOperation {\"RecallScene\":7}")
        );
        assert!(!m.apply_midi_reply(&buffer(3, &[0xc0, 7]), t).unwrap());
        assert_eq!(m.store.writer().suppressed_count(), 1);
    }

    #[test]
    fn loopback_polling_correlates_read_rpc_and_preserves_write_gate() {
        for writable in [false, true] {
            let tree = mixer().store.tree().clone();
            let server =
                ua_engine::MockEngine::start(tree, ua_engine::MockOptions::default()).unwrap();
            server.set_read_rpc_reply("/GetMidiBuffers", buffer(1, &[0xc0, 7]));
            let mut m = EngineMixer::connect(server.endpoint(), writable);
            let deadline = Instant::now() + Duration::from_secs(10);
            while *m.store.status() != Status::Live {
                assert!(Instant::now() < deadline);
                m.pump();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                !server
                    .received_commands()
                    .iter()
                    .any(|s| s.contains("GetMidiBuffers"))
            );
            server.push("/MidiInputDevice", "Synthetic MIDI");
            while m.midi.watermark != Some(1) {
                assert!(Instant::now() < deadline);
                m.pump();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                server
                    .received_commands()
                    .iter()
                    .any(|s| s.starts_with("post /GetMidiBuffers?func_id=") && s.ends_with(" {}"))
            );
            if writable {
                while !server
                    .write_commands()
                    .iter()
                    .any(|s| s.starts_with("post /SceneOperation"))
                {
                    assert!(Instant::now() < deadline);
                    m.pump();
                    std::thread::sleep(Duration::from_millis(5));
                }
            } else {
                assert!(
                    !server
                        .write_commands()
                        .iter()
                        .any(|s| s.starts_with("post /SceneOperation"))
                );
                assert!(matches!(
                    m.store
                        .connection()
                        .unwrap()
                        .send(ua_engine::Command::post("/SceneOperation", None)),
                    Err(ua_engine::Error::WriteRefused)
                ));
            }
            for _ in 0..25 {
                m.pump();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                server
                    .write_commands()
                    .iter()
                    .filter(|s| s.starts_with("post /SceneOperation"))
                    .count(),
                usize::from(writable)
            );
            server.set_read_rpc_reply(
                "/GetCorrectionImage",
                serde_json::json!({"image":"synthetic"}),
            );
            let h = m.store.connection().unwrap();
            assert!(h.request_correction_image("invalid").is_err());
            let r = h
                .request_correction_image("speaker")
                .unwrap()
                .wait(Duration::from_secs(2))
                .unwrap();
            assert_eq!(r.data.unwrap()["image"], "synthetic");
        }
    }
}
