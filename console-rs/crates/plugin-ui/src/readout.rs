//! Readout helpers: Swift's `String(format:)` for one floating conversion, sprite-font cells
//! (LED/nixie digit filmstrips) and the text fitting SwiftUI's `Text` does for a readout
//! (`lineLimit`, `minimumScaleFactor(0.5)`), all pure.

use crate::spec::SpriteFont;

/// The equal-tempered note nearest `hz` with A = `a_hz` ("A", "A#", …); empty unless both are
/// positive frequencies. Exactly ±50 cents rounds away from the reference's note.
pub fn note_name(hz: f64, a_hz: f64) -> &'static str {
    const NAMES: [&str; 12] = ["A", "A#", "B", "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#"];
    let semitones = (12.0 * (hz / a_hz).log2()).round();
    if hz > 0.0 && semitones.is_finite() { NAMES[semitones.rem_euclid(12.0) as usize] } else { "" }
}

/// `String(format: fmt, v)` for formats holding one `%f`/`%F`/`%e`/`%E`/`%g`/`%G` conversion
/// (flags `-+ 0#`, width, precision) plus literal text and `%%` — the formats the pipeline
/// validator admits. Anything else is copied through.
pub fn format_number(fmt: &str, v: f64) -> String {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    let mut used = false;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'%') {
            out.push('%');
            i += 2;
            continue;
        }
        // %[flags][width][.precision]conv
        let start = i;
        i += 1;
        let mut flags = Flags::default();
        while let Some(&f) = chars.get(i) {
            match f {
                '-' => flags.left = true,
                '+' => flags.plus = true,
                ' ' => flags.space = true,
                '0' => flags.zero = true,
                '#' => flags.alt = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
            width = width * 10 + d as usize;
            i += 1;
        }
        let mut precision = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            let mut p = 0usize;
            while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
                p = p * 10 + d as usize;
                i += 1;
            }
            precision = Some(p);
        }
        match chars.get(i) {
            Some(&conv @ ('f' | 'F' | 'e' | 'E' | 'g' | 'G')) if !used => {
                used = true;
                i += 1;
                out.push_str(&pad(convert(v, conv, precision, &flags), width, &flags));
            }
            _ => {
                // Not a conversion this runtime prints: keep the text.
                out.extend(&chars[start..i.min(chars.len())]);
            }
        }
    }
    out
}

#[derive(Default)]
struct Flags {
    left: bool,
    plus: bool,
    space: bool,
    zero: bool,
    alt: bool,
}

fn convert(v: f64, conv: char, precision: Option<usize>, flags: &Flags) -> String {
    let upper = conv.is_ascii_uppercase();
    let sign = if v.is_sign_negative() && !v.is_nan() {
        "-"
    } else if flags.plus {
        "+"
    } else if flags.space {
        " "
    } else {
        ""
    };
    let a = v.abs();
    if !a.is_finite() {
        let s = if a.is_nan() { "nan" } else { "inf" };
        return format!("{sign}{}", if upper { s.to_uppercase() } else { s.into() });
    }
    let body = match conv.to_ascii_lowercase() {
        'f' => {
            let p = precision.unwrap_or(6);
            let mut s = format!("{a:.p$}");
            if flags.alt && p == 0 {
                s.push('.');
            }
            s
        }
        'e' => exp(a, precision.unwrap_or(6), flags.alt),
        _ => {
            let p = match precision {
                Some(0) => 1,
                Some(p) => p,
                None => 6,
            };
            // The exponent after rounding to p significant digits.
            let x = if a == 0.0 { 0 } else { decimal_exponent(a, p - 1) };
            let mut s = if (x as i64) < p as i64 && x >= -4 { format!("{a:.prec$}", prec = (p as i64 - 1 - x as i64) as usize) } else { exp(a, p - 1, flags.alt) };
            if !flags.alt {
                s = strip_zeros(&s);
            }
            s
        }
    };
    let body = if upper { body.to_uppercase() } else { body };
    format!("{sign}{body}")
}

/// `d.ddde±XX`.
fn exp(a: f64, p: usize, alt: bool) -> String {
    let s = format!("{a:.p$e}");
    let (mantissa, e) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = e.parse().unwrap_or(0);
    let mut m = mantissa.to_string();
    if alt && p == 0 {
        m.push('.');
    }
    format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

fn decimal_exponent(a: f64, p: usize) -> i32 {
    let s = format!("{a:.p$e}");
    s.split_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0)
}

/// `%g` without `#`: trailing zeros of the fraction (and a bare point) go.
fn strip_zeros(s: &str) -> String {
    let (num, e) = match s.find('e') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let num = if num.contains('.') { num.trim_end_matches('0').trim_end_matches('.') } else { num };
    format!("{num}{e}")
}

fn pad(s: String, width: usize, flags: &Flags) -> String {
    let n = s.chars().count();
    if n >= width {
        return s;
    }
    let fill = width - n;
    if flags.left {
        format!("{s}{}", " ".repeat(fill))
    } else if flags.zero && !s.ends_with("inf") && !s.ends_with("nan") && !s.ends_with("INF") && !s.ends_with("NAN") {
        let (sign, digits) = match s.chars().next() {
            Some(c @ ('-' | '+' | ' ')) => (c.to_string(), &s[1..]),
            _ => (String::new(), &s[..]),
        };
        format!("{sign}{}{digits}", "0".repeat(fill))
    } else {
        format!("{}{s}", " ".repeat(fill))
    }
}

/// One drawn cell of a sprite-font readout: the filmstrip frame and whether the decimal-point
/// sprite goes over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub frame: u32,
    pub dot: bool,
}

/// Swift's `spriteReadout` cells: each character's frame in the charset (exact, then upper-,
/// then lower-case), "." over the previous cell when the font has a decimal sprite, characters
/// outside the charset dropped, and leading/trailing blank cells (without a dot) trimmed.
pub fn sprite_cells(text: &str, font: &SpriteFont) -> Vec<Cell> {
    let charset: Vec<char> = font.charset.chars().collect();
    let find = |c: char| charset.iter().position(|&x| x == c);
    let mut cells: Vec<Cell> = Vec::new();
    for c in text.chars() {
        if c == '.' && font.decimal_sprite.is_some() {
            if let Some(last) = cells.last_mut() {
                last.dot = true;
            }
            continue;
        }
        let upper = c.to_uppercase().next().unwrap_or(c);
        let lower = c.to_lowercase().next().unwrap_or(c);
        if let Some(i) = find(c).or_else(|| find(upper)).or_else(|| find(lower)) {
            cells.push(Cell { frame: i as u32, dot: false });
        }
    }
    let blank = |c: &Cell| charset.get(c.frame as usize) == Some(&' ') && !c.dot;
    while cells.first().is_some_and(blank) {
        cells.remove(0);
    }
    while cells.last().is_some_and(blank) {
        cells.pop();
    }
    cells
}

/// Estimated advance of `text` in points (SF-like metrics: digits 0.56 em, average 0.54 em,
/// monospaced 0.6 em). Used only to shrink and wrap readouts the way SwiftUI's `Text` does.
pub fn estimate_width(text: &str, size: f32, monospaced: bool, bold: bool) -> f32 {
    let em: f32 = text
        .chars()
        .map(|c| {
            if monospaced {
                return 0.6;
            }
            match c {
                'i' | 'l' | 'j' | '\'' | '|' | '!' | '.' | ',' | ':' | ';' => 0.25,
                'f' | 't' | 'r' | 'I' | ' ' | '(' | ')' | '[' | ']' | '-' => 0.32,
                'm' | 'w' | 'M' | 'W' | '%' | '@' => 0.85,
                '0'..='9' => 0.56,
                c if c.is_uppercase() => 0.66,
                _ => 0.52,
            }
        })
        .sum();
    em * size * if bold { 1.06 } else { 1.0 }
}

/// Greedy word wrap into at most `max_lines` lines (0 = unlimited); the last line keeps the rest.
pub fn wrap(text: &str, width: f32, size: f32, monospaced: bool, bold: bool, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && estimate_width(&candidate, size, monospaced, bold) > width {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    if max_lines > 0 && lines.len() > max_lines {
        let rest = lines.split_off(max_lines - 1).join(" ");
        lines.push(rest);
    }
    lines
}

/// The font size a one-line readout of `text` is drawn at in a box `width` wide: shrunk to fit,
/// down to half (`minimumScaleFactor(0.5)`).
pub fn fitted_size(text: &str, width: f32, size: f32, monospaced: bool, bold: bool) -> f32 {
    let w = estimate_width(text, size, monospaced, bold);
    if w <= width || w <= 0.0 { size } else { (size * width / w).max(size * 0.5) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_names_from_a_period_in_samples() {
        // A tuner meter reporting the period in samples at 48 kHz, as `ratio: [48000, meter]`.
        let note = |period: f64, a: f64| note_name(48000.0 / period, a);
        let cents = |name_hz: f64, c: f64| 48000.0 / (name_hz * 2f64.powf(c / 1200.0));
        assert_eq!(note(48000.0 / 440.0, 440.0), "A");
        assert_eq!(note(48000.0 / 261.6256, 440.0), "C", "middle C");
        assert_eq!(note(48000.0 / 82.4069, 440.0), "E", "low E string");
        assert_eq!(note(48000.0 / 30.8677, 440.0), "B", "5-string bass low B");
        assert_eq!(note(48000.0 / 4186.009, 440.0), "C", "C8");
        // Semitone boundaries: ±49.9 cents stays, ±50.1 cents is the neighbour.
        assert_eq!(note(cents(440.0, 49.9), 440.0), "A");
        assert_eq!(note(cents(440.0, 50.1), 440.0), "A#");
        assert_eq!(note(cents(440.0, -49.9), 440.0), "A");
        assert_eq!(note(cents(440.0, -50.1), 440.0), "G#");
        assert_eq!(note(cents(261.6256, -50.1), 440.0), "B", "wraps below C");
        // Reference 438…442 (`param 2` normalized 0…1 → 438 + 4·n): each one's A is "A", and
        // A440 + 49 cents (452.6 Hz) is A# against 438 and 439 (+56.9, +53.0 cents), A against 440…442.
        for a in [438.0, 439.0, 440.0, 441.0, 442.0] {
            assert_eq!(note(48000.0 / a, a), "A", "reference {a}");
            assert_eq!(note(cents(a, 50.1), a), "A#", "reference {a}");
            assert_eq!(note(cents(a, -50.1), a), "G#", "reference {a}");
        }
        let a49 = cents(440.0, 49.0);
        assert_eq!((note(a49, 438.0), note(a49, 439.0)), ("A#", "A#"));
        for a in [440.0, 441.0, 442.0] {
            assert_eq!(note(a49, a), "A", "reference {a}");
        }
        // No pitch: no name.
        assert_eq!(note_name(0.0, 440.0), "");
        assert_eq!(note_name(-1.0, 440.0), "");
        assert_eq!(note_name(440.0, 0.0), "");
        assert_eq!(note_name(f64::NAN, 440.0), "");
    }

    #[test]
    fn printf_like_swift() {
        assert_eq!(format_number("%.0f", 2.5), "2");
        assert_eq!(format_number("%.0f", 3.5), "4");
        assert_eq!(format_number("%.1f dB", -0.04), "-0.0 dB");
        assert_eq!(format_number("%.2f k", 1.23456), "1.23 k");
        assert_eq!(format_number("%.0f %%", 42.4), "42 %");
        assert_eq!(format_number("Target: %.1f dB", -12.25), "Target: -12.2 dB");
        assert_eq!(format_number("%.3g Hz", 1234.5), "1.23e+03 Hz");
        assert_eq!(format_number("%.3g Hz", 123.45), "123 Hz");
        assert_eq!(format_number("%.3g", 0.5), "0.5");
        assert_eq!(format_number("%.3g", 0.0001234), "0.000123");
        assert_eq!(format_number("%.3g", 0.00001234), "1.23e-05");
        assert_eq!(format_number("%g", 100000.0), "100000");
        assert_eq!(format_number("%g", 1000000.0), "1e+06");
        assert_eq!(format_number("%.3g", 999.6), "1e+03");
        assert_eq!(format_number("%e", 1234.5), "1.234500e+03");
        assert_eq!(format_number("%+.1f", 3.0), "+3.0");
        assert_eq!(format_number("%6.1f|", -3.0), "  -3.0|");
        assert_eq!(format_number("%-6.1f|", 3.0), "3.0   |");
        assert_eq!(format_number("%06.1f", -3.0), "-003.0");
        assert_eq!(format_number("%.1f", f64::NEG_INFINITY), "-inf");
        assert_eq!(format_number("%.1fdB", 1.0), "1.0dB");
        assert_eq!(format_number("%f", 1.0), "1.000000");
    }

    #[test]
    fn sprite_font_cells() {
        let font = SpriteFont { sprite: "LED".into(), frames: 12, charset: " 0123456789-".into(), cell: vec![10.0, 14.0], align: None, decimal_sprite: Some("dp".into()) };
        let frames = |s: &str| sprite_cells(s, &font).iter().map(|c| (c.frame, c.dot)).collect::<Vec<_>>();
        assert_eq!(frames("-1.5 dB"), vec![(11, false), (2, true), (6, false)], "units dropped, dot over the previous cell");
        assert_eq!(frames("  42  "), vec![(5, false), (3, false)], "blank cells trimmed");
        assert_eq!(frames(".5"), vec![(6, false)], "a leading dot has no cell to sit on");
        let letters = SpriteFont { charset: "ABC".into(), frames: 3, decimal_sprite: None, ..font };
        assert_eq!(sprite_cells("abc.", &letters).iter().map(|c| c.frame).collect::<Vec<_>>(), vec![0, 1, 2], "case-folded lookups; '.' without a DP sprite is just dropped");
    }

    #[test]
    fn wrapping_and_fitting() {
        let lines = wrap("one two three four five", 40.0, 10.0, false, false, 0);
        assert!(lines.len() > 1 && lines.iter().all(|l| !l.is_empty()));
        assert_eq!(wrap("one two three four five", 40.0, 10.0, false, false, 2).len(), 2);
        assert_eq!(wrap("a\nb", 400.0, 10.0, false, false, 0), vec!["a", "b"]);
        assert_eq!(fitted_size("12", 100.0, 10.0, false, false), 10.0);
        assert_eq!(fitted_size("a very long readout text", 10.0, 10.0, false, false), 5.0);
    }
}
