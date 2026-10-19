//! Persistent settings: `$XDG_CONFIG_HOME/open-apollo/uaconsole.toml` (a flat TOML subset:
//! `key = "string" | number | bool`, comments, unknown keys ignored).

use std::path::{Path, PathBuf};

use rack::surfaces::{BrowserTab, InsertSize, Sections};
use rack::ViewState;

use crate::cli::EngineAddr;
use crate::view::{ViewMode, ZOOM_MAX, ZOOM_MIN};

/// The mixer's view state that Console 1.3.1 remembers between launches: the INPUTS / INSERTS /
/// SENDS / FADER rows, insert size, FIXED SLOTS, SENDS OVERVIEW, the browser, SHOW AUX / CTRL ROOM
/// and the window size (layout points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerPrefs {
    pub sections: Sections,
    pub insert_size: InsertSize,
    pub fixed_slots: bool,
    pub sends_overview: bool,
    pub show_browser: bool,
    pub tab: BrowserTab,
    pub show_aux: bool,
    pub show_ctrl_room: bool,
    pub window: Option<(f32, f32)>,
}

impl Default for MixerPrefs {
    fn default() -> Self {
        Self::capture(&ViewState::default())
    }
}

impl MixerPrefs {
    pub fn capture(v: &ViewState) -> Self {
        Self {
            sections: v.sections,
            insert_size: v.insert_size,
            fixed_slots: v.fixed_slots,
            sends_overview: v.sends_overview,
            show_browser: v.show_browser,
            tab: v.browser.tab,
            show_aux: v.show_aux,
            show_ctrl_room: v.show_ctrl_room,
            window: v.window,
        }
    }

    pub fn apply(&self, v: &mut ViewState) {
        v.sections = self.sections;
        v.insert_size = self.insert_size;
        v.fixed_slots = self.fixed_slots;
        v.sends_overview = self.sends_overview;
        v.show_browser = self.show_browser;
        v.browser.tab = self.tab;
        v.show_aux = self.show_aux;
        v.show_ctrl_room = self.show_ctrl_room;
        v.window = self.window;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Last engine connected to (recorded for a future connect menu; launches without
    /// --engine always use the local engine).
    pub engine: Option<EngineAddr>,
    pub zoom: f32,
    pub compact: bool,
    pub view: ViewMode,
    pub mixer: MixerPrefs,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            engine: None,
            zoom: 1.0,
            compact: false,
            view: ViewMode::Fit,
            mixer: MixerPrefs::default(),
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
                ("inputs", Some(Value::Bool(v))) => s.mixer.sections.inputs = v,
                ("inserts", Some(Value::Bool(v))) => s.mixer.sections.inserts = v,
                ("sends", Some(Value::Bool(v))) => s.mixer.sections.sends = v,
                ("fader", Some(Value::Bool(v))) => s.mixer.sections.fader = v,
                ("insert_size", Some(Value::Str(v))) => match v.as_str() {
                    "small" => s.mixer.insert_size = InsertSize::Small,
                    "large" => s.mixer.insert_size = InsertSize::Large,
                    _ => {}
                },
                ("fixed_slots", Some(Value::Bool(v))) => s.mixer.fixed_slots = v,
                ("sends_overview", Some(Value::Bool(v))) => s.mixer.sends_overview = v,
                ("browser", Some(Value::Bool(v))) => s.mixer.show_browser = v,
                ("browser_tab", Some(Value::Str(v))) => match v.as_str() {
                    "plugins" => s.mixer.tab = BrowserTab::Plugins,
                    "presets" => s.mixer.tab = BrowserTab::Presets,
                    _ => {}
                },
                ("aux", Some(Value::Bool(v))) => s.mixer.show_aux = v,
                ("ctrl_room", Some(Value::Bool(v))) => s.mixer.show_ctrl_room = v,
                ("window", Some(Value::Str(v))) => s.mixer.window = parse_window(&v).or(s.mixer.window),
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
        let m = &self.mixer;
        out += &format!(
            "inputs = {}\ninserts = {}\nsends = {}\nfader = {}\ninsert_size = {}\nfixed_slots = {}\n\
             sends_overview = {}\nbrowser = {}\nbrowser_tab = {}\naux = {}\nctrl_room = {}\n",
            m.sections.inputs,
            m.sections.inserts,
            m.sections.sends,
            m.sections.fader,
            quote(if m.insert_size == InsertSize::Small { "small" } else { "large" }),
            m.fixed_slots,
            m.sends_overview,
            m.show_browser,
            quote(if m.tab == BrowserTab::Presets { "presets" } else { "plugins" }),
            m.show_aux,
            m.show_ctrl_room,
        );
        if let Some((w, h)) = m.window {
            out += &format!("window = \"{}x{}\"\n", w.round(), h.round());
        }
        out
    }
}

/// `"WxH"` in layout points, within what a window can be.
fn parse_window(v: &str) -> Option<(f32, f32)> {
    let (w, h) = v.split_once('x')?;
    let (w, h) = (w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?);
    ((320.0..=16384.0).contains(&w) && (200.0..=16384.0).contains(&h)).then_some((w, h))
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
            mixer: MixerPrefs {
                sections: Sections { inputs: false, inserts: true, sends: true, fader: false },
                insert_size: InsertSize::Small,
                fixed_slots: false,
                sends_overview: true,
                show_browser: false,
                tab: BrowserTab::Presets,
                show_aux: false,
                show_ctrl_room: true,
                window: Some((1440.0, 900.0)),
            },
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
        // Out-of-range or malformed mixer values keep the defaults.
        let s = Settings::parse("window = \"10x10\"\nbrowser_tab = \"x\"\ninsert_size = 3\nsends = \"yes\"\n");
        assert_eq!(s.mixer, MixerPrefs::default());
    }

    #[test]
    fn mixer_prefs_apply_to_view() {
        let text = "sends = true\ninputs = false\ninsert_size = \"small\"\nwindow = \"1200x800\"\n";
        let prefs = Settings::parse(text).mixer;
        let mut v = ViewState::default();
        prefs.apply(&mut v);
        assert!(v.sections.sends && !v.sections.inputs && v.sections.fader);
        assert_eq!(v.insert_size, InsertSize::Small);
        assert_eq!(v.window, Some((1200.0, 800.0)));
        assert_eq!(MixerPrefs::capture(&v), prefs);
    }
}
