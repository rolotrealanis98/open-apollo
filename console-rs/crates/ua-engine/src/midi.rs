//! Console MIDI wire format, not RFC base64 or a concatenated raw MIDI stream.
//! Provenance: Mac CMxGetMidiBuffers::execute 0x10028c168 serializes a JUCE
//! MemoryBlock; Windows reply 0x180e10810 decodes it into MidiBuffer. Iterator
//! 0x181e003d0 reads little-endian i32 position + u16 length + message bytes.

use crate::{Error, Json, Result};

const ALPHABET: &[u8; 64] = b".ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+";
const MAX_BYTES: usize = 1024 * 1024;
const MAX_BATCH: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiEvent {
    /// Native buffer position. Its time unit is not yet established; do not treat as UTC.
    pub position: i32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiBuffer {
    /// Engine watermark, used only to reject duplicate replies, not as wall-clock time.
    pub time_stamp: i64,
    pub events: Vec<MidiEvent>,
}

fn invalid() -> Error {
    Error::Other("invalid Console MIDI buffer".into())
}

/// JUCE's `length.payload`, with least-significant six-bit groups first.
pub fn decode_memory_block(text: &str) -> Result<Vec<u8>> {
    let (size, payload) = text.split_once('.').ok_or_else(invalid)?;
    if size.is_empty() || !size.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let size: usize = size.parse().map_err(|_| invalid())?;
    if size > MAX_BYTES || payload.len() != (size * 8).div_ceil(6) {
        return Err(invalid());
    }
    let mut out = vec![0u8; size];
    for (i, c) in payload.bytes().enumerate() {
        let value = ALPHABET.iter().position(|b| *b == c).ok_or_else(invalid)? as u8;
        for bit in 0..6 {
            let index = i * 6 + bit;
            if index < size * 8 {
                out[index / 8] |= ((value >> bit) & 1) << (index % 8);
            } else if value >> bit != 0 {
                return Err(invalid());
            }
        }
    }
    Ok(out)
}

pub fn decode_events(bytes: &[u8]) -> Result<Vec<MidiEvent>> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    let mut rest = bytes;
    let mut events = Vec::new();
    while !rest.is_empty() {
        if rest.len() < 6 {
            return Err(invalid());
        }
        let position = i32::from_le_bytes(rest[..4].try_into().unwrap());
        let len = u16::from_le_bytes(rest[4..6].try_into().unwrap()) as usize;
        rest = &rest[6..];
        if len == 0 || rest.len() < len {
            return Err(invalid());
        }
        // Unknown/system messages stay intact; consumers decide which messages to act on.
        events.push(MidiEvent {
            position,
            bytes: rest[..len].to_vec(),
        });
        rest = &rest[len..];
    }
    Ok(events)
}

/// Atomic decoding: a malformed entry rejects the whole reply before any action is taken.
pub fn decode_buffers(data: &Json) -> Result<Vec<MidiBuffer>> {
    let array = data.as_array().ok_or_else(invalid)?;
    if array.len() > MAX_BATCH {
        return Err(invalid());
    }
    let mut total = 0usize;
    array
        .iter()
        .map(|entry| {
            let time_stamp = entry
                .get("time_stamp")
                .and_then(Json::as_i64)
                .ok_or_else(invalid)?;
            let text = entry
                .get("midi_buffer")
                .and_then(Json::as_str)
                .ok_or_else(invalid)?;
            let bytes = decode_memory_block(text)?;
            total = total.checked_add(bytes.len()).ok_or_else(invalid)?;
            if total > MAX_BYTES {
                return Err(invalid());
            }
            Ok(MidiBuffer {
                time_stamp,
                events: decode_events(&bytes)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn juce_vectors_and_rejection() {
        for (s, bytes) in [
            ("0.", vec![]),
            ("1...", vec![0]),
            ("1.+C", vec![255]),
            ("3.AHv.", vec![1, 2, 3]),
        ] {
            assert_eq!(decode_memory_block(s).unwrap(), bytes);
        }
        for s in ["1.A", "1.A=", "1..+", "1048577.", "-1.", "1.é", "1e0..."] {
            assert!(decode_memory_block(s).is_err(), "{s}");
        }
    }

    #[test]
    fn native_structured_events_preserve_position_and_sysex() {
        let bytes = [
            0, 0, 0, 0, 3, 0, 0x90, 64, 100, 8, 0, 0, 0, 2, 0, 0xc1, 7, 9, 0, 0, 0, 3, 0, 0xf0, 1,
            0xf7,
        ];
        let events = decode_events(&bytes).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1],
            MidiEvent {
                position: 8,
                bytes: vec![0xc1, 7]
            }
        );
        assert_eq!(events[2].bytes, [0xf0, 1, 0xf7]);
        for n in [1, 5, 7, 8] {
            assert!(decode_events(&bytes[..n]).is_err());
        }
        assert!(decode_events(&[0; 6]).is_err());
        assert!(decode_buffers(&serde_json::json!([{"time_stamp": 1,"midi_buffer":"0."},{"time_stamp":2,"midi_buffer":"bad"}])).is_err());
    }
}
