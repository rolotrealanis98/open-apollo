//! `oa-assets dsp`: every Powered plug-in's DSP modules, from the user's own
//! Windows UAD install, for hosting plug-ins on the Apollo's DSP later.
//!
//! Each UAD-2 Powered plug-in DLL carries its DSP code and data as "Bill"
//! records ([`firmware::all_bill_records`]). This reads every DLL under
//! `UAD-2 Powered Plugins` (and UA's System32 DLLs, which carry a few
//! SDK-level modules) and writes, under `--out` (default [`DEFAULT_OUT`]):
//!
//! - `modules/<sha256>.bill`: each distinct record once, exactly as the DLL
//!   holds it (on-disk form, unmodified), named by its SHA-256;
//! - `manifest.json`: for each DLL its name, UA.xml entries, Powered and VST3
//!   manifest identities, version and SHA-256, and the modules it carries (id,
//!   kind, arch, w4, length, SHA-256, offset), so hosting code can find "the
//!   modules plug-in X needs" ([`find_plugin`], [`module_path`]).
//!
//! Records are kept by hash: there are no pins for arbitrary plug-in versions.
//! A module is `verified` only when its hash is one `oa-assets firmware`
//! pins (the Console strip modules). Writes are atomic, files 0644 and folders
//! 0755. A rerun rewrites nothing that is already right and removes store files
//! the new manifest no longer lists. The source is only read.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::ci_path;
use crate::detect::{has_ext, is_ua_name, tools_dir, walk};
use crate::firmware::{self, BillRecord, Event, Pins, sha256_hex};

/// Next to the strip modules `oa-assets firmware` writes.
pub const DEFAULT_OUT: &str = "/lib/firmware/universal-audio/plugins";
pub const MANIFEST: &str = "manifest.json";
pub const MANIFEST_FORMAT: &str = "open-apollo-plugin-dsp/1";
/// The content-addressed store, relative to `--out`.
pub const STORE: &str = "modules";

/// UA's System32 DLLs that carry Bill records of their own.
const SYSTEM_DLLS: [&str; 2] = ["UAD2SDK.dll", "UAD2DriverClient.dll"];

const EXIT_NOTHING: u8 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Scan and report, write nothing.
    pub dry_run: bool,
    /// Parallel readers; 0 picks up to 4.
    pub jobs: usize,
}

#[derive(Clone, Debug)]
pub struct Report {
    /// What was (or, with `dry_run`, would be) written to `<out>/manifest.json`.
    pub manifest: Value,
    /// Store files written, already present with the right bytes, and removed as stale.
    pub written: usize,
    pub kept: usize,
    pub removed: usize,
    /// 0 done (warnings allowed), 1 no Powered plug-ins found.
    pub exit: u8,
}

impl Report {
    pub fn totals(&self) -> &Value {
        &self.manifest["totals"]
    }
}

// ---- where the inputs are ----

/// The Powered plug-in DLLs: `Universal Audio\Powered Plugins\UAD-2 Powered Plugins\*.dll`.
pub fn plugin_dlls(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = ["Program Files", "Program Files (x86)"]
        .iter()
        .filter_map(|pf| ci_path(root, &[pf, "Universal Audio", "Powered Plugins", "UAD-2 Powered Plugins"]))
        .flat_map(|d| walk(&d, 0, &|p| has_ext(p, "dll") && p.is_file()))
        .collect();
    out.dedup();
    out
}

fn system_dlls(root: &Path) -> Vec<PathBuf> {
    SYSTEM_DLLS.iter().filter_map(|n| ci_path(root, &["Windows", "System32", n])).filter(|p| p.is_file()).collect()
}

/// Whether `root` holds Powered plug-in DLLs.
pub fn has_inputs(root: &Path) -> bool {
    !plugin_dlls(root).is_empty()
}

// ---- plug-in identities ----

/// A `<plugin id=…>` entry of UA.xml's `<plugins>` registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UaXmlEntry {
    pub id: u32,
    pub title: String,
    pub render_id: Option<u32>,
    pub vst_ids: Vec<String>,
}

impl UaXmlEntry {
    fn to_json(&self) -> Value {
        json!({"id": self.id, "title": self.title, "renderid": self.render_id, "vstids": self.vst_ids})
    }
}

/// The registry entries of a UA.xml text. Category lists (`<plugin title=…/>`
/// without an id) are not registry entries.
pub fn parse_ua_xml(text: &str) -> Vec<UaXmlEntry> {
    let Some(start) = text.find("<plugins>") else { return Vec::new() };
    let body = &text[start..];
    let body = &body[..body.find("</plugins>").unwrap_or(body.len())];
    body.split("<plugin ")
        .skip(1)
        .filter_map(|tag| {
            let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
            let a = xml_attrs(tag.trim_end_matches('/'));
            let get = |k: &str| a.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
            Some(UaXmlEntry {
                id: get("id")?.trim().parse().ok()?,
                title: get("title").unwrap_or_default().to_string(),
                render_id: get("renderid").and_then(|r| r.trim().parse().ok()),
                vst_ids: split_ids(get("vstids").unwrap_or_default()),
            })
        })
        .collect()
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect()
}

/// `name="value"` pairs of one tag, entities decoded.
fn xml_attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag;
    while let Some(eq) = rest.find('=') {
        let name = rest[..eq].split_whitespace().last().unwrap_or_default().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
        let Some(end) = after[1..].find(q) else { break };
        out.push((name, xml_unescape(&after[1..1 + end])));
        rest = &after[end + 2..];
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// What `Powered Plugins\Manifest\<DLL stem>\manifest.json` says about a plug-in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginMeta {
    pub name: Option<String>,
    pub plugin_id: Option<String>,
    pub render_id: Option<u32>,
    pub vst_ids: Vec<String>,
}

/// Manifest version 2 writes `renderID`/`vstID`, version 1 `renderid`/`vstids`.
pub fn parse_plugin_manifest(v: &Value) -> PluginMeta {
    let text = |k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let number =
        |k: &str| v[k].as_u64().map(|n| n as u32).or_else(|| v[k].as_str().and_then(|s| s.trim().parse().ok()));
    let mut vst_ids = split_ids(v["vstID"].as_str().unwrap_or_default());
    for id in split_ids(v["vstids"].as_str().unwrap_or_default()) {
        if !vst_ids.contains(&id) {
            vst_ids.push(id);
        }
    }
    PluginMeta {
        name: text("name"),
        plugin_id: text("plugin_id"),
        render_id: number("renderID").or_else(|| number("renderid")),
        vst_ids,
    }
}

/// One UA VST3 bundle: `Contents\Resources\{manifest,moduleinfo}.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Vst3 {
    /// Relative to the VST3 folder.
    bundle: String,
    plugin_id: String,
    bundle_id: Option<String>,
    /// The Audio Module Class CID a VST3 host knows the plug-in by.
    cid: Option<String>,
}

/// UA's VST3 bundles by the `plugin_id` their manifest names.
fn vst3_index(root: &Path) -> BTreeMap<String, Vec<Vst3>> {
    let mut out: BTreeMap<String, Vec<Vst3>> = BTreeMap::new();
    let Some(dir) = ci_path(root, &["Program Files", "Common Files", "VST3"]) else { return out };
    let bundles = walk(&dir, 3, &|p| has_ext(p, "vst3") && p.strip_prefix(&dir).is_ok_and(is_ua_name));
    for b in bundles {
        let read = |name: &str| -> Option<Value> {
            let p = ci_path(&b, &["Contents", "Resources", name])?;
            serde_json::from_slice(&fs::read(p).ok()?).ok()
        };
        let Some(m) = read("manifest.json") else { continue };
        let Some(plugin_id) = m["plugin_id"].as_str().filter(|s| !s.is_empty()) else { continue };
        let cid = read("moduleinfo.json").and_then(|mi| {
            mi["Classes"]
                .as_array()?
                .iter()
                .find(|c| c["Category"] == "Audio Module Class")
                .and_then(|c| c["CID"].as_str().map(str::to_string))
        });
        let v = Vst3 {
            bundle: firmware::rel(&dir, &b),
            plugin_id: plugin_id.to_string(),
            bundle_id: m["bundle_id"].as_str().map(str::to_string),
            cid,
        };
        out.entry(v.plugin_id.clone()).or_default().push(v);
    }
    out
}

// ---- scanning ----

/// One DLL's Bill records and identity.
struct DllScan {
    path: PathBuf,
    size: usize,
    sha256: String,
    version: Option<String>,
    file_version: Option<String>,
    /// Each record and its SHA-256.
    records: Vec<(BillRecord, String)>,
}

/// Read one DLL; its distinct record bytes go into `store`.
fn scan_dll(path: &Path, store: &Mutex<BTreeMap<String, Vec<u8>>>) -> io::Result<DllScan> {
    let data = fs::read(path)?;
    let mut records = Vec::new();
    for rec in firmware::all_bill_records(&data) {
        let bytes = &data[rec.offset..rec.offset + rec.len];
        let sha = sha256_hex(bytes);
        let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
        s.entry(sha.clone()).or_insert_with(|| bytes.to_vec());
        records.push((rec, sha));
    }
    Ok(DllScan {
        path: path.to_path_buf(),
        size: data.len(),
        sha256: sha256_hex(&data),
        version: crate::pe::string_file_version(&data),
        file_version: crate::pe::file_version_of(&data),
        records,
    })
}

/// Scan `paths` on `jobs` threads; results in `paths` order.
fn scan_all(
    paths: &[PathBuf],
    jobs: usize,
    store: &Mutex<BTreeMap<String, Vec<u8>>>,
) -> Vec<(PathBuf, io::Result<DllScan>)> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<io::Result<DllScan>>>> = Mutex::new((0..paths.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..jobs.clamp(1, paths.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = paths.get(i) else { break };
                    let r = scan_dll(p, store);
                    results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    paths.iter().cloned().zip(results.into_iter().map(|r| r.expect("every path scanned"))).collect()
}

fn id_hex(id: u32) -> String {
    format!("{id:03x}")
}

fn module_ref(rec: &BillRecord, sha: &str) -> Value {
    json!({"id": id_hex(rec.id), "kind": rec.kind(), "arch": rec.arch(), "w2": rec.w2, "w4": rec.w4,
           "length": rec.len, "sha256": sha, "offset": rec.offset})
}

/// The store file of module `sha256`, relative to `--out`.
pub fn store_name(sha256: &str) -> String {
    format!("{STORE}/{sha256}.bill")
}

fn is_store_file_name(name: &str) -> bool {
    name.strip_suffix(".bill")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}

/// Whether `path` is a regular file holding exactly the bytes hashing to `sha256`.
fn holds(path: &Path, len: usize, sha256: &str) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == len as u64)
        && fs::read(path).is_ok_and(|b| sha256_hex(&b) == sha256)
}

// ---- collect ----

/// Collect every Powered plug-in's modules from `root` into `out`.
pub fn collect(root: &Path, out: &Path, pins: &Pins, opts: Options, log: &mut dyn FnMut(Event)) -> io::Result<Report> {
    log(Event::Header("Reading your UAD install's plug-in DSP modules".into()));
    log(Event::Info(format!("From {} (read-only)", root.display())));
    let uad_version = crate::detect::uad_version(root);
    if let Some(v) = &uad_version {
        log(Event::Info(format!("UAD software {v}")));
    }
    if !opts.dry_run {
        log(Event::Info(format!("To {}", out.display())));
    }
    let plugins = plugin_dlls(root);
    let system = system_dlls(root);
    let empty = |log: &mut dyn FnMut(Event)| {
        log(Event::Fail("No Powered plug-in DLLs in this install (UAD-2 Powered Plugins folder)".into()));
        Report { manifest: Value::Null, written: 0, kept: 0, removed: 0, exit: EXIT_NOTHING }
    };
    if plugins.is_empty() {
        return Ok(empty(log));
    }
    let jobs =
        if opts.jobs > 0 { opts.jobs } else { std::thread::available_parallelism().map_or(2, |n| n.get()).min(4) };
    log(Event::Info(format!("Scanning {} plug-in DLLs and {} UAD system DLLs", plugins.len(), system.len())));

    let store = Mutex::new(BTreeMap::new());
    let all: Vec<PathBuf> = plugins.iter().chain(&system).cloned().collect();
    let mut scans = Vec::new();
    for (path, r) in scan_all(&all, jobs, &store) {
        match r {
            Ok(s) => scans.push(s),
            Err(e) => log(Event::Warn(format!("can't read {}: {e}", path.display()))),
        }
    }
    let store = store.into_inner().unwrap_or_else(|e| e.into_inner());
    let is_plugin = |s: &DllScan| plugins.contains(&s.path);
    if !scans.iter().any(is_plugin) {
        return Ok(empty(log));
    }

    let ua_xml = tools_dir(root)
        .and_then(|t| ci_path(&t, &["UA.xml"]))
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| parse_ua_xml(&t))
        .unwrap_or_default();
    let vst3 = vst3_index(root);
    let pinned: BTreeMap<&str, String> =
        pins.modules.iter().flat_map(|m| m.known.iter().map(move |k| (k.sha256.as_str(), m.file_name()))).collect();

    // Module table: one entry per distinct record.
    let mut users: BTreeMap<&str, BTreeSet<&Path>> = BTreeMap::new();
    let mut first: BTreeMap<&str, &BillRecord> = BTreeMap::new();
    for s in &scans {
        for (rec, sha) in &s.records {
            users.entry(sha).or_default().insert(&s.path);
            first.entry(sha).or_insert(rec);
        }
    }
    let mut modules: Vec<(&str, &BillRecord)> = first.into_iter().collect();
    modules.sort_by_key(|(sha, r)| (r.id, r.w2, *sha));
    let records: usize = scans.iter().filter(|s| is_plugin(s)).map(|s| s.records.len()).sum();
    let bytes: usize = modules.iter().map(|(_, r)| r.len).sum();
    let module_json: Vec<Value> = modules
        .iter()
        .map(|(sha, r)| {
            let mut m = json!({"sha256": sha, "file": store_name(sha), "id": id_hex(r.id), "kind": r.kind(),
                               "arch": r.arch(), "w2": r.w2, "w4": r.w4, "length": r.len,
                               "verified": pinned.contains_key(sha), "used_by": users[sha].len()});
            if let Some(name) = pinned.get(sha) {
                m["pinned_as"] = json!(name);
            }
            m
        })
        .collect();

    let dll_json = |s: &DllScan| -> Value {
        let mods: Vec<Value> = s.records.iter().map(|(r, sha)| module_ref(r, sha)).collect();
        json!({"dll": firmware::file_label(&s.path), "path": firmware::rel(root, &s.path), "size": s.size,
               "sha256": s.sha256, "version": s.version, "file_version": s.file_version, "modules": mods})
    };
    let tools = tools_dir(root);
    let mut plugin_json = Vec::new();
    let mut unmatched = 0;
    for s in scans.iter().filter(|s| is_plugin(s)) {
        let stem = s.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let meta = tools
            .as_ref()
            .and_then(|t| ci_path(t, &["Manifest", &stem, "manifest.json"]))
            .and_then(|p| serde_json::from_slice::<Value>(&fs::read(p).ok()?).ok())
            .map(|v| parse_plugin_manifest(&v))
            .unwrap_or_default();
        let registry: Vec<Value> = ua_xml
            .iter()
            .filter(|e| {
                (meta.render_id.is_some() && e.render_id == meta.render_id)
                    || e.vst_ids.iter().any(|v| meta.vst_ids.contains(v))
            })
            .map(UaXmlEntry::to_json)
            .collect();
        let bundles: Vec<Value> = meta
            .plugin_id
            .as_ref()
            .and_then(|id| vst3.get(id))
            .into_iter()
            .flatten()
            .map(|b| json!({"bundle": b.bundle, "plugin_id": b.plugin_id, "bundle_id": b.bundle_id, "cid": b.cid}))
            .collect();
        unmatched += usize::from(registry.is_empty());
        let mut v = dll_json(s);
        v["name"] = json!(meta.name);
        v["plugin_id"] = json!(meta.plugin_id);
        v["render_id"] = json!(meta.render_id);
        v["vst_ids"] = json!(meta.vst_ids);
        v["ua_xml"] = json!(registry);
        v["vst3"] = json!(bundles);
        plugin_json.push(v);
    }
    let system_json: Vec<Value> = scans.iter().filter(|s| !is_plugin(s)).map(dll_json).collect();
    let verified = module_json.iter().filter(|m| m["verified"] == true).count();
    let manifest = json!({
        "format": MANIFEST_FORMAT,
        "generator": format!("oa-assets {}", env!("CARGO_PKG_VERSION")),
        "created": firmware::utc_now(),
        "from": std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()),
        "uad_version": uad_version,
        "store": STORE,
        "totals": {"plugins": plugin_json.len(), "records": records, "modules": modules.len(), "bytes": bytes,
                   "verified": verified},
        "modules": module_json,
        "plugins": plugin_json,
        "system": system_json,
    });
    log(Event::Info(format!(
        "{} plug-ins carry {records} records: {} distinct modules, {:.1} MB ({verified} are pinned strip modules)",
        plugin_json.len(),
        modules.len(),
        bytes as f64 / 1e6
    )));
    if unmatched > 0 {
        log(Event::Info(format!("{unmatched} plug-ins have no UA.xml entry (Console strip headers and internals)")));
    }

    let mut report = Report { manifest, written: 0, kept: 0, removed: 0, exit: 0 };
    if opts.dry_run {
        log(Event::Ok(format!(
            "Dry run: nothing written; {} store files and {MANIFEST} would go to {}",
            modules.len(),
            out.display()
        )));
        return Ok(report);
    }

    log(Event::Header("Module store".into()));
    for (sha, rec) in &modules {
        let name = store_name(sha);
        if holds(&out.join(&name), rec.len, sha) {
            report.kept += 1;
        } else {
            firmware::write_atomic(out, &name, &store[*sha])?;
            report.written += 1;
        }
    }
    write_manifest(out, &report.manifest)?;
    let keep: BTreeSet<String> = modules.iter().map(|(sha, _)| format!("{sha}.bill")).collect();
    // No store folder: no module was ever written (plug-ins without records).
    for e in fs::read_dir(out.join(STORE)).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if is_store_file_name(&name) && !keep.contains(&name) {
            fs::remove_file(e.path())?;
            report.removed += 1;
        }
    }
    log(Event::Ok(format!(
        "{} modules in {}: {} written, {} already there, {} stale removed",
        modules.len(),
        out.join(STORE).display(),
        report.written,
        report.kept,
        report.removed
    )));
    Ok(report)
}

/// Write the manifest unless the one there differs only in `created`.
fn write_manifest(out: &Path, manifest: &Value) -> io::Result<()> {
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.remove("created");
        }
        v
    };
    let same = fs::read(out.join(MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .is_some_and(|old| strip(&old) == strip(manifest));
    if same {
        return Ok(());
    }
    let text = serde_json::to_string_pretty(manifest).map_err(io::Error::other)? + "\n";
    firmware::write_atomic(out, MANIFEST, text.as_bytes())
}

// ---- lookup, for hosting code ----

/// The manifest in `dir`, if any.
pub fn read_manifest(dir: &Path) -> Option<Value> {
    let v: Value = serde_json::from_slice(&fs::read(dir.join(MANIFEST)).ok()?).ok()?;
    (v["format"] == MANIFEST_FORMAT).then_some(v)
}

/// The manifest entry of a plug-in, found by DLL file name or stem, plug-in
/// name, `plugin_id`, VST id or render id (ASCII case ignored). Its `modules`
/// list names the store files the plug-in needs ([`module_path`]).
pub fn find_plugin<'a>(manifest: &'a Value, key: &str) -> Option<&'a Value> {
    let key = key.trim();
    let eq = |v: &Value| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(key));
    manifest["plugins"].as_array()?.iter().find(|p| {
        let dll = p["dll"].as_str().unwrap_or_default();
        dll.eq_ignore_ascii_case(key)
            || dll.rsplit_once('.').is_some_and(|(stem, _)| stem.eq_ignore_ascii_case(key))
            || eq(&p["name"])
            || eq(&p["plugin_id"])
            || p["vst_ids"].as_array().is_some_and(|ids| ids.iter().any(eq))
            || p["render_id"].as_u64().is_some_and(|r| r.to_string() == key)
    })
}

/// Where a module of a manifest entry's `modules` list is stored under `dir`.
pub fn module_path(dir: &Path, module: &Value) -> Option<PathBuf> {
    let sha = module["sha256"].as_str()?;
    is_store_file_name(&format!("{sha}.bill")).then(|| dir.join(store_name(sha)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmware::{BILL_HDR, BILL_W2, Known, ModulePin};
    use std::os::unix::fs::PermissionsExt;

    /// A scratch folder, removed on drop.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("oa-assets-dsp-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A synthetic on-disk Bill record of `payload` bytes: made-up bytes, never UA's.
    /// `w4` defaults to the largest the validator allows.
    fn bill(id: u32, w2: u32, payload: usize, seed: u8) -> Vec<u8> {
        let w4 = (payload / 4).saturating_sub(8) as u32;
        let mut r = b"Bill".to_vec();
        for w in [id, w2, payload as u32, w4] {
            r.extend_from_slice(&w.to_le_bytes());
        }
        r.extend((0..payload).map(|i| seed.wrapping_add((i * 13) as u8)));
        r
    }

    /// A fake DLL holding `records`, each 4-byte aligned, between filler.
    fn dll(records: &[&[u8]]) -> Vec<u8> {
        let mut d = b"MZ synthetic test file".to_vec();
        for r in records {
            d.extend_from_slice(&[0xcc; 7]);
            while !d.len().is_multiple_of(4) {
                d.push(0);
            }
            d.extend_from_slice(r);
        }
        d.extend_from_slice(&[0; 16]);
        d
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }

    const PP: &str = "Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins";
    const TOOLS: &str = "Program Files (x86)/Universal Audio/Powered Plugins";
    const VST3: &str = "Program Files/Common Files/VST3/Universal Audio";

    fn no_pins() -> Pins {
        Pins { strip_headers: Vec::new(), sdk: "UAD2SDK.dll".into(), modules: Vec::new(), images: Vec::new() }
    }

    fn pin(id: u32, rec: &[u8]) -> ModulePin {
        let known = Known { uad: None, length: rec.len(), w4: None, sha256: sha256_hex(rec) };
        ModulePin { id, required: true, known: vec![known] }
    }

    fn run(root: &Path, out: &Path, pins: &Pins, opts: Options) -> (Report, Vec<Event>) {
        let mut events = Vec::new();
        let r = collect(root, out, pins, opts, &mut |e| events.push(e)).unwrap();
        (r, events)
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn store_files(out: &Path) -> BTreeSet<String> {
        fs::read_dir(out.join(STORE)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
    }

    /// Two plug-ins sharing a code module, a kind-3 data record, UA.xml,
    /// both manifest versions, a VST3 bundle and a System32 SDK.
    struct Install {
        t: Tmp,
        root: PathBuf,
        shared: Vec<u8>,
        eq_a1: Vec<u8>,
        data3: Vec<u8>,
        comp: Vec<u8>,
    }

    fn install(tag: &str) -> Install {
        let t = Tmp::new(tag);
        let root = t.0.join("win");
        let shared = bill(0x0da, BILL_W2, 64, 1);
        let eq_a1 = bill(0x04b, 0x0100_0000, 64, 2);
        let data3 = bill(0x26f, 3, 96, 3);
        let comp = bill(0x2c3, BILL_W2, 40, 4);
        put(&root, &format!("{PP}/UAD Pultec EQP-1A.dll"), &dll(&[&eq_a1, &shared, &data3]));
        put(&root, &format!("{PP}/UAD UA 1176AE.dll"), &dll(&[&shared, &comp, &shared]));
        put(&root, &format!("{PP}/readme.txt"), b"not a dll");
        put(&root, "Windows/System32/UAD2SDK.dll", &dll(&[&shared]));
        put(
            &root,
            &format!("{TOOLS}/UA.xml"),
            br#"<uadparams><verinfo><swReleaseName>12.9.9.1</swReleaseName></verinfo>
  <plugins>
    <plugin id="122" title="UAD Pultec Passive EQ Collection" renderid="2140" vstids="NAAU,RAAU" />
    <plugin id="7" title="UAD 1176 &amp; Friends" renderid="2120" vstids="Z9AU" />
  </plugins>
  <categories><category title="EQ &amp; More"><plugin title="UAD Pultec EQP-1A" /></category></categories>
</uadparams>"#,
        );
        put(
            &root,
            &format!("{TOOLS}/Manifest/UAD Pultec EQP-1A/manifest.json"),
            br#"{"manifest_version": 2, "name": "UAD Pultec EQP-1A", "plugin_id": "uaudio_pultec_eqp-1a",
                "renderID": 2140, "vstID": "NAAU"}"#,
        );
        put(
            &root,
            &format!("{TOOLS}/Manifest/uad ua 1176ae/manifest.json"),
            br#"{"manifest_version": 1, "name": "UAD UA 1176AE", "plugin_id": "uaudio_ua_1176ae",
                "renderid": "2120", "vstids": "Z9AU"}"#,
        );
        let bundle = format!("{VST3}/Equalizers/UAD Pultec EQP-1A.vst3/Contents/Resources");
        put(
            &root,
            &format!("{bundle}/manifest.json"),
            br#"{"plugin_id": "uaudio_pultec_eqp-1a", "bundle_id": "NAAU"}"#,
        );
        put(
            &root,
            &format!("{bundle}/moduleinfo.json"),
            br#"{"Classes": [{"CID": "C0FFEE", "Category": "Component Controller Class"},
                             {"CID": "5653544E", "Category": "Audio Module Class"}]}"#,
        );
        Install { t, root, shared, eq_a1, data3, comp }
    }

    #[test]
    fn the_validator_bounds_select_records() {
        let mut data = Vec::new();
        let good: Vec<Vec<u8>> = vec![
            bill(0x0da, BILL_W2, 64, 1),
            bill(0x04b, 0x0100_0000, 64, 2),
            bill(0x25c, 1, 32, 3),
            bill(0x26f, 3, 96, 4),
            bill(0x100, 4, 32, 5),
        ];
        let mut bad = vec![
            bill(0x101, 5, 32, 6),           // kind 5
            bill(0x102, 0x0300_0000, 32, 7), // architecture 3
            bill(0x103, 0x0000_0100, 32, 8), // flags
        ];
        let mut w4 = bill(0x104, BILL_W2, 64, 9);
        w4[16..20].copy_from_slice(&1u32.to_le_bytes()); // fine: below the bound (64 / 4 - 8)
        let mut over = bill(0x105, BILL_W2, 32, 10);
        over[16..20].copy_from_slice(&1000u32.to_le_bytes()); // w4 beyond the record
        bad.push(over);
        let mut odd = bill(0x106, BILL_W2, 32, 11);
        odd[12..16].copy_from_slice(&30u32.to_le_bytes()); // not whole words
        bad.push(odd);
        for r in good.iter().chain(&bad).chain([&w4]) {
            data.extend_from_slice(r);
        }
        let ids: Vec<u32> = firmware::all_bill_records(&data).map(|r| r.id).collect();
        assert_eq!(ids, [0x0da, 0x04b, 0x25c, 0x26f, 0x100, 0x104]);
        let first = firmware::all_bill_records(&data).next().unwrap();
        assert_eq!((first.kind(), first.arch(), first.len), (0, 2, BILL_HDR + 64));
        // The firmware search is unchanged: word 2 0x02000000, no size rules (its pins check those).
        let strict: Vec<u32> = firmware::bill_records(&data).map(|r| r.id).collect();
        assert_eq!(strict, [0x0da, 0x105, 0x106, 0x104]);
    }

    #[test]
    fn collects_dedupes_and_maps_identities() {
        let i = install("collect");
        let out = i.t.0.join("out");
        let pins = Pins { modules: vec![pin(0x0da, &i.shared)], ..no_pins() };
        let (r, _) = run(&i.root, &out, &pins, Options::default());
        assert_eq!(r.exit, 0);
        let m: Value = serde_json::from_slice(&fs::read(out.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(m, r.manifest);
        assert_eq!(m["format"], MANIFEST_FORMAT);
        assert_eq!(m["uad_version"], "12.9.9.1");
        // 3 + 3 plug-in records (the 1176 carries `shared` twice); 4 distinct.
        assert_eq!(
            m["totals"],
            json!({"plugins": 2, "records": 6, "modules": 4,
            "bytes": i.shared.len() + i.eq_a1.len() + i.data3.len() + i.comp.len(), "verified": 1})
        );
        let want: BTreeSet<String> =
            [&i.shared, &i.eq_a1, &i.data3, &i.comp].iter().map(|b| format!("{}.bill", sha256_hex(b))).collect();
        assert_eq!(store_files(&out), want);
        for rec in [&i.shared, &i.eq_a1, &i.data3, &i.comp] {
            let p = out.join(store_name(&sha256_hex(rec)));
            assert_eq!(&fs::read(&p).unwrap(), rec, "stored unmodified");
            assert_eq!(mode(&p), 0o644);
        }
        assert_eq!((mode(&out), mode(&out.join(STORE)), mode(&out.join(MANIFEST))), (0o755, 0o755, 0o644));

        let shared = m["modules"].as_array().unwrap().iter().find(|x| x["sha256"] == sha256_hex(&i.shared)).unwrap();
        assert_eq!(
            *shared,
            json!({"sha256": sha256_hex(&i.shared), "file": store_name(&sha256_hex(&i.shared)), "id": "0da",
                   "kind": 0, "arch": 2, "w2": BILL_W2, "w4": 8, "length": i.shared.len(), "verified": true,
                   "used_by": 3, "pinned_as": "dsp/0da.bill"})
        );
        let data3 = m["modules"].as_array().unwrap().iter().find(|x| x["id"] == "26f").unwrap();
        assert_eq!((&data3["kind"], &data3["arch"], &data3["verified"]), (&json!(3), &json!(0), &json!(false)));

        let eq = find_plugin(&m, "UAD Pultec EQP-1A.dll").unwrap();
        assert_eq!(eq["path"], format!("{PP}/UAD Pultec EQP-1A.dll"));
        assert_eq!(eq["sha256"], sha256_hex(&fs::read(i.root.join(format!("{PP}/UAD Pultec EQP-1A.dll"))).unwrap()));
        assert_eq!(
            (&eq["plugin_id"], &eq["render_id"], &eq["vst_ids"]),
            (&json!("uaudio_pultec_eqp-1a"), &json!(2140), &json!(["NAAU"]))
        );
        assert_eq!(
            eq["ua_xml"],
            json!([{"id": 122, "title": "UAD Pultec Passive EQ Collection", "renderid": 2140,
                                         "vstids": ["NAAU", "RAAU"]}])
        );
        assert_eq!(
            eq["vst3"],
            json!([{"bundle": "Universal Audio/Equalizers/UAD Pultec EQP-1A.vst3",
                                       "plugin_id": "uaudio_pultec_eqp-1a", "bundle_id": "NAAU", "cid": "5653544E"}])
        );
        let ids: Vec<&str> = eq["modules"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["04b", "0da", "26f"]);
        let dll_bytes = fs::read(i.root.join(format!("{PP}/UAD Pultec EQP-1A.dll"))).unwrap();
        for x in eq["modules"].as_array().unwrap() {
            let (at, len) = (x["offset"].as_u64().unwrap() as usize, x["length"].as_u64().unwrap() as usize);
            let stored = fs::read(module_path(&out, x).unwrap()).unwrap();
            assert_eq!(&dll_bytes[at..at + len], stored.as_slice(), "offset and length point at the record");
        }

        // Manifest version 1 keys, a case-mangled manifest folder, an escaped UA.xml title.
        let comp = find_plugin(&m, "z9au").unwrap();
        assert_eq!(comp["dll"], "UAD UA 1176AE.dll");
        assert_eq!(comp["render_id"], 2120);
        assert_eq!(comp["ua_xml"][0]["title"], "UAD 1176 & Friends");
        assert_eq!(comp["vst3"], json!([]));
        assert_eq!(comp["modules"].as_array().unwrap().len(), 3);
        for key in ["uad ua 1176ae", "UAD UA 1176AE", "uaudio_ua_1176ae", "2120"] {
            assert_eq!(find_plugin(&m, key).unwrap()["dll"], "UAD UA 1176AE.dll", "{key}");
        }
        assert!(find_plugin(&m, "UAD Nothing").is_none());
        let odd = json!({"plugins": [{"dll": "é", "name": null}, {"dll": "Ünï.dll"}]});
        assert_eq!(find_plugin(&odd, "ünï"), None, "ASCII case only, and no panic on non-ASCII names");
        assert_eq!(find_plugin(&odd, "Ünï").unwrap()["dll"], "Ünï.dll");
        assert_eq!(m["system"][0]["dll"], "UAD2SDK.dll");
        assert_eq!(m["system"][0]["modules"][0]["sha256"], sha256_hex(&i.shared));
        assert_eq!(read_manifest(&out), Some(m));
    }

    #[test]
    fn a_rerun_rewrites_nothing_and_drops_stale_modules() {
        let i = install("rerun");
        let out = i.t.0.join("out");
        let (first, _) = run(&i.root, &out, &no_pins(), Options::default());
        assert_eq!((first.written, first.kept, first.removed), (4, 0, 0));
        let manifest = fs::read(out.join(MANIFEST)).unwrap();

        let (again, _) = run(&i.root, &out, &no_pins(), Options { jobs: 1, ..Options::default() });
        assert_eq!((again.written, again.kept, again.removed), (0, 4, 0));
        assert_eq!(fs::read(out.join(MANIFEST)).unwrap(), manifest, "only `created` would differ");

        // A damaged store file is rewritten; a foreign file is left alone.
        let comp = out.join(store_name(&sha256_hex(&i.comp)));
        fs::write(&comp, b"damaged").unwrap();
        put(&out, "modules/notes.txt", b"mine");
        let (r, _) = run(&i.root, &out, &no_pins(), Options::default());
        assert_eq!((r.written, r.kept), (1, 3));
        assert_eq!(fs::read(&comp).unwrap(), i.comp);

        // The 1176 is uninstalled: its own module goes, the shared one stays.
        fs::remove_file(i.root.join(format!("{PP}/UAD UA 1176AE.dll"))).unwrap();
        let (r, _) = run(&i.root, &out, &no_pins(), Options::default());
        assert_eq!((r.written, r.kept, r.removed), (0, 3, 1));
        assert!(!comp.exists());
        assert!(out.join(STORE).join("notes.txt").exists());
        assert_eq!(r.totals()["plugins"], 1);
        assert!(find_plugin(&read_manifest(&out).unwrap(), "UAD UA 1176AE").is_none());
    }

    #[test]
    fn dry_run_and_empty_installs_write_nothing() {
        let i = install("dry");
        let out = i.t.0.join("out");
        let (r, events) = run(&i.root, &out, &no_pins(), Options { dry_run: true, ..Options::default() });
        assert_eq!((r.exit, r.totals()["modules"].as_u64()), (0, Some(4)));
        assert!(!out.exists());
        assert!(matches!(events.last(), Some(Event::Ok(m)) if m.starts_with("Dry run")));

        let (r, events) = run(&i.t.0.join("nothing"), &out, &no_pins(), Options::default());
        assert_eq!(r.exit, 1);
        assert!(!out.exists(), "no empty folder is created");
        assert!(matches!(events.last(), Some(Event::Fail(_))));

        // An earlier run's store and manifest survive an empty source.
        run(&i.root, &out, &no_pins(), Options::default());
        let before = fs::read(out.join(MANIFEST)).unwrap();
        run(&i.t.0.join("nothing"), &out, &no_pins(), Options::default());
        assert_eq!(fs::read(out.join(MANIFEST)).unwrap(), before);
        assert_eq!(store_files(&out).len(), 4);
    }

    #[test]
    fn ua_xml_registry_entries_only() {
        let e = parse_ua_xml(
            "<x><plugins>\n<plugin id=\"3\" title=\"A &lt;B&gt; &quot;C&quot;\" renderid=\"12\" vstids=\" ab , cd \"/>\
             <plugin title='no id'/><plugin id='4' title='D'></plugin></plugins>\
             <categories><plugin id=\"9\" title=\"outside\"/></categories></x>",
        );
        assert_eq!(
            e,
            [
                UaXmlEntry {
                    id: 3,
                    title: "A <B> \"C\"".into(),
                    render_id: Some(12),
                    vst_ids: vec!["ab".into(), "cd".into()]
                },
                UaXmlEntry { id: 4, title: "D".into(), render_id: None, vst_ids: vec![] },
            ]
        );
        assert!(parse_ua_xml("<uadparams/>").is_empty());
    }

    #[test]
    fn store_names_are_hashes_only() {
        assert!(is_store_file_name(&format!("{}.bill", "a".repeat(64))));
        assert!(!is_store_file_name(&format!("{}.bill", "A".repeat(64))));
        assert!(!is_store_file_name(&format!("{}.bin", "a".repeat(64))));
        assert!(!is_store_file_name("0da.bill"));
        assert_eq!(module_path(Path::new("/x"), &json!({"sha256": "../../etc/passwd"})), None);
    }
}
