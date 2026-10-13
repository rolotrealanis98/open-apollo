//! Finding and indexing a folder of plug-in panel specs. Port of ConsoleLab's `PluginSpecLibrary`.
//!
//! Two folder layouts are read:
//! - **release (schema 2)**: `PluginSpec_*.json` plus `recipes/*.json`. Sprite fields are recipe
//!   ids; the art is generated from the user's own UA install by the importer into the art root
//!   ([`ArtLocator`]: `Images/<id>@2x.png`, `Text/<id>.txt|.json`). Nothing UA-made ships.
//! - **legacy (development)**: `PluginSpec_*.json` plus `Images/<sprite>@2x.png` next to them
//!   (ConsoleLink's stage). Ids the folder lacks are also looked up in the art root, so a stage
//!   copy can use generated chrome art.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use houston::ImageCache;
use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::recipes::{RecipeStatus, Recipes};
use crate::spec::PluginSpec;

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `$XDG_DATA_HOME`, else `~/.local/share`.
fn data_home() -> PathBuf {
    match std::env::var("XDG_DATA_HOME") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => home().join(".local/share"),
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var(name).ok().filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// Where plug-in specs are looked for, in order (first folder with a `PluginSpec_*.json` wins):
/// 1. `$OA_PLUGIN_SPECS` (or the older `$UACONSOLE_PLUGIN_SPECS`)
/// 2. `$XDG_DATA_HOME/open-apollo/plugin-specs` (`~/.local/share/open-apollo/plugin-specs`)
/// 3. `/usr/share/open-apollo/plugin-specs`
///
/// A development stage is used by pointing `$OA_PLUGIN_SPECS` at it.
pub struct SpecLocator;

impl SpecLocator {
    pub const ENV: &'static str = "OA_PLUGIN_SPECS";
    /// Accepted until the app is renamed.
    pub const ENV_ALIAS: &'static str = "UACONSOLE_PLUGIN_SPECS";

    pub fn candidates() -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = [Self::ENV, Self::ENV_ALIAS].iter().filter_map(|e| env_dir(e)).collect();
        out.push(data_home().join("open-apollo/plugin-specs"));
        out.push(PathBuf::from("/usr/share/open-apollo/plugin-specs"));
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

/// Where the importer writes the art and texts it regenerates from the user's install:
/// `$OA_PLUGIN_ART`, else `$XDG_DATA_HOME/open-apollo/generated`.
pub struct ArtLocator;

impl ArtLocator {
    pub const ENV: &'static str = "OA_PLUGIN_ART";

    pub fn locate() -> PathBuf {
        env_dir(Self::ENV).unwrap_or_else(|| data_home().join("open-apollo/generated"))
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

/// A text the importer extracted: one string (`dll-string`) or a label list (`dll-string-table`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextValue {
    Text(String),
    List(Vec<String>),
}

/// Whether a spec's art and texts are all present (see [`PluginSpecLibrary::availability`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Every sprite and text the spec references can be drawn.
    Ready,
    /// The importer can make these ids but has not yet (run `oa-assets build` and reopen).
    Pending(Vec<String>),
    /// These ids have no buildable recipe (needs-tracer, no-recipe, deferred, unknown): the panel
    /// cannot be drawn from this install; show the generic list.
    Unavailable(Vec<String>),
}

impl Availability {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// The specs in one folder, indexed by effect name. Specs are decoded on first use and cached.
pub struct PluginSpecLibrary {
    pub folder: PathBuf,
    /// Where sprites are read: `<folder>/Images` (legacy) or `<art root>/Images` (release).
    pub images_dir: PathBuf,
    /// The generated-art root ([`ArtLocator`]); the `spec` image scheme's root for a release folder.
    pub art_root: PathBuf,
    /// `recipes/*.json` of a release folder (empty for a legacy folder).
    pub recipes: Recipes,
    /// Longest `name_pattern` first, so "LA-2A Silver" wins over "LA-2A".
    pub entries: Vec<Entry>,
    /// Files whose header did not decode, with the error.
    pub failures: BTreeMap<String, String>,
    legacy: bool,
    cache: Mutex<HashMap<PathBuf, Arc<PluginSpec>>>,
    sprite_keys: Mutex<HashMap<String, Option<Arc<str>>>>,
    texts: Mutex<HashMap<String, Option<TextValue>>>,
}

impl PluginSpecLibrary {
    /// Image scheme of the art root when the spec folder is a legacy stage.
    pub const ART_SCHEME: &'static str = "oa-art";

    /// The first folder [`SpecLocator`] finds, with the art root [`ArtLocator`] gives.
    pub fn locate() -> Option<Self> {
        Self::open(&SpecLocator::locate()?).ok()
    }

    /// Scans `folder` (reads each spec's header) with the default art root.
    pub fn open(folder: &Path) -> Result<Self, String> {
        Self::open_with_art(folder, &ArtLocator::locate())
    }

    /// Scans `folder`; generated art is read from `art_root`. A folder with `recipes/` is a
    /// release folder (its recipes are loaded; duplicate ids are an error), else a legacy one.
    pub fn open_with_art(folder: &Path, art_root: &Path) -> Result<Self, String> {
        let mut files: Vec<PathBuf> = spec_files(folder).collect();
        files.sort();
        if files.is_empty() {
            return Err(format!("{} has no PluginSpec_*.json", folder.display()));
        }
        let mut entries = Vec::new();
        let mut failures = BTreeMap::new();
        for file in files {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            let header = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str::<Header>(&t).map_err(|e| e.to_string()));
            match header {
                Ok(h) => entries.push(Entry { effect_name: h.effect_name, name_pattern: h.name_pattern, file }),
                Err(e) => {
                    failures.insert(name, e);
                }
            }
        }
        entries.sort_by(|a, b| b.name_pattern.len().cmp(&a.name_pattern.len()).then_with(|| a.stem().cmp(b.stem())));
        let recipes_dir = folder.join("recipes");
        let legacy = !recipes_dir.is_dir();
        let recipes = if legacy { Recipes::default() } else { Recipes::load(&recipes_dir)? };
        let images_dir = if legacy {
            let images = folder.join("Images");
            if images.is_dir() { images } else { folder.join("images") }
        } else {
            art_root.join("Images")
        };
        Ok(Self {
            folder: folder.to_path_buf(),
            images_dir,
            art_root: art_root.to_path_buf(),
            recipes,
            entries,
            failures,
            legacy,
            cache: Mutex::new(HashMap::new()),
            sprite_keys: Mutex::new(HashMap::new()),
            texts: Mutex::new(HashMap::new()),
        })
    }

    /// A legacy (development) folder: sprites next to the specs, no recipes.
    pub fn is_legacy(&self) -> bool {
        self.legacy
    }

    /// The entry for an engine `EffectName` ("UAD SPL Vitalizer MK2-T"): an exact
    /// `effect_name` match, else the longest `name_pattern` the name contains.
    pub fn entry_for_effect(&self, effect_name: &str) -> Option<&Entry> {
        if effect_name.is_empty() {
            return None;
        }
        self.entries.iter().find(|e| e.effect_name == effect_name).or_else(|| {
            self.entries.iter().find(|e| !e.name_pattern.is_empty() && effect_name.contains(&e.name_pattern))
        })
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

    /// Lets `cache` (and so `gfx`, which draws through it) resolve this library's sprite keys:
    /// `spec:` is the art root (release) or the spec folder (legacy, plus `oa-art:` for the art
    /// root).
    pub fn attach(&self, cache: &ImageCache) {
        if self.legacy {
            cache.register_root(ImageCache::SPEC_SCHEME, &self.folder);
            cache.register_root(Self::ART_SCHEME, &self.art_root);
        } else {
            cache.register_root(ImageCache::SPEC_SCHEME, &self.art_root);
        }
    }

    /// Image key for a sprite (recipe) id: `spec:Images/<id>@2x.png`, else `…/<id>.png`; a
    /// legacy folder then tries the art root (`oa-art:Images/…`). None when no file exists, or
    /// when the id's recipe says it cannot be made (no-recipe, deferred). Resolved once per id.
    pub fn sprite_key(&self, name: &str) -> Option<Arc<str>> {
        if let Some(hit) = self.sprite_keys.lock().get(name) {
            return hit.clone();
        }
        let blocked =
            self.recipes.status(name).is_some_and(|s| matches!(s, RecipeStatus::NoRecipe | RecipeStatus::Deferred));
        let files = [format!("{name}@2x.png"), format!("{name}.png")];
        let mut key = None;
        if !blocked && valid_id(name) {
            let dir = self.images_dir.file_name().and_then(|n| n.to_str()).unwrap_or("Images").to_string();
            key = files
                .iter()
                .find(|f| self.images_dir.join(f).is_file())
                .map(|f| Arc::<str>::from(format!("{}:{dir}/{f}", ImageCache::SPEC_SCHEME)));
            if key.is_none() && self.legacy {
                let art = self.art_root.join("Images");
                key = files
                    .iter()
                    .find(|f| art.join(f).is_file())
                    .map(|f| Arc::<str>::from(format!("{}:Images/{f}", Self::ART_SCHEME)));
            }
        }
        self.sprite_keys.lock().insert(name.to_string(), key.clone());
        key
    }

    /// A text the importer extracted (`Text/<id>.txt` or `Text/<id>.json` under the art root).
    /// With a recipe that pins `output.sha256`, the text must match it (bytes of the `.txt`; the
    /// compact UTF-8 JSON of the list); a mismatch reads as not imported.
    pub fn text(&self, id: &str) -> Option<TextValue> {
        if let Some(hit) = self.texts.lock().get(id) {
            return hit.clone();
        }
        let value = self.read_text(id);
        self.texts.lock().insert(id.to_string(), value.clone());
        value
    }

    fn read_text(&self, id: &str) -> Option<TextValue> {
        if !valid_id(id) {
            return None;
        }
        let dir = self.art_root.join("Text");
        let recipe = self.recipes.texts.get(id);
        if recipe.is_some_and(|r| !r.status.is_buildable()) {
            return None;
        }
        let expect = recipe.and_then(|r| r.output.as_ref()).and_then(|o| o.sha256.clone());
        let hex = |bytes: &[u8]| Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        if let Ok(bytes) = std::fs::read(dir.join(format!("{id}.txt"))) {
            if expect.as_ref().is_some_and(|e| *e != hex(&bytes)) {
                eprintln!("plugin-ui: Text/{id}.txt does not match its recipe; ignored");
                return None;
            }
            return String::from_utf8(bytes).ok().map(TextValue::Text);
        }
        if let Ok(text) = std::fs::read_to_string(dir.join(format!("{id}.json"))) {
            let list: Vec<String> = serde_json::from_str(&text).ok()?;
            let canonical = serde_json::to_string(&list).ok()?;
            if expect.as_ref().is_some_and(|e| *e != hex(canonical.as_bytes())) {
                eprintln!("plugin-ui: Text/{id}.json does not match its recipe; ignored");
                return None;
            }
            return Some(TextValue::List(list));
        }
        None
    }

    /// Whether `spec` can be drawn from this install. Release folders: an id whose recipe is not
    /// buildable (or that no recipe names) makes the spec [`Availability::Unavailable`]; a
    /// buildable id not generated yet makes it [`Availability::Pending`]. Legacy folders have no
    /// recipes: missing files are pending.
    pub fn availability(&self, spec: &PluginSpec) -> Availability {
        let mut pending = Vec::new();
        let mut unavailable = Vec::new();
        for id in spec.sprite_names() {
            if !self.legacy && !self.recipes.is_buildable(id) {
                unavailable.push(id.to_string());
            } else if self.sprite_key(id).is_none() {
                pending.push(id.to_string());
            }
        }
        for id in spec.text_refs() {
            if !self.legacy && !self.recipes.is_buildable(id) {
                unavailable.push(id.to_string());
            } else if self.text(id).is_none() {
                pending.push(id.to_string());
            }
        }
        if !unavailable.is_empty() {
            Availability::Unavailable(unavailable)
        } else if !pending.is_empty() {
            Availability::Pending(pending)
        } else {
            Availability::Ready
        }
    }
}

/// Recipe ids (`^[A-Za-z0-9][A-Za-z0-9_.+-]{0,127}$`) never name a path outside their folder.
/// Legacy sprite names are looser (spaces, `@`), so only separators and a leading dot are refused.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 160 && !id.contains(['/', '\\', '\0']) && !id.starts_with('.')
}
