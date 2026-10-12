//! Finding and indexing a folder of ConsoleLink plug-in specs
//! (`PluginSpec_*.json` plus `Images/<sprite>@2x.png`). Port of ConsoleLab's `PluginSpecLibrary`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use houston::ImageCache;
use parking_lot::Mutex;
use serde::Deserialize;

use crate::spec::PluginSpec;

/// Where plug-in specs are looked for, in order (first folder with a `PluginSpec_*.json` wins):
/// 1. `$UACONSOLE_PLUGIN_SPECS`
/// 2. `~/.local/share/open-apollo/plugin-specs`
/// 3. ConsoleLink's integrated development stage (full catalog and sprites)
/// 4. `~/wine-ua-work/plugin-specs-local` (small development copy)
pub struct SpecLocator;

impl SpecLocator {
    pub const ENV: &'static str = "UACONSOLE_PLUGIN_SPECS";

    pub fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(p) = std::env::var(Self::ENV) {
            if !p.is_empty() {
                out.push(PathBuf::from(p));
            }
        }
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        out.push(home.join(".local/share/open-apollo/plugin-specs"));
        out.push(home.join("wine-ua-work/plugin-specs/integrate/stage/Console Link/Console Link/PluginSpecs"));
        out.push(home.join("wine-ua-work/plugin-specs-local"));
        out
    }

    pub fn locate() -> Option<PathBuf> {
        Self::candidates().into_iter().find(|p| Self::looks_like_specs(p))
    }

    /// A folder with at least one `PluginSpec_*.json`.
    pub fn looks_like_specs(dir: &Path) -> bool {
        spec_files(dir).next().is_some()
    }
}

fn spec_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| {
        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("PluginSpec_") && n.ends_with(".json"))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub effect_name: String,
    pub name_pattern: String,
    pub file: PathBuf,
}

impl Entry {
    /// `spl_vitalizer_mk2_t` for `PluginSpec_spl_vitalizer_mk2_t.json`.
    pub fn stem(&self) -> &str {
        let name = self.file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        name.strip_prefix("PluginSpec_").unwrap_or(name)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    effect_name: String,
    name_pattern: String,
}

/// The specs in one folder, indexed by effect name. Specs are decoded on first use and cached.
pub struct PluginSpecLibrary {
    pub folder: PathBuf,
    /// `Images` (or `images`) under `folder`.
    pub images_dir: PathBuf,
    /// Longest `name_pattern` first, so "LA-2A Silver" wins over "LA-2A".
    pub entries: Vec<Entry>,
    /// Files whose header did not decode, with the error.
    pub failures: BTreeMap<String, String>,
    cache: Mutex<HashMap<PathBuf, Arc<PluginSpec>>>,
    sprite_keys: Mutex<HashMap<String, Option<Arc<str>>>>,
}

impl PluginSpecLibrary {
    /// The first folder [`SpecLocator`] finds.
    pub fn locate() -> Option<Self> {
        Self::open(&SpecLocator::locate()?).ok()
    }

    /// Scans `folder` (reads each spec's header; ~4.6 MB for ConsoleLink's 208 specs).
    pub fn open(folder: &Path) -> Result<Self, String> {
        let mut files: Vec<PathBuf> = spec_files(folder).collect();
        files.sort();
        if files.is_empty() {
            return Err(format!("{} has no PluginSpec_*.json", folder.display()));
        }
        let mut entries = Vec::new();
        let mut failures = BTreeMap::new();
        for file in files {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            let header = std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| serde_json::from_str::<Header>(&t).map_err(|e| e.to_string()));
            match header {
                Ok(h) => entries.push(Entry { effect_name: h.effect_name, name_pattern: h.name_pattern, file }),
                Err(e) => {
                    failures.insert(name, e);
                }
            }
        }
        entries.sort_by(|a, b| b.name_pattern.len().cmp(&a.name_pattern.len()).then_with(|| a.stem().cmp(b.stem())));
        let images = folder.join("Images");
        let images_dir = if images.is_dir() { images } else { folder.join("images") };
        Ok(Self {
            folder: folder.to_path_buf(),
            images_dir,
            entries,
            failures,
            cache: Mutex::new(HashMap::new()),
            sprite_keys: Mutex::new(HashMap::new()),
        })
    }

    /// The entry for an engine `EffectName` ("UAD SPL Vitalizer MK2-T"): an exact
    /// `effect_name` match, else the longest `name_pattern` the name contains.
    pub fn entry_for_effect(&self, effect_name: &str) -> Option<&Entry> {
        if effect_name.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find(|e| e.effect_name == effect_name)
            .or_else(|| self.entries.iter().find(|e| !e.name_pattern.is_empty() && effect_name.contains(&e.name_pattern)))
    }

    /// An entry by file stem (`spl_vitalizer_mk2_t`), effect name or name pattern.
    pub fn entry_named(&self, name: &str) -> Option<&Entry> {
        let stem = name.strip_prefix("PluginSpec_").unwrap_or(name);
        let stem = stem.strip_suffix(".json").unwrap_or(stem);
        self.entries
            .iter()
            .find(|e| e.stem() == stem)
            .or_else(|| self.entry_for_effect(name))
            .or_else(|| self.entries.iter().find(|e| e.name_pattern == name))
    }

    pub fn spec(&self, entry: &Entry) -> Result<Arc<PluginSpec>, String> {
        if let Some(hit) = self.cache.lock().get(&entry.file) {
            return Ok(hit.clone());
        }
        let spec = Arc::new(PluginSpec::load(&entry.file)?);
        self.cache.lock().insert(entry.file.clone(), spec.clone());
        Ok(spec)
    }

    pub fn spec_for_effect(&self, effect_name: &str) -> Option<Arc<PluginSpec>> {
        self.entry_for_effect(effect_name).and_then(|e| self.spec(e).ok())
    }

    /// Lets `cache` (and so `gfx`, which draws through it) resolve this library's sprite keys.
    pub fn attach(&self, cache: &ImageCache) {
        cache.register_root(ImageCache::SPEC_SCHEME, &self.folder);
    }

    /// Image key for a sprite name: `spec:Images/<name>@2x.png`, else `spec:Images/<name>.png`;
    /// None when neither file exists. Resolved once per name.
    pub fn sprite_key(&self, name: &str) -> Option<Arc<str>> {
        if let Some(hit) = self.sprite_keys.lock().get(name) {
            return hit.clone();
        }
        let dir = self.images_dir.file_name().and_then(|n| n.to_str()).unwrap_or("Images").to_string();
        let key = [format!("{name}@2x.png"), format!("{name}.png")]
            .into_iter()
            .find(|f| self.images_dir.join(f).is_file())
            .map(|f| Arc::<str>::from(format!("{}:{dir}/{f}", ImageCache::SPEC_SCHEME)));
        self.sprite_keys.lock().insert(name.to_string(), key.clone());
        key
    }
}
