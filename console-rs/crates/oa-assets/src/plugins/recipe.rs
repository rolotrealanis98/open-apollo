//! Recipes (schema `plugin-recipes-v2`, see `data/plugin-recipes-v2.schema.json`).
//! A recipe holds only facts about UA's files (names, SHA-256 hashes,
//! resource ids, offsets) and our operations: never pixels or UA text.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// One file per spec: the sprites that spec introduces plus every id it draws.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecipeFile {
    pub recipes_schema: u32,
    /// The spec (capture) id, e.g. `pultec_eqp_1a`.
    pub group: String,
    /// Coverage tier of the spec (0 best .. 4 code-drawn), for reports.
    #[serde(default)]
    pub tier: Option<u8>,
    /// Not a spec: art and texts plugin-ui draws with (e.g. the "chrome" group).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    /// Shared groups: the ids a spec also needs (built with it, counted for its
    /// readiness). Ids attached to no spec are built with every spec.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attach: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub inputs: BTreeMap<String, Input>,
    /// Every sprite id the spec draws, including ids defined by other groups.
    #[serde(default)]
    pub uses: Vec<String>,
    /// Blobs that `load` steps name by key (`{"op": "load", "blob": "b0"}`), so a
    /// blob used many times is spelled out once.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub blobs: BTreeMap<String, Blob>,
    #[serde(default)]
    pub sprites: BTreeMap<String, Sprite>,
    #[serde(default)]
    pub texts: BTreeMap<String, Text>,
}

/// A blob of an input file: a PE resource `TYPE/NAME/LANG`, or `carve: [offset,
/// length]` (PNG/JPEG/SVG embedded anywhere in the file), and how to decode it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Blob {
    #[serde(rename = "in")]
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub res: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carve: Option<[u64; 2]>,
    /// SHA-256 of the blob bytes.
    pub sha: String,
    /// "deflate-raw": the bytes are a raw DEFLATE stream (a ZIP member) that
    /// inflates to exactly `size` bytes before decoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<usize>,
    pub dec: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    pub kind: String,
    pub file: String,
    pub root: String,
    /// Versions the recipes were made from; others are tried and checked per blob.
    #[serde(default)]
    pub sha256: Vec<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub third_party: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Built from the install; the output hash equals the reference art's.
    Verified,
    /// Built from the install on purpose differently from the reference art
    /// (e.g. LANCZOS 2x of the DLL's 1x art); the output hash is ours.
    Fallback,
    /// Drawn by plugin-ui at run time (`owner`, `kind`).
    Runtime,
    /// A code-drawn panel that is out of scope for now; the spec is not ready.
    Deferred,
    /// Nothing in the install can make it.
    NoRecipe,
}

impl Status {
    pub fn buildable(self) -> bool {
        matches!(self, Status::Verified | Status::Fallback)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Verified => "verified",
            Status::Fallback => "fallback",
            Status::Runtime => "runtime",
            Status::Deferred => "deferred",
            Status::NoRecipe => "no-recipe",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sprite {
    pub status: Status,
    /// Why the recipe looks the way it does: exact, derived, equivalent,
    /// dll-composite, similar (another state or frame of the art), synth, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    /// Runtime/deferred: who draws it (e.g. `plugin-ui/chrome`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Runtime/deferred: chrome, text, vector, scaled, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<Vec<Op>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub w: u32,
    pub h: u32,
    /// SHA-256 over `u32le w, u32le h, RGBA` with RGB zeroed where alpha is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rgba: Option<String>,
}

/// A pipeline step. The first step of every pipeline is a source
/// (`load`, `canvas` or `sprite`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    /// Decode a blob: either `blob` (a key of the file's `blobs`) or the blob's fields inline.
    Load {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob: Option<String>,
        #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        res: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        carve: Option<[u64; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inflate: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dec: Option<String>,
    },
    /// A transparent `w`×`h` layer with Pillow's `ImageDraw.line` from (x0, y0)
    /// to (x1, y1) set to `rgba` (no blending); width 0 draws nothing.
    Line { w: u32, h: u32, xy: [i32; 4], width: u32, rgba: [u8; 4] },
    /// A new image filled with `rgba` (default transparent black).
    Canvas {
        w: u32,
        h: u32,
        #[serde(default)]
        rgba: [u8; 4],
    },
    /// The output of another sprite recipe.
    Sprite { id: String },
    /// `[x, y, w, h]`; area outside the image is transparent (Pillow).
    Crop {
        #[serde(rename = "box")]
        bx: [i64; 4],
    },
    /// Pillow LANCZOS to twice the size.
    Lanczos2x,
    /// Pillow `resize((w, h), filter, box)`; filter: lanczos, bicubic, bilinear, hamming, box, nearest.
    Resize {
        w: u32,
        h: u32,
        filter: String,
        #[serde(default, rename = "box", skip_serializing_if = "Option::is_none")]
        bx: Option<[f32; 4]>,
    },
    /// Parts placed one after another (no blending) along `axis` ("y" default, or "x").
    Stack {
        parts: Vec<Vec<Op>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axis: Option<String>,
    },
    /// Place `src` at `at` on the current image. blend: "over" (alpha
    /// composite, default), "replace" (paste), "mask" (paste through src's alpha).
    Paste {
        src: Vec<Op>,
        at: [i64; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blend: Option<String>,
    },
    /// JUCE 1.52 fader: `thumb` over a copy of `track` per pixel step of travel
    /// (`k` = pixels per frame), frames stacked vertically.
    ThumbFader { track: Vec<Op>, thumb: Vec<Op>, w: u32, h: u32, k: u32 },
    /// A thumb moved across `n` frames of `w`×`h`. Per axis `[base, span, dir]`:
    /// position = base + round(t * span), t = v (dir 1), 1 - v (dir -1) or 0,
    /// v = i / (n - 1); composited at (max(x, 0), i*h + max(y, 0)). `clip`: the
    /// thumb is cut at its frame's right and bottom edge instead of running on.
    Slide {
        thumb: Vec<Op>,
        n: u32,
        w: u32,
        h: u32,
        x: [i64; 3],
        y: [i64; 3],
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        clip: bool,
    },
    /// A level meter from its lit image: frame k of `n` shows the bar filled to k/(n-1).
    MeterFill { n: u32, vertical: bool },
    /// `n` frames, each the "over" composite of frame k of every layer (a layer with `frames` 1 repeats).
    Layers { n: u32, layers: Vec<Layer> },
    /// Mirror: "x" left-right, "y" top-bottom.
    Flip { axis: String },
    /// Pillow `transform(size, AFFINE, matrix, BICUBIC)`: output (x, y) samples the
    /// input at (a(x+.5) + b(y+.5) + c, d(x+.5) + e(y+.5) + f).
    Affine { matrix: [f64; 6], resample: String },
    /// Brainworx "disabled" tint: every pixel with alpha > 0 moves toward `rgb`
    /// by `a`/256 after scaling colour by alpha/256, rounded half to even (integer form).
    PremulTint { rgb: [u8; 3], a: u32 },
    /// The image as `n` equal vertical frames; output frame i is input frame `order[i]` (exact copy).
    Reorder { n: u32, order: Vec<u32> },
    /// Linear alpha ramp `px` wide at every edge: alpha *= min(d_y/px, 1) * min(d_x/px, 1)
    /// in f64 (d = distance to the nearer edge in pixels), truncated.
    Feather { px: f64 },
    /// Colourise (multiply): each colour channel becomes `(c * rgb + 127) / 255`; alpha unchanged.
    Tint { rgb: [u8; 3] },
    /// JUCE's load-time premultiply (`(c * a + 127) >> 8`, skipped at alpha 255)
    /// undone by an integer divide, as the art looked in the plug-in's memory.
    JucePremulRoundtrip,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub src: Vec<Op>,
    pub frames: u32,
}

/// A text recipe: a string (or string table) read from a DLL.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Text {
    pub status: Status,
    pub kind: String,
    pub input: String,
    pub at: TextAt,
    #[serde(default)]
    pub stride: Option<usize>,
    #[serde(default)]
    pub count: Option<usize>,
    pub encoding: String,
    pub output: TextOutput,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextAt {
    pub file_offset: usize,
    #[serde(default)]
    pub length: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextOutput {
    pub sha256: String,
    #[serde(default)]
    pub bytes: Option<usize>,
    #[serde(default)]
    pub items: Option<usize>,
}

/// All recipe files, with sprite ids resolved across files.
#[derive(Default)]
pub struct Book {
    pub files: Vec<RecipeFile>,
    /// sprite id → (file index)
    pub sprite_at: BTreeMap<String, usize>,
    pub text_at: BTreeMap<String, usize>,
}

impl Book {
    pub fn new(files: Vec<RecipeFile>) -> Result<Book, String> {
        let mut b = Book { files, ..Default::default() };
        for (i, f) in b.files.iter().enumerate() {
            if f.recipes_schema != 2 {
                return Err(format!("{}: recipesSchema {} (want 2)", f.group, f.recipes_schema));
            }
            for id in f.sprites.keys() {
                if b.sprite_at.insert(id.clone(), i).is_some() {
                    return Err(format!("sprite {id} is defined twice"));
                }
            }
            for id in f.texts.keys() {
                if b.text_at.insert(id.clone(), i).is_some() {
                    return Err(format!("text {id} is defined twice"));
                }
            }
        }
        Ok(b)
    }

    pub fn text(&self, id: &str) -> Option<&Text> {
        self.files[*self.text_at.get(id)?].texts.get(id)
    }

    pub fn sprite(&self, id: &str) -> Option<(&RecipeFile, &Sprite)> {
        let f = &self.files[*self.sprite_at.get(id)?];
        Some((f, &f.sprites[id]))
    }

    pub fn group(&self, id: &str) -> Option<&RecipeFile> {
        self.files.iter().find(|f| f.group == id)
    }

    /// The sprite and text ids spec `spec` needs from shared groups: those
    /// attached to it, plus those attached to no spec.
    pub fn shared_for(&self, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        for f in self.files.iter().filter(|f| f.shared) {
            let attached: BTreeSet<&String> = f.attach.values().flatten().collect();
            out.extend(f.attach.get(spec).into_iter().flatten().cloned());
            out.extend(f.sprites.keys().chain(f.texts.keys()).filter(|id| !attached.contains(id)).cloned());
        }
        out
    }
}

/// Sprite ids a pipeline depends on (`sprite` steps), recursively through sub-pipelines.
pub fn deps(ops: &[Op], out: &mut Vec<String>) {
    for op in ops {
        match op {
            Op::Sprite { id } => out.push(id.clone()),
            Op::Stack { parts, .. } => parts.iter().for_each(|p| deps(p, out)),
            Op::Paste { src, .. } => deps(src, out),
            Op::ThumbFader { track, thumb, .. } => {
                deps(track, out);
                deps(thumb, out);
            }
            Op::Slide { thumb, .. } => deps(thumb, out),
            Op::Layers { layers, .. } => layers.iter().for_each(|l| deps(&l.src, out)),
            _ => {}
        }
    }
}

/// Input ids a pipeline reads.
pub fn inputs(ops: &[Op], blobs: &BTreeMap<String, Blob>, out: &mut Vec<String>) {
    for op in ops {
        match op {
            Op::Load { input: Some(i), .. } => out.push(i.clone()),
            Op::Load { blob: Some(b), .. } => out.extend(blobs.get(b).map(|b| b.input.clone())),
            Op::Stack { parts, .. } => parts.iter().for_each(|p| inputs(p, blobs, out)),
            Op::Paste { src, .. } => inputs(src, blobs, out),
            Op::ThumbFader { track, thumb, .. } => {
                inputs(track, blobs, out);
                inputs(thumb, blobs, out);
            }
            Op::Slide { thumb, .. } => inputs(thumb, blobs, out),
            Op::Layers { layers, .. } => layers.iter().for_each(|l| inputs(&l.src, blobs, out)),
            _ => {}
        }
    }
}
