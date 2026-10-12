//! Console's font model: JUCE font heights and the metrics of the faces Console names.
//!
//! Rack.dll builds every skin font as `juce::Font(name, height, style)` (0x181c93970), where the
//! JUCE height is the face's ascent + descent, not its point size. A renderer that sizes text by
//! the em (CoreText, swash, cosmic-text) draws `height / (ascent + descent)` points; for Helvetica
//! the two agree, for Helvetica Neue bold a height of 12 is a 10.07-pt em.

/// Console's `font_size_adjust` preference at its default: fonts marked `font-size-dyn` are drawn
/// this much taller (Rack.dll size getter 0x181c4d4d0: `size += adjust`, then capped at
/// `font-size-max`). The default is 1 on macOS (Rack.framework 0x20963e4) and 2 on Windows
/// (0x184c34b50); the reference screenshots are macOS.
pub const FONT_SIZE_ADJUST: f32 = 1.0;

/// Ascent and descent, as fractions of the em, of the faces Console names: CoreText's
/// CTFontGetAscent/Descent on macOS 27 for the system faces, `hhea` for the fonts Console bundles
/// in `Resources/fonts` (Oxygen). Linux substitutes (Nimbus Sans, Liberation Sans) are laid out
/// with these, so text sits where Console puts it.
pub fn face_metrics(family: &str, bold: bool, italic: bool) -> Option<(f32, f32)> {
    Some(match (family, bold) {
        ("Helvetica", _) => (0.770, 0.230),
        ("Helvetica Neue", true) => (0.975, 0.217),
        ("Helvetica Neue", false) => (0.952, 0.213),
        ("Arial", _) => (0.905, 0.212),
        ("Lucida Grande", _) => (0.967, 0.211),
        ("Oxygen", true) if italic => (0.88, 0.10),
        ("Oxygen", true) => (0.88, 0.12),
        ("Oxygen", false) => (0.90, 0.10),
        _ => return None,
    })
}

/// Em size (points) of a JUCE font `height` in `family`. Unknown faces count as ascent + descent = 1.
pub fn em_for_height(family: &str, bold: bool, italic: bool, height: f32) -> f32 {
    match face_metrics(family, bold, italic) {
        Some((a, d)) => height / (a + d),
        None => height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn juce_heights_to_em() {
        assert_eq!(em_for_height("Helvetica", true, false, 11.0), 11.0);
        // Channel-Strip-Section-Header: Helvetica Neue 11 bold, dyn → height 12 → 10.07 pt.
        assert!((em_for_height("Helvetica Neue", true, false, 12.0) - 10.067).abs() < 0.01);
        // Switch: Arial 9 bold, dyn → height 10 → 8.95 pt (measured 9 in Console).
        assert!((em_for_height("Arial", true, false, 10.0) - 8.95).abs() < 0.01);
        assert_eq!(em_for_height("Some Font", false, false, 7.0), 7.0);
    }

    #[test]
    fn skin_font_dyn_and_max() {
        let dir = std::env::temp_dir().join(format!("houston-fonts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Appearance.json");
        std::fs::write(
            &file,
            r#"{"fonts": {
                "Header": {"font": "Helvetica Neue", "font-style": "bold", "font-size": 11, "font-size-dyn": true},
                "Capped": {"font": "Helvetica", "font-size": 9, "font-size-dyn": true, "font-size-max": 9.5, "font-kerning": -0.02},
                "Fixed": {"font": "Arial", "font-size": 9}
            }}"#,
        )
        .unwrap();
        let a = crate::Appearance::load_files(&[&file]).unwrap();
        let f = a.skin("fonts", "Header").unwrap().font();
        assert_eq!(f.height, 12.0);
        assert!((f.size - 10.067).abs() < 0.01 && f.bold);
        let f = a.skin("fonts", "Capped").unwrap().font();
        assert_eq!((f.height, f.size, f.kerning), (9.5, 9.5, -0.02));
        assert_eq!(a.skin("fonts", "Header").unwrap().font_with_adjust(2.0).height, 13.0);
        let f = a.skin("fonts", "Fixed").unwrap().font();
        assert!((f.height - 9.0).abs() < 1e-6 && (f.size - 9.0 / 1.117).abs() < 0.01);
        std::fs::remove_dir_all(&dir).ok();
    }
}
