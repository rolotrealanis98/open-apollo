//! "Resize Plug-In" scales, remembered per plug-in as Console does (the more menu of the
//! plug-in window, capture s42: 75–200 %, Apply To All). Stored in
//! `$XDG_CONFIG_HOME/open-apollo/plugin-scales.toml`: `default = 100` and one
//! `"<plug-in name>" = <percent>` line per plug-in that differs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The scales Console offers.
pub const SCALES: [u16; 6] = [75, 100, 125, 150, 175, 200];

#[derive(Clone, Debug, PartialEq)]
pub struct PluginScales {
    /// The scale of plug-ins without their own (Apply To All sets it).
    pub default: u16,
    pub per_plugin: BTreeMap<String, u16>,
    /// Where `save` writes (None: memory only, e.g. tests and the FFI).
    pub path: Option<PathBuf>,
}

impl Default for PluginScales {
    fn default() -> Self {
        Self { default: 100, per_plugin: BTreeMap::new(), path: None }
    }
}

fn valid(pct: u16) -> Option<u16> {
    SCALES.contains(&pct).then_some(pct)
}

impl PluginScales {
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("open-apollo").join("plugin-scales.toml"))
    }

    /// From the default path (defaults when missing or unreadable).
    pub fn load() -> Self {
        let path = Self::default_path();
        let mut s = path.as_deref().map(Self::load_from).unwrap_or_default();
        s.path = path;
        s
    }

    pub fn load_from(path: &Path) -> Self {
        let mut s = std::fs::read_to_string(path).map(|t| Self::parse(&t)).unwrap_or_default();
        s.path = Some(path.to_path_buf());
        s
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let Some((k, v)) = line.split_once('=') else { continue };
            let Some(pct) = v.trim().parse::<u16>().ok().and_then(valid) else { continue };
            let key = k.trim();
            if key == "default" {
                s.default = pct;
            } else if let Some(name) = key.strip_prefix('"').and_then(|k| k.strip_suffix('"')) {
                s.per_plugin.insert(name.replace("\\\"", "\""), pct);
            }
        }
        s
    }

    pub fn to_toml(&self) -> String {
        let mut out = format!("# Resize Plug-In scales (percent)\ndefault = {}\n", self.default);
        for (name, pct) in &self.per_plugin {
            out.push_str(&format!("\"{}\" = {pct}\n", name.replace('"', "\\\"")));
        }
        out
    }

    /// The scale for `plugin`.
    pub fn get(&self, plugin: &str) -> u16 {
        self.per_plugin.get(plugin).copied().unwrap_or(self.default)
    }

    /// One plug-in's scale (Resize Plug-In › N %).
    pub fn set(&mut self, plugin: &str, pct: u16) {
        let Some(pct) = valid(pct) else { return };
        if pct == self.default {
            self.per_plugin.remove(plugin);
        } else {
            self.per_plugin.insert(plugin.to_string(), pct);
        }
        self.save();
    }

    /// Apply To All: every plug-in at `pct`.
    pub fn apply_to_all(&mut self, pct: u16) {
        let Some(pct) = valid(pct) else { return };
        self.default = pct;
        self.per_plugin.clear();
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, self.to_toml())?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            eprintln!("plug-in scales: could not save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_plugin_then_apply_to_all() {
        let mut s = PluginScales::default();
        assert_eq!(s.get("UAD Pultec EQP-1A"), 100);
        s.set("UAD Pultec EQP-1A", 150);
        s.set("UAD UA 1176 Rev A", 33); // not offered: ignored
        assert_eq!(s.get("UAD Pultec EQP-1A"), 150);
        assert_eq!(s.get("UAD UA 1176 Rev A"), 100);
        s.apply_to_all(125);
        assert_eq!(s.get("UAD Pultec EQP-1A"), 125);
        assert!(s.per_plugin.is_empty());
        s.set("UAD Pultec \"EQ\"", 75);
        assert_eq!(PluginScales::parse(&s.to_toml()).per_plugin, s.per_plugin);
        assert_eq!(PluginScales::parse(&s.to_toml()).default, 125);
    }

    #[test]
    fn saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("plugin-scales-{}", std::process::id()));
        let path = dir.join("plugin-scales.toml");
        let mut s = PluginScales::load_from(&path);
        s.set("UAD SSL G Bus Compressor", 200);
        assert_eq!(PluginScales::load_from(&path).get("UAD SSL G Bus Compressor"), 200);
        let _ = std::fs::remove_dir_all(dir);
    }
}
