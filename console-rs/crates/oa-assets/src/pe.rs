//! Just enough PE reading to get a file version.

use std::path::Path;

/// The `VS_FIXEDFILEINFO` file version of a Windows executable, e.g. "1.2.9.905".
pub fn file_version(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    file_version_of(&bytes)
}

/// Searches for the fixed-file-info signature instead of walking the resource
/// tree; the version block is the only place 0xFEEF04BD appears in practice.
pub fn file_version_of(bytes: &[u8]) -> Option<String> {
    const SIG: [u8; 4] = 0xFEEF_04BDu32.to_le_bytes();
    let at = bytes.windows(4).rposition(|w| w == SIG)?;
    let word = |off: usize| -> Option<u32> {
        let b = bytes.get(at + off..at + off + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (ms, ls) = (word(8)?, word(12)?);
    Some(format!("{}.{}.{}.{}", ms >> 16, ms & 0xffff, ls >> 16, ls & 0xffff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fixed_file_info() {
        let mut b = vec![0u8; 64];
        b.extend_from_slice(&0xFEEF_04BDu32.to_le_bytes());
        b.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // struct version
        b.extend_from_slice(&((1u32 << 16) | 2).to_le_bytes());
        b.extend_from_slice(&((9u32 << 16) | 905).to_le_bytes());
        assert_eq!(file_version_of(&b).as_deref(), Some("1.2.9.905"));
    }

    #[test]
    fn none_without_signature() {
        assert_eq!(file_version_of(&[0u8; 32]), None);
    }
}
