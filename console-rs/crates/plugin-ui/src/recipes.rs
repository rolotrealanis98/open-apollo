//! The release recipes (`recipes/*.json`, `recipesSchema` 1 or 2): what plugin-ui needs to know
//! about each sprite and text id a schema-2 spec references. The importer (`oa-assets`) runs the
//! recipes against the user's own UA install and writes the results under the art root
//! ([`crate::library::ArtLocator`]); plugin-ui never runs them. It reads only an id's `status`,
//! its `output.sha256` (texts are checked against it) and a text recipe's `kind`; every other
//! field is the importer's and is ignored here, so the recipe format can grow.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// A recipe's `status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecipeStatus {
    /// Checked against the golden art (`oa-assets verify --golden`).
    Verified,
    /// Concrete, not yet checked.
    Unverified,
    /// Concrete, made our way on purpose (e.g. 1x + LANCZOS where the reference was hand art).
    Fallback,
    /// The source is not traced to an install file yet (maintainer work; not shippable).
    NeedsTracer,
    /// The art exists only in live captures; the importer cannot make it.
    NoRecipe,
    /// Postponed on purpose (e.g. the code-drawn tier-4 panels): the spec opens as a generic list.
    Deferred,
    /// Drawn by plugin-ui's own elements (toolbars, labels, shapes, lamps), never generated: a spec
    /// that still names such an id has a hole where our element belongs.
    Runtime,
    /// A status this runtime does not know (treated as not buildable).
    Other,
}

impl RecipeStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "verified" => Self::Verified,
            "unverified" => Self::Unverified,
            "fallback" => Self::Fallback,
            "needs-tracer" => Self::NeedsTracer,
            "no-recipe" => Self::NoRecipe,
            "deferred" => Self::Deferred,
            "runtime" => Self::Runtime,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Fallback => "fallback",
            Self::NeedsTracer => "needs-tracer",
            Self::NoRecipe => "no-recipe",
            Self::Deferred => "deferred",
            Self::Runtime => "runtime",
            Self::Other => "other",
        }
    }

    /// The importer can make this id from an install (verified, unverified, fallback).
    pub fn is_buildable(self) -> bool {
        matches!(self, Self::Verified | Self::Unverified | Self::Fallback)
    }
}

impl<'de> Deserialize<'de> for RecipeStatus {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(d)?))
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct RecipeOutput {
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub w: Option<u32>,
    #[serde(default)]
    pub h: Option<u32>,
    /// Text recipes: byte length (`dll-string`) or item count (`dll-string-table`).
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub items: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SpriteRecipe {
    pub status: RecipeStatus,
    #[serde(default)]
    pub output: Option<RecipeOutput>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TextRecipe {
    pub status: RecipeStatus,
    /// `dll-string` (one text) or `dll-string-table` (a label list).
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub output: Option<RecipeOutput>,
}

impl TextRecipe {
    pub fn is_list(&self) -> bool {
        self.kind.as_deref() == Some("dll-string-table")
    }
}

#[derive(Deserialize)]
struct RecipeFile {
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    sprites: HashMap<String, SpriteRecipe>,
    #[serde(default)]
    texts: HashMap<String, TextRecipe>,
}

/// Every recipe of a `recipes/` folder, by id (one namespace for sprites and texts).
#[derive(Clone, Debug, Default)]
pub struct Recipes {
    pub sprites: HashMap<String, SpriteRecipe>,
    pub texts: HashMap<String, TextRecipe>,
    /// The file each id came from (duplicate reports).
    origin: HashMap<String, String>,
}

impl Recipes {
    /// Merges `dir/*.json` (sorted by name). An id defined twice, in one file's two maps or in two
    /// files, is an error.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        files.sort();
        let mut out = Self::default();
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            out.merge_json(&text, &name)?;
        }
        Ok(out)
    }

    /// Adds one recipes file (`name` is used in error messages).
    pub fn merge_json(&mut self, text: &str, name: &str) -> Result<(), String> {
        let file: RecipeFile = serde_json::from_str(text).map_err(|e| format!("{name}: {e}"))?;
        let label = file.group.map(|g| format!("{name} ({g})")).unwrap_or_else(|| name.to_string());
        for (id, r) in file.sprites {
            self.claim(&id, &label)?;
            self.sprites.insert(id, r);
        }
        for (id, r) in file.texts {
            self.claim(&id, &label)?;
            self.texts.insert(id, r);
        }
        Ok(())
    }

    fn claim(&mut self, id: &str, file: &str) -> Result<(), String> {
        if let Some(prev) = self.origin.insert(id.to_string(), file.to_string()) {
            return Err(format!("recipe id {id:?} is defined in {prev} and again in {file}"));
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty() && self.texts.is_empty()
    }

    /// The status of a sprite or text id; None when no recipe names it.
    pub fn status(&self, id: &str) -> Option<RecipeStatus> {
        self.sprites.get(id).map(|r| r.status).or_else(|| self.texts.get(id).map(|r| r.status))
    }

    /// True when the importer can make `id`.
    pub fn is_buildable(&self, id: &str) -> bool {
        self.status(id).is_some_and(RecipeStatus::is_buildable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_files_and_rejects_duplicates() {
        let mut r = Recipes::default();
        r.merge_json(
            r#"{"recipesSchema": 1, "group": "a", "inputs": {}, "sprites": {"k1": {"status": "verified", "sources": [], "ops": [],
               "output": {"w": 2, "h": 2, "sha256": "00"}}, "k2": {"status": "deferred"}},
               "texts": {"t1": {"status": "unverified", "kind": "dll-string-table", "output": {"sha256": "ab", "items": 3}}}}"#,
            "a.json",
        )
        .unwrap();
        assert_eq!(r.status("k1"), Some(RecipeStatus::Verified));
        assert_eq!(r.status("k2"), Some(RecipeStatus::Deferred));
        assert!(r.is_buildable("k1") && !r.is_buildable("k2") && r.is_buildable("t1"));
        assert!(r.texts["t1"].is_list());
        assert_eq!(r.status("nope"), None);
        assert_eq!(RecipeStatus::parse("future-status"), RecipeStatus::Other);
        assert!(!RecipeStatus::parse("runtime").is_buildable());
        let err = r.merge_json(r#"{"group": "b", "sprites": {"t1": {"status": "verified"}}}"#, "b.json").unwrap_err();
        assert!(err.contains("t1") && err.contains("a.json") && err.contains("b.json"), "{err}");
    }
}

/// The importer's `<art root>/manifest.json` (oa-assets): what it built from this install and, per
/// spec, whether every sprite the spec uses is built or drawn at runtime with none deferred.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Manifest {
    #[serde(default)]
    pub schema: Option<i64>,
    #[serde(default)]
    pub sprites: HashMap<String, ManifestSprite>,
    #[serde(default)]
    pub texts: HashMap<String, ManifestText>,
    #[serde(default)]
    pub specs: HashMap<String, ManifestSpec>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ManifestSprite {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub rgba: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ManifestText {
    #[serde(default)]
    pub verified: bool,
}

/// Counts per spec; `ready` = every used sprite built or runtime, none deferred or without a recipe.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ManifestSpec {
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub tier: Option<i64>,
    #[serde(default)]
    pub sprites: u32,
    #[serde(default)]
    pub built: u32,
    #[serde(default)]
    pub runtime: u32,
    #[serde(default)]
    pub deferred: u32,
    #[serde(default)]
    pub missing: u32,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}
