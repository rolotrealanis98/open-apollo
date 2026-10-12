//! UA Mixer Engine text framing on TCP 4710: every message is UTF-8 terminated by a NUL byte.
//!
//! Console stays in this text mode on 4710 for the whole session (it never sends
//! `post command_format 2`, which switches the 4720 helper sockets to UBJSON).

#[derive(Debug, Default)]
pub struct NulFramer {
    buffer: Vec<u8>,
    /// Bytes at the front of `buffer` already searched for a NUL, so a 1.7 MB tree arriving in
    /// 64 KB chunks is scanned once rather than once per chunk.
    scanned: usize,
}

impl NulFramer {
    pub fn new() -> NulFramer {
        NulFramer::default()
    }

    /// Bytes held waiting for a terminator.
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// Appends received bytes and returns every complete frame (without its NUL). Empty frames
    /// (back-to-back NULs) are dropped.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut start = 0;
        let mut cursor = self.scanned;
        while let Some(off) = self.buffer[cursor..].iter().position(|&b| b == 0) {
            let end = cursor + off;
            if end > start {
                frames.push(self.buffer[start..end].to_vec());
            }
            start = end + 1;
            cursor = start;
        }
        if start > 0 {
            self.buffer.drain(..start);
        }
        self.scanned = self.buffer.len();
        frames
    }

    /// Like [`NulFramer::push`], decoding frames as UTF-8 (lossy).
    pub fn push_text(&mut self, bytes: &[u8]) -> Vec<String> {
        self.push(bytes)
            .into_iter()
            .map(|f| String::from_utf8_lossy(&f).into_owned())
            .collect()
    }

    /// `text` + NUL.
    pub fn encode(text: &str) -> Vec<u8> {
        let mut v = Vec::with_capacity(text.len() + 1);
        v.extend_from_slice(text.as_bytes());
        v.push(0);
        v
    }
}
