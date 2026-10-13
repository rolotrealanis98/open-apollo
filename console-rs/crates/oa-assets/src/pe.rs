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

/// The `FileVersion` string of a PE's version resource, e.g. "11.8.3 12-03-2025 06:03".
/// UA's plug-ins put a build date there that the fixed version numbers lack.
pub fn string_file_version(bytes: &[u8]) -> Option<String> {
    let leaves = crate::plugins::pe::resources(bytes)?;
    // RT_VERSION is resource type 16.
    let leaf = leaves.iter().find(|l| l.ty == "16")?;
    string_value(bytes.get(leaf.offset..leaf.offset + leaf.size)?, "FileVersion")
}

/// The value of the `String` entry `key` in a `VS_VERSIONINFO` block:
/// `wLength, wValueLength (in WCHARs), wType, szKey, padding to 4, value`.
fn string_value(block: &[u8], key: &str) -> Option<String> {
    let mut pat: Vec<u8> = key.encode_utf16().flat_map(u16::to_le_bytes).collect();
    pat.extend_from_slice(&[0, 0]);
    let mut from = 0;
    while let Some(hit) = block.get(from..)?.windows(pat.len()).position(|w| w == pat.as_slice()) {
        let at = from + hit;
        from = at + 2;
        // The entry starts 6 bytes before its key, on a 4-byte boundary.
        let Some(start) = at.checked_sub(6).filter(|s| s % 4 == 0) else { continue };
        let chars = u16::from_le_bytes([block[start + 2], block[start + 3]]) as usize;
        let value = start + (6 + pat.len()).div_ceil(4) * 4;
        let units: Vec<u16> = (0..chars)
            .map_while(|k| block.get(value + 2 * k..value + 2 * k + 2).map(|b| u16::from_le_bytes([b[0], b[1]])))
            .take_while(|&u| u != 0)
            .collect();
        let s = String::from_utf16_lossy(&units).trim().to_string();
        return (!s.is_empty()).then_some(s);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `String` entry of a version block: header, key, padding, value.
    fn string_entry(key: &str, value: &str) -> Vec<u8> {
        let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect() };
        let (k, v) = (utf16(key), utf16(value));
        let mut e = vec![0u8; 6];
        e[2..4].copy_from_slice(&((v.len() / 2) as u16).to_le_bytes());
        e[4..6].copy_from_slice(&1u16.to_le_bytes());
        e.extend_from_slice(&k);
        while !e.len().is_multiple_of(4) {
            e.push(0);
        }
        e.extend_from_slice(&v);
        while !e.len().is_multiple_of(4) {
            e.push(0);
        }
        let n = e.len() as u16;
        e[0..2].copy_from_slice(&n.to_le_bytes());
        e
    }

    #[test]
    fn reads_string_values_of_a_version_block() {
        let mut block = b"\0\0\0\0StringFileInfo junk".to_vec();
        while !block.len().is_multiple_of(4) {
            block.push(0);
        }
        block.extend(string_entry("CompanyName", "Synthetic Co"));
        block.extend(string_entry("FileVersion", "11.8.3 12-03-2025 06:03 "));
        assert_eq!(string_value(&block, "FileVersion").as_deref(), Some("11.8.3 12-03-2025 06:03"));
        assert_eq!(string_value(&block, "CompanyName").as_deref(), Some("Synthetic Co"));
        assert_eq!(string_value(&block, "ProductVersion"), None);
        assert_eq!(string_value(&string_entry("FileVersion", "  "), "FileVersion"), None);
    }

    #[test]
    fn finds_the_version_resource_of_a_pe() {
        let entry = string_entry("FileVersion", "1.2.3 test");
        let mut pe = crate::plugins::pe::tests::tiny_pe_with(b"png", &entry);
        // tiny_pe files these bytes as RCDATA (type 10); a real DLL uses RT_VERSION (16).
        assert_eq!(string_file_version(&pe), None);
        // The root directory's id entry is at 0x200 + 24.
        assert_eq!(pe[0x218..0x21c], 10u32.to_le_bytes());
        pe[0x218..0x21c].copy_from_slice(&16u32.to_le_bytes());
        assert_eq!(string_file_version(&pe).as_deref(), Some("1.2.3 test"));
    }

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
