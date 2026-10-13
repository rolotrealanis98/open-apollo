//! Menu text widths as Console 1.3.1 on macOS measures them: Helvetica Bold glyph advances
//! plus its pair kerning (the system `Helvetica.ttc`, face 1, 2048 units per em), so JUCE's
//! `getIdealPopupMenuItemSize` width (text width rounded up, plus twice the item height)
//! comes out as in the drive captures (set-d-ctlmode 173, set-d-tapev 132, set-d-scenev 167).
//! Printable ASCII only; other characters fall back to `helvetica_bold_width`.

const UPM: f32 = 2048.0;

/// Advance widths of U+0020..=U+007E, in font units.
const ADVANCE: [u16; 95] = [
    569, 682, 971, 1139, 1139, 1821, 1479, 487, 682, 682, 797, 1196, 569, 682, 569, 569,
    1139, 1139, 1139, 1139, 1139, 1139, 1139, 1139, 1139, 1139, 682, 682, 1196, 1196, 1196, 1251,
    1997, 1479, 1479, 1479, 1479, 1366, 1251, 1593, 1479, 569, 1139, 1479, 1251, 1706, 1479, 1593,
    1366, 1593, 1479, 1366, 1251, 1479, 1366, 1933, 1366, 1366, 1251, 682, 569, 682, 1196, 1139,
    682, 1139, 1251, 1139, 1251, 1139, 682, 1251, 1251, 569, 569, 1139, 569, 1821, 1251, 1251,
    1251, 1251, 797, 1139, 682, 1251, 1139, 1593, 1139, 1139, 1024, 797, 573, 797, 1196,
];

/// Kerning pairs (left, right, font units), sorted for binary search.
const KERN: [(u8, u8, i16); 93] = [
    (b' ', b'A', -75), (b' ', b'Y', -36), (b'1', b'1', -112), (b'A', b' ', -75), (b'A', b'T', -151), (b'A', b'V', -151),
    (b'A', b'W', -112), (b'A', b'Y', -188), (b'A', b'v', -75), (b'A', b'w', -36), (b'A', b'y', -75), (b'F', b',', -227),
    (b'F', b'.', -227), (b'F', b'A', -112), (b'L', b' ', -36), (b'L', b'T', -151), (b'L', b'V', -151), (b'L', b'W', -112),
    (b'L', b'Y', -188), (b'L', b'y', -75), (b'P', b' ', -36), (b'P', b',', -264), (b'P', b'.', -264), (b'P', b'A', -151),
    (b'R', b'V', -36), (b'R', b'W', -36), (b'R', b'Y', -75), (b'T', b',', -227), (b'T', b'-', -112), (b'T', b'.', -227),
    (b'T', b':', -227), (b'T', b';', -227), (b'T', b'A', -151), (b'T', b'O', -36), (b'T', b'a', -151), (b'T', b'c', -151),
    (b'T', b'e', -151), (b'T', b'i', -36), (b'T', b'o', -151), (b'T', b'r', -112), (b'T', b's', -151), (b'T', b'u', -151),
    (b'T', b'w', -151), (b'T', b'y', -151), (b'V', b',', -188), (b'V', b'-', -112), (b'V', b'.', -188), (b'V', b':', -112),
    (b'V', b';', -112), (b'V', b'A', -151), (b'V', b'a', -112), (b'V', b'e', -112), (b'V', b'i', -36), (b'V', b'o', -151),
    (b'V', b'r', -112), (b'V', b'u', -75), (b'V', b'y', -75), (b'W', b',', -112), (b'W', b'-', -40), (b'W', b'.', -112),
    (b'W', b':', -36), (b'W', b';', -36), (b'W', b'A', -112), (b'W', b'a', -75), (b'W', b'e', -36), (b'W', b'i', -18),
    (b'W', b'o', -36), (b'W', b'r', -36), (b'W', b'u', -36), (b'W', b'y', -36), (b'Y', b' ', -36), (b'Y', b',', -227),
    (b'Y', b'-', -112), (b'Y', b'.', -227), (b'Y', b':', -151), (b'Y', b';', -151), (b'Y', b'A', -188), (b'Y', b'a', -112),
    (b'Y', b'e', -112), (b'Y', b'i', -75), (b'Y', b'o', -151), (b'Y', b'p', -112), (b'Y', b'q', -151), (b'Y', b'u', -112),
    (b'Y', b'v', -112), (b'r', b',', -112), (b'r', b'.', -112), (b'v', b',', -151), (b'v', b'.', -151), (b'w', b',', -75),
    (b'w', b'.', -75), (b'y', b',', -151), (b'y', b'.', -151),
];

/// Width of `text` in Helvetica Bold at `size` points, kerned, as macOS lays it out.
pub fn helvetica_bold_kerned_width(text: &str, size: f32) -> f32 {
    let mut units = 0.0f32;
    let mut extra = 0.0f32;
    let mut prev: Option<u8> = None;
    for ch in text.chars() {
        let c = ch as u32;
        if (0x20..=0x7e).contains(&c) {
            let b = c as u8;
            units += ADVANCE[(b - 0x20) as usize] as f32;
            if let Some(p) = prev {
                if let Ok(i) = KERN.binary_search_by(|&(l, r, _)| (l, r).cmp(&(p, b))) {
                    units += KERN[i].2 as f32;
                }
            }
            prev = Some(b);
        } else {
            extra += crate::window::helvetica_bold_width(ch.encode_utf8(&mut [0; 4]), size);
            prev = None;
        }
    }
    units * size / UPM + extra
}
