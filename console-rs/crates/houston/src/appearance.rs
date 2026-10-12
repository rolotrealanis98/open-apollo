use std::collections::HashMap;
use std::path::Path;

use crate::geom::{Insets, Rect, Rgba};
use crate::json;
use crate::resources::Resources;

pub const SKIN_SECTIONS: [&str; 8] = ["views", "backgrounds", "knobs", "faders", "meters", "inserts", "fonts", "cursors"];

/// A JSON scalar from a skin entry.
#[derive(Clone, Debug, PartialEq)]
pub enum SkinValue {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl SkinValue {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        match v {
            serde_json::Value::String(s) => Some(Self::Str(s.clone())),
            serde_json::Value::Number(n) => n.as_f64().map(Self::Num),
            serde_json::Value::Bool(b) => Some(Self::Bool(*b)),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Self::Str(s) = self { Some(s) } else { None }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            Self::Str(s) => s.trim().parse().ok(),
            Self::Bool(_) => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Str(s) if s == "true" => Some(true),
            Self::Str(s) if s == "false" => Some(false),
            Self::Num(n) => Some(*n != 0.0),
            _ => None,
        }
    }
}

/// Font of a skin: `font`, `font-size`, `font-style`, `color-text`.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: Option<Rgba>,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self { family: "Helvetica".into(), size: 9.0, bold: false, italic: false, color: None }
    }
}

/// One fully inherited skin entry.
#[derive(Clone, Debug)]
pub struct Skin {
    pub section: String,
    pub key: String,
    pub parent: Option<String>,
    pub props: HashMap<String, SkinValue>,
    palette: std::sync::Arc<HashMap<String, Rgba>>,
}

impl Skin {
    pub fn id(&self) -> String {
        format!("{}/{}", self.section, self.key)
    }
    pub fn get(&self, prop: &str) -> Option<&SkinValue> {
        self.props.get(prop)
    }
    /// Image path (relative to `images/`) for e.g. `image`, `image-on`, `image-film`;
    /// an explicit `""` means "no image".
    pub fn image(&self, prop: &str) -> Option<&str> {
        self.get(prop)?.as_str().filter(|s| !s.is_empty())
    }
    /// All image-valued properties.
    pub fn images(&self) -> impl Iterator<Item = (&str, &str)> {
        self.props.iter().filter_map(|(k, v)| {
            let s = v.as_str()?;
            (k.starts_with("image") && !s.is_empty() && (s.ends_with(".png") || s.ends_with(".svg") || s.contains('/')))
                .then_some((k.as_str(), s))
        })
    }
    pub fn number(&self, prop: &str) -> Option<f64> {
        self.get(prop)?.as_f64()
    }
    pub fn flag(&self, prop: &str) -> bool {
        self.get(prop).and_then(SkinValue::as_bool).unwrap_or(false)
    }
    /// Logical size (`width`, `height`) in points.
    pub fn size(&self) -> Option<(f32, f32)> {
        Some((self.number("width")? as f32, self.number("height")? as f32))
    }
    /// Pixels per filmstrip frame.
    pub fn frame_height(&self) -> Option<u32> {
        self.number("frame-height").map(|v| v as u32)
    }
    fn floats(&self, prop: &str) -> Option<Vec<f32>> {
        let s = self.get(prop)?.as_str()?;
        s.split(',').map(|p| p.trim().parse::<f32>().ok()).collect()
    }
    /// `"l,t,r,b"` in image pixels.
    pub fn insets(&self, prop: &str) -> Option<Insets> {
        match self.floats(prop)?.as_slice() {
            [l, t, r, b] => Some(Insets { l: *l, t: *t, r: *r, b: *b }),
            _ => None,
        }
    }
    /// `"x,y,w,h"` in image pixels.
    pub fn rect(&self, prop: &str) -> Option<Rect> {
        match self.floats(prop)?.as_slice() {
            [x, y, w, h] => Some(Rect::new(*x, *y, *w, *h)),
            _ => None,
        }
    }
    pub fn color(&self, prop: &str) -> Option<Rgba> {
        let s = self.get(prop)?.as_str()?;
        Rgba::parse(s).or_else(|| self.palette.get(s.trim()).copied())
    }
    pub fn font(&self) -> FontSpec {
        let style = self.get("font-style").and_then(SkinValue::as_str).unwrap_or("");
        FontSpec {
            family: self.get("font").and_then(SkinValue::as_str).unwrap_or("Helvetica").to_string(),
            size: self.number("font-size").unwrap_or(9.0) as f32,
            bold: style.contains("bold"),
            italic: style.contains("italic"),
            color: self.color("color-text"),
        }
    }
}

/// The merged skin database (`appearance/*.json`).
#[derive(Clone, Debug, Default)]
pub struct Appearance {
    pub colors: std::sync::Arc<HashMap<String, Rgba>>,
    pub image_paths: HashMap<String, String>,
    pub skins: HashMap<String, Skin>,
    pub source_files: Vec<String>,
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Json(String, serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Json(file, e) => write!(f, "{file}: {e}"),
        }
    }
}
impl std::error::Error for LoadError {}

impl Appearance {
    pub fn skin(&self, section: &str, key: &str) -> Option<&Skin> {
        self.skins.get(&format!("{section}/{key}"))
    }

    /// `Appearance.json` first, then every other `appearance/*.json`; union per section,
    /// first definition wins; parents resolved after the merge.
    pub fn load(resources: &Resources) -> Result<Self, LoadError> {
        let dir = resources.appearance_dir();
        let mut files = vec![dir.join("Appearance.json")];
        let mut others: Vec<_> = std::fs::read_dir(&dir)
            .map_err(LoadError::Io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json") && p.file_name().is_some_and(|n| n != "Appearance.json"))
            .collect();
        others.sort();
        files.extend(others);
        Self::load_files(&files)
    }

    pub fn load_files(files: &[impl AsRef<Path>]) -> Result<Self, LoadError> {
        let mut raw_colors: HashMap<String, String> = HashMap::new();
        let mut image_paths: HashMap<String, String> = HashMap::new();
        let mut raw: HashMap<String, HashMap<String, serde_json::Map<String, serde_json::Value>>> = HashMap::new();
        let mut loaded = Vec::new();

        for file in files {
            let file = file.as_ref();
            let text = std::fs::read_to_string(file).map_err(LoadError::Io)?;
            let root = json::parse(&text).map_err(|e| LoadError::Json(file.display().to_string(), e))?;
            let Some(root) = root.as_object() else { continue };
            loaded.push(file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            for (name, value) in root {
                let Some(section) = value.as_object() else { continue };
                match name.as_str() {
                    "colors" => {
                        for (k, v) in section {
                            if let Some(s) = v.as_str() {
                                raw_colors.entry(k.clone()).or_insert_with(|| s.to_string());
                            }
                        }
                    }
                    "image_paths" => {
                        for (k, v) in section {
                            if let Some(s) = v.as_str() {
                                image_paths.entry(k.clone()).or_insert_with(|| s.to_string());
                            }
                        }
                    }
                    _ => {
                        let entries = raw.entry(name.clone()).or_default();
                        for (key, entry) in section {
                            let Some(props) = entry.as_object() else { continue };
                            if key.starts_with("__") || key.starts_with('*') || key == "comment" {
                                continue;
                            }
                            entries.entry(key.clone()).or_insert_with(|| props.clone());
                        }
                    }
                }
            }
        }

        let mut colors = HashMap::new();
        for name in raw_colors.keys() {
            if let Some(c) = resolve_color(&raw_colors[name], &raw_colors, 0) {
                colors.insert(name.clone(), c);
            }
        }
        let palette = std::sync::Arc::new(colors);

        let mut skins = HashMap::new();
        for (section, entries) in &raw {
            for key in entries.keys() {
                let mut chain = Vec::new();
                let mut seen = std::collections::HashSet::new();
                let mut current = Some(key.as_str());
                while let Some(k) = current {
                    if !seen.insert(k) {
                        break;
                    }
                    let Some(props) = entries.get(k) else { break };
                    chain.push(props);
                    current = props.get("parent").and_then(|p| p.as_str());
                }
                let mut merged = HashMap::new();
                for props in chain.iter().rev() {
                    for (p, v) in props.iter() {
                        if p.starts_with('_') || p == "parent" || p == "comment" {
                            continue;
                        }
                        if let Some(v) = SkinValue::from_json(v) {
                            merged.insert(p.clone(), v);
                        }
                    }
                }
                let parent = entries[key].get("parent").and_then(|p| p.as_str()).map(String::from);
                skins.insert(
                    format!("{section}/{key}"),
                    Skin { section: section.clone(), key: key.clone(), parent, props: merged, palette: palette.clone() },
                );
            }
        }
        Ok(Self { colors: palette, image_paths, skins, source_files: loaded })
    }
}

fn resolve_color(text: &str, palette: &HashMap<String, String>, depth: u32) -> Option<Rgba> {
    if let Some(c) = Rgba::parse(text) {
        return Some(c);
    }
    if depth > 8 {
        return None;
    }
    resolve_color(palette.get(text.trim())?, palette, depth + 1)
}
