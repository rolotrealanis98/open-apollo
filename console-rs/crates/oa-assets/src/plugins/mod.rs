//! Plug-in panel art regenerated from the user's own UAD install.
//!
//! open-apollo ships plug-in panel specs and *recipes*, never UA's art. A
//! recipe names a file of the install (by SHA-256), a blob inside it (PE
//! resource or byte range, also by SHA-256), the decode and a few fixed
//! operations (crop, restack, Pillow-exact LANCZOS, compositing), and the
//! SHA-256 of the RGBA it must produce. [`build`] runs them and writes
//! `<out>/Images/<id>@2x.png`, `<out>/Text/` and `<out>/manifest.json`
//! (default `~/.local/share/open-apollo/generated`).

pub mod decode;
pub mod draw;
pub mod geometry;
pub mod img;
pub mod jpeg;
pub mod pe;
pub mod recipe;
pub mod resample;
pub mod run;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use serde_json::{Value, json};

use recipe::{Book, RecipeFile, Status};

include!(concat!(env!("OUT_DIR"), "/recipes_index.rs"));

/// Where generated art goes: `$OA_PLUGIN_ART`, else `<data dir>/generated`.
pub fn default_out() -> PathBuf {
    if let Some(x) = std::env::var_os("OA_PLUGIN_ART").filter(|v| !v.is_empty()) {
        return PathBuf::from(x);
    }
    crate::data_dir().join("generated")
}

/// The recipes compiled into this binary.
pub fn builtin_book() -> Result<Book, String> {
    use std::io::Read;
    let mut files = Vec::new();
    for (name, packed) in RECIPES {
        let mut text = Vec::new();
        flate2::read::DeflateDecoder::new(*packed)
            .read_to_end(&mut text)
            .map_err(|e| format!("recipes {name}: {e}"))?;
        let f: RecipeFile = serde_json::from_slice(&text).map_err(|e| format!("recipes {name}: {e}"))?;
        files.push(f);
    }
    Book::new(files)
}

/// Recipes from a folder of `*.json` files (maintainers; overrides the built-in set).
pub fn load_book(dir: &Path) -> Result<Book, String> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    names.sort();
    let mut files = Vec::new();
    for p in names {
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        files.push(serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?);
    }
    Book::new(files)
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Recipe folder instead of the built-in recipes.
    pub recipes: Option<PathBuf>,
    /// Worker threads (0: up to 8, by CPU count).
    pub jobs: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Built and its RGBA hash matched the recipe.
    Ok,
    /// Built, but the hash differs from the recipe (not written).
    Mismatch { got: String, want: String },
    /// Couldn't be built (input missing, blob changed, decode error, …).
    Failed(String),
    /// Not built here: drawn by plugin-ui, deferred or without a recipe.
    Skipped,
}

#[derive(Clone, Debug)]
pub struct SpriteResult {
    pub id: String,
    pub group: String,
    pub status: Status,
    pub basis: Option<String>,
    pub outcome: Outcome,
    /// RGBA SHA-256 of what was built (also set on a mismatch).
    pub rgba: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SpecResult {
    pub id: String,
    pub tier: Option<u8>,
    pub sprites: usize,
    pub built: usize,
    pub runtime: usize,
    pub deferred: usize,
    /// Buildable sprites that aren't (yet) in the output, or failed.
    pub missing: Vec<String>,
    pub ready: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub sprites: Vec<SpriteResult>,
    pub texts: Vec<(String, Outcome)>,
    pub specs: Vec<SpecResult>,
    pub out: PathBuf,
}

impl Report {
    pub fn failures(&self) -> impl Iterator<Item = &SpriteResult> {
        self.sprites.iter().filter(|s| matches!(s.outcome, Outcome::Mismatch { .. } | Outcome::Failed(_)))
    }
}

/// [`build_with`] with the built-in recipes. `specs` empty builds every spec;
/// `progress(done, total)` is called after each sprite.
pub fn build(
    root: &Path,
    out: &Path,
    specs: &[String],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Report, String> {
    build_with(root, out, specs, &Options::default(), progress)
}

/// Build the art of `specs` (all when empty) from the install at `root`
/// (the folder holding Windows' `C:\`, or a Wine prefix's `drive_c`) into `out`.
/// Every file is written atomically (temp file + rename) and `manifest.json` is merged.
pub fn build_with(
    root: &Path,
    out: &Path,
    specs: &[String],
    opts: &Options,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<Report, String> {
    let book = match &opts.recipes {
        Some(d) => load_book(d)?,
        None => builtin_book()?,
    };
    let groups: Vec<&RecipeFile> = if specs.is_empty() {
        book.files.iter().collect()
    } else {
        specs
            .iter()
            .map(|s| book.group(s).ok_or_else(|| format!("no recipes for spec {s}")))
            .collect::<Result<_, _>>()?
    };
    // sprite ids to make: what the groups draw
    let mut want: BTreeSet<String> = BTreeSet::new();
    for g in &groups {
        want.extend(g.uses.iter().cloned());
        want.extend(g.sprites.keys().cloned());
    }
    let mut results = Vec::new();
    let mut jobs: Vec<(String, String)> = Vec::new(); // (primary input, id)
    let mut needed_inputs: BTreeSet<String> = BTreeSet::new();
    for id in &want {
        let Some((f, s)) = book.sprite(id) else {
            results.push(SpriteResult {
                id: id.clone(),
                group: String::new(),
                status: Status::NoRecipe,
                basis: None,
                outcome: Outcome::Failed("no recipe".into()),
                rgba: None,
            });
            continue;
        };
        if !s.status.buildable() {
            results.push(SpriteResult {
                id: id.clone(),
                group: f.group.clone(),
                status: s.status,
                basis: s.basis.clone(),
                outcome: Outcome::Skipped,
                rgba: None,
            });
            continue;
        }
        let mut ins = Vec::new();
        let mut stack = vec![id.clone()];
        let mut seen = BTreeSet::new();
        while let Some(x) = stack.pop() {
            if !seen.insert(x.clone()) {
                continue;
            }
            if let Some((f, s)) = book.sprite(&x)
                && let Some(b) = &s.build
            {
                recipe::inputs(b, &f.blobs, &mut ins);
                recipe::deps(b, &mut stack);
            }
        }
        needed_inputs.extend(ins.iter().cloned());
        jobs.push((ins.first().cloned().unwrap_or_default(), id.clone()));
    }
    for g in &groups {
        for t in g.texts.values() {
            needed_inputs.insert(t.input.clone());
        }
    }
    let paths = resolve_inputs(root, &book, &needed_inputs);
    jobs.sort();

    let images = out.join("Images");
    std::fs::create_dir_all(&images).map_err(|e| format!("{}: {e}", images.display()))?;
    let total = jobs.len();
    let threads =
        if opts.jobs > 0 { opts.jobs } else { std::thread::available_parallelism().map_or(4, |n| n.get()).min(8) };
    // contiguous chunks share input files, so each worker opens few DLLs
    let chunks: Vec<&[(String, String)]> = jobs.chunk_by(|a, b| a.0 == b.0).collect();
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(SpriteResult, Option<(String, BTreeMap<String, String>)>)>();
    let book_ref = &book;
    let paths_ref = &paths;
    let mut built: BTreeMap<String, Value> = BTreeMap::new();
    std::thread::scope(|sc| {
        for _ in 0..threads.max(1) {
            let tx = tx.clone();
            let next = &next;
            let chunks = &chunks;
            let images = &images;
            sc.spawn(move || {
                let mut runner = run::Runner::new(book_ref, paths_ref);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(chunk) = chunks.get(i) else { break };
                    runner.clear_cache();
                    for (_, id) in chunk.iter() {
                        let r = make_one(&mut runner, id, images);
                        if tx.send(r).is_err() {
                            return;
                        }
                    }
                }
            });
        }
        drop(tx);
        let mut done = 0;
        for (r, rec) in rx {
            done += 1;
            if let Some((hash, inputs)) = rec {
                built.insert(
                    r.id.clone(),
                    json!({"rgba": hash, "status": r.status.as_str(), "inputs": inputs, "group": r.group}),
                );
            }
            results.push(r);
            progress(done, total);
        }
    });

    let texts = build_texts(&groups, &paths, out);
    results.sort_by(|a, b| a.id.cmp(&b.id));
    let manifest = merge_manifest(out, &book, built, &texts, root)?;
    let specs_out = readiness(&book, &manifest);
    write_atomic(
        &out.join("manifest.json"),
        (serde_json::to_string_pretty(&with_specs(manifest, &specs_out)).map_err(|e| e.to_string())? + "\n").as_bytes(),
    )
    .map_err(|e| format!("manifest: {e}"))?;
    Ok(Report { sprites: results, texts, specs: specs_out, out: out.to_path_buf() })
}

type Made = (SpriteResult, Option<(String, BTreeMap<String, String>)>);

fn make_one(runner: &mut run::Runner, id: &str, images: &Path) -> Made {
    let (f, s) = runner.book.sprite(id).expect("job ids come from the book");
    let mut r = SpriteResult {
        id: id.to_string(),
        group: f.group.clone(),
        status: s.status,
        basis: s.basis.clone(),
        outcome: Outcome::Ok,
        rgba: None,
    };
    let im = match runner.sprite(id, 0) {
        Ok(im) => im,
        Err(e) => {
            r.outcome = Outcome::Failed(e);
            return (r, None);
        }
    };
    let hash = im.hash();
    r.rgba = Some(hash.clone());
    if let Some(o) = &s.output {
        if (o.w, o.h) != (im.w, im.h) {
            r.outcome = Outcome::Failed(format!("size {}x{}, want {}x{}", im.w, im.h, o.w, o.h));
            return (r, None);
        }
        if let Some(want) = &o.rgba
            && *want != hash
        {
            r.outcome = Outcome::Mismatch { got: hash, want: want.clone() };
            return (r, None);
        }
    }
    if let Err(e) = write_png(&images.join(format!("{id}@2x.png")), &im) {
        r.outcome = Outcome::Failed(format!("write: {e}"));
        return (r, None);
    }
    let mut ins = Vec::new();
    if let Some(b) = &s.build {
        recipe::inputs(b, &f.blobs, &mut ins);
    }
    let inputs = ins.into_iter().filter_map(|i| runner.read.get(&i).map(|h| (i, h.clone()))).collect();
    (r, Some((hash, inputs)))
}

/// Find every input file under `root`: Powered Plug-in DLLs by file name, ignoring case.
pub fn resolve_inputs(root: &Path, book: &Book, needed: &BTreeSet<String>) -> HashMap<String, PathBuf> {
    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    let bases = [
        crate::ci_path(root, &["Program Files", "Universal Audio", "Powered Plugins"]),
        crate::ci_path(root, &["Program Files (x86)", "Universal Audio", "Powered Plugins"]),
        Some(root.to_path_buf()),
    ];
    for base in bases.into_iter().flatten() {
        for p in crate::detect::walk(&base, 2, &|p| crate::detect::has_ext(p, "dll")) {
            if let Some(n) = p.file_name() {
                by_name.entry(n.to_string_lossy().to_ascii_lowercase()).or_insert(p);
            }
        }
    }
    let mut out = HashMap::new();
    for id in needed {
        let file = book.files.iter().find_map(|f| f.inputs.get(id)).map(|i| i.file.clone());
        let name = file.unwrap_or_else(|| id.split_once(':').map_or(id.clone(), |(_, n)| n.to_string()));
        if let Some(p) = by_name.get(&name.to_ascii_lowercase()) {
            out.insert(id.clone(), p.clone());
        }
    }
    out
}

fn build_texts(groups: &[&RecipeFile], paths: &HashMap<String, PathBuf>, out: &Path) -> Vec<(String, Outcome)> {
    let dir = out.join("Text");
    let mut res = Vec::new();
    for g in groups {
        for (id, t) in &g.texts {
            let o = (|| -> Result<(), String> {
                let path = paths.get(&t.input).ok_or_else(|| format!("{} not found in the install", t.input))?;
                let b = std::fs::read(path).map_err(|e| e.to_string())?;
                let (bytes, ext) = text_value(&b, t)?;
                let got = img::sha256(&bytes);
                if got != t.output.sha256 {
                    return Err(format!("text hash {got} differs"));
                }
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                write_atomic(&dir.join(format!("{id}.{ext}")), &bytes).map_err(|e| e.to_string())
            })();
            res.push((id.clone(), o.map_or_else(Outcome::Failed, |_| Outcome::Ok)));
        }
    }
    res
}

/// A text recipe's bytes: `dll-string` → the UTF-8 string; `dll-string-table`
/// → compact JSON list. Hashes are over exactly these bytes.
fn text_value(b: &[u8], t: &recipe::Text) -> Result<(Vec<u8>, &'static str), String> {
    let cstr = |at: usize, max: usize| -> Result<String, String> {
        let s = b.get(at..(at + max).min(b.len())).ok_or("text offset outside the file")?;
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        match t.encoding.as_str() {
            "utf-8" => String::from_utf8(s[..end].to_vec()).map_err(|e| e.to_string()),
            "latin-1" => Ok(s[..end].iter().map(|&c| c as char).collect()),
            e => Err(format!("unknown encoding {e}")),
        }
    };
    match t.kind.as_str() {
        "dll-string" => Ok((cstr(t.at.file_offset, t.at.length.unwrap_or(1 << 16))?.into_bytes(), "txt")),
        "dll-string-table" => {
            let (stride, count) = (t.stride.ok_or("stride")?, t.count.ok_or("count")?);
            let items: Vec<String> =
                (0..count).map(|k| cstr(t.at.file_offset + k * stride, stride)).collect::<Result<_, _>>()?;
            Ok((serde_json::to_string(&items).map_err(|e| e.to_string())?.into_bytes(), "json"))
        }
        k => Err(format!("unknown text kind {k}")),
    }
}

fn merge_manifest(
    out: &Path,
    book: &Book,
    built: BTreeMap<String, Value>,
    texts: &[(String, Outcome)],
    root: &Path,
) -> Result<Value, String> {
    let path = out.join("manifest.json");
    let mut m: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    m["schema"] = json!(1);
    m["generator"] = json!(format!("oa-assets {}", env!("CARGO_PKG_VERSION")));
    m["recipesSchema"] = json!(2);
    m["from"] = json!(root);
    if !m["sprites"].is_object() {
        m["sprites"] = json!({});
    }
    for (id, v) in built {
        m["sprites"][&id] = v;
    }
    // drop entries whose file is gone or whose recipe no longer exists
    if let Some(obj) = m["sprites"].as_object_mut() {
        // keep what the current recipes still describe (same expected hash) and is on disk
        obj.retain(|id, v| {
            book.sprite(id).is_some_and(|(_, s)| current(s, v))
                && out.join("Images").join(format!("{id}@2x.png")).is_file()
        });
    }
    if !m["texts"].is_object() {
        m["texts"] = json!({});
    }
    for (id, o) in texts {
        if *o == Outcome::Ok {
            m["texts"][id] = json!({"verified": true});
        }
    }
    Ok(m)
}

/// A manifest entry still matches the recipe: built from it, with the hash the recipe expects.
fn current(s: &recipe::Sprite, entry: &Value) -> bool {
    let want = s.output.as_ref().and_then(|o| o.rgba.as_deref());
    s.status.buildable() && want.is_none_or(|w| entry["rgba"].as_str() == Some(w))
}

fn readiness(book: &Book, manifest: &Value) -> Vec<SpecResult> {
    let have = |id: &str| {
        let e = &manifest["sprites"][id];
        !e.is_null() && book.sprite(id).is_some_and(|(_, s)| current(s, e))
    };
    book.files
        .iter()
        .map(|g| {
            let mut s = SpecResult {
                id: g.group.clone(),
                tier: g.tier,
                sprites: g.uses.len(),
                built: 0,
                runtime: 0,
                deferred: 0,
                missing: Vec::new(),
                ready: false,
            };
            for id in &g.uses {
                match book.sprite(id).map(|(_, r)| r.status) {
                    Some(Status::Runtime) => s.runtime += 1,
                    Some(Status::Deferred) => s.deferred += 1,
                    Some(st) if st.buildable() && have(id) => s.built += 1,
                    _ => s.missing.push(id.clone()),
                }
            }
            s.ready = s.missing.is_empty() && s.deferred == 0;
            s
        })
        .collect()
}

fn with_specs(mut m: Value, specs: &[SpecResult]) -> Value {
    let mut o = serde_json::Map::new();
    for s in specs {
        o.insert(
            s.id.clone(),
            json!({
                "ready": s.ready, "tier": s.tier, "sprites": s.sprites, "built": s.built,
                "runtime": s.runtime, "deferred": s.deferred, "missing": s.missing.len(),
            }),
        );
    }
    m["specs"] = Value::Object(o);
    m
}

/// Write via a temp file in the same folder and rename over the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp =
        dir.join(format!(".{}.tmp{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn write_png(path: &Path, im: &img::Rgba) -> io::Result<()> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, im.w, im.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut w = enc.write_header().map_err(io::Error::other)?;
        w.write_image_data(&im.px).map_err(io::Error::other)?;
    }
    write_atomic(path, &buf)
}
