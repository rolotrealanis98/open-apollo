//! houston files are JUCE-lenient JSON: trailing commas and `//`/`/* */` comments occur.

/// Strips comments, then trailing commas (outside strings), so serde_json accepts the text.
pub fn sanitize(input: &str) -> String {
    strip_trailing_commas(&strip_comments(input))
}

fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\\' && i + 1 < bytes.len() {
                out.push_str(&input[i..i + 2]);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        let len = utf8_len(b);
        out.push_str(&input[i..i + len]);
        i += len;
    }
    out
}

fn strip_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                // Copy whole UTF-8 sequences untouched.
                let len = utf8_len(bytes[i]);
                out.push_str(&input[i..i + len]);
                i += len;
            }
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

pub fn parse(text: &str) -> serde_json::Result<serde_json::Value> {
    serde_json::from_str(&sanitize(text))
}

#[cfg(test)]
mod tests {
    #[test]
    fn trailing_commas_and_comments() {
        let v = super::parse("{\"a\": [1, 2,], // c\n \"b\": \"x,}\", /* d */ }").unwrap();
        assert_eq!(v["a"][1], 2);
        assert_eq!(v["b"], "x,}");
    }
}
