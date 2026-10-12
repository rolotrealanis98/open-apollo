//! Persistent settings: `$XDG_CONFIG_HOME/open-apollo/uaconsole.toml` (a flat TOML subset:
//! `key = "string" | number | bool`, comments, unknown keys ignored).

use std::path::{Path, PathBuf};

use crate::cli::EngineAddr;
use crate::view::{ViewMode, ZOOM_MAX, ZOOM_MIN};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Last engine connected to (recorded for a future connect menu; launches without
    /// --engine always use the local engine).
    pub engine: Option<EngineAddr>,
    pub zoom: f32,
    pub compact: bool,
    pub view: ViewMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: None,
            zoom: 1.0,
            compact: false,
            view: ViewMode::Fit,
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("open-apollo").join("uaconsole.toml"))
    }

    /// Settings from disk; defaults when missing or unreadable.
    pub fn load() -> Self {
        Self::path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path =
            Self::path().ok_or_else(|| std::io::Error::other("no $XDG_CONFIG_HOME or $HOME"))?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Writes atomically (temp file + rename), creating the directory.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(&tmp, path)
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = Value::parse(value.trim());
            match (key.trim(), value) {
                ("engine", Some(Value::Str(v))) => s.engine = v.parse().ok(),
                ("zoom", Some(Value::Num(v))) => s.zoom = (v as f32).clamp(ZOOM_MIN, ZOOM_MAX),
                ("compact", Some(Value::Bool(v))) => s.compact = v,
                ("view", Some(Value::Str(v))) => s.view = ViewMode::parse(&v).unwrap_or(s.view),
                _ => {}
            }
        }
        s
    }

    pub fn to_toml(&self) -> String {
        let mut out = String::from("# uaconsole settings (rewritten when they change)\n");
        if let Some(e) = &self.engine {
            out += &format!("engine = {}\n", quote(&e.to_string()));
        }
        out += &format!(
            "zoom = {}\ncompact = {}\nview = {}\n",
            self.zoom,
            self.compact,
            quote(self.view.as_str())
        );
        out
    }
}

enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl Value {
    fn parse(v: &str) -> Option<Self> {
        if let Some(rest) = v.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => return Some(Value::Str(out)),
                    '\\' => match chars.next()? {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        c => out.push(c),
                    },
                    c => out.push(c),
                }
            }
            return None;
        }
        let v = v.split('#').next().unwrap_or("").trim();
        match v {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => v.parse().ok().map(Value::Num),
        }
    }
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Settings {
            engine: Some("[::1]:4710".parse().unwrap()),
            zoom: 1.25,
            compact: true,
            view: ViewMode::OneToOne,
        };
        assert_eq!(Settings::parse(&s.to_toml()), s);
        assert_eq!(
            Settings::parse(&Settings::default().to_toml()),
            Settings::default()
        );
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("uaconsole-settings-{}", std::process::id()));
        let path = dir.join("open-apollo").join("uaconsole.toml");
        let s = Settings {
            zoom: 0.75,
            compact: true,
            ..Default::default()
        };
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), s);
        assert_eq!(
            Settings::load_from(&dir.join("missing.toml")),
            Settings::default()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lenient() {
        let s = Settings::parse(
            "# hi\n[ui]\nzoom = 9 # too big\ncompact=true\nengine = \"studio\"\nview = \"nope\"\nother = 1\n",
        );
        assert_eq!(s.zoom, ZOOM_MAX);
        assert!(s.compact);
        assert_eq!(s.engine.unwrap().port, 4710);
        assert_eq!(s.view, ViewMode::Fit);
    }
}
