//! `oa-assets firmware`: the DSP modules and USB boot images the Apollo needs,
//! taken from the user's own Windows UAD install.
//!
//! open-apollo ships no Universal Audio bytes. At install time this reads a
//! Windows root (a mounted partition, a Wine prefix, an attached VM disk or an
//! MSI-extracted tree) and writes, under `--out` (default
//! [`DEFAULT_OUT`]):
//!
//! - `dsp/<id>.bill`: the Console strip DSP modules, each the on-disk "Bill"
//!   record exactly as UA's DLLs carry it (`Bill`, id, `0x02000000`, payload
//!   size, w4, payload). The driver and the USB init patch the arch byte and
//!   word 2 when they send it.
//! - `ApolloSolo.bin`, `ApolloTwin.bin`, `ApolloTwinX.bin`: the USB FX3 boot
//!   images, copied as-is from `Powered Plugins\Firmware\USB`. Only USB
//!   Apollos need them, so a missing one is a warning.
//! - `manifest.json`: where each file came from and its SHA-256.
//!
//! Only versions pinned in `data/firmware-pins.json` (hashes, never bytes) are
//! accepted unless `--accept-unknown` is given. The source is only read.
//!
//! Output, one line per file (open-apollo-setup parses it):
//! `NAME  ok|warn|missing|failed  [sha256 HEX]  [note]`, between the installers'
//! `── Header ──` and `[ OK ]`/`[INFO]`/`[WARN]`/`[FAIL]` lines.
//! Exit codes: 0 done (warnings allowed), 1 a required module is missing,
//! 2 a required module isn't a known version.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ci_path;
use crate::detect::{has_ext, walk};

/// Where the driver's `request_firmware` and the USB init look.
pub const DEFAULT_OUT: &str = "/lib/firmware/universal-audio";
pub const MANIFEST: &str = "manifest.json";
pub const MANIFEST_FORMAT: &str = "open-apollo-firmware-manifest/1";
pub const PINS_FORMAT: &str = "open-apollo-firmware-pins/1";
const PINS_JSON: &str = include_str!("../data/firmware-pins.json");

/// On-disk Bill record: `Bill`, id (arch byte 0), `0x02000000`, payload size, w4, payload.
pub const BILL_MAGIC: &[u8; 4] = b"Bill";
pub const BILL_W2: u32 = 0x0200_0000;
pub const BILL_HDR: usize = 0x14;

const EXIT_MISSING: u8 = 1;
const EXIT_MISMATCH: u8 = 2;

// ---- pins ----

/// One accepted version of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub uad: Option<String>,
    pub length: usize,
    /// The Bill record's w4 word (DSP modules only).
    pub w4: Option<u32>,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModulePin {
    pub id: u32,
    /// The Mic Input strip's modules; the others are reserved for later strips.
    pub required: bool,
    pub known: Vec<Known>,
}

impl ModulePin {
    pub fn file_name(&self) -> String {
        format!("dsp/{:03x}.bill", self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImagePin {
    pub name: String,
    pub known: Vec<Known>,
}

/// The known-good table: names, lengths, w4 words and hashes. No UA bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pins {
    /// Console strip plug-ins, searched first, in this order.
    pub strip_headers: Vec<String>,
    /// File name of UAD's SDK under `Windows\System32`, searched second.
    pub sdk: String,
    pub modules: Vec<ModulePin>,
    pub images: Vec<ImagePin>,
}

impl Pins {
    /// The table compiled into this program (`data/firmware-pins.json`).
    pub fn builtin() -> Pins {
        Pins::parse(PINS_JSON).expect("data/firmware-pins.json is valid")
    }

    pub fn parse(text: &str) -> Result<Pins, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| format!("pins: {e}"))?;
        if v["format"] != PINS_FORMAT {
            return Err(format!("pins: format is not {PINS_FORMAT}"));
        }
        let strings = |v: &Value, what: &str| -> Result<Vec<String>, String> {
            v.as_array()
                .ok_or(format!("pins: {what} is not a list"))?
                .iter()
                .map(|s| s.as_str().map(str::to_string).ok_or(format!("pins: {what} holds a non-string")))
                .collect()
        };
        let known = |v: &Value, what: &str| -> Result<Vec<Known>, String> {
            let list = v["known"].as_array().filter(|a| !a.is_empty()).ok_or(format!("pins: {what} has no known"))?;
            list.iter()
                .map(|k| {
                    let sha256 = k["sha256"].as_str().unwrap_or_default().to_string();
                    if sha256.len() != 64 || !sha256.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
                        return Err(format!("pins: {what}: sha256 must be 64 lowercase hex digits"));
                    }
                    Ok(Known {
                        uad: k["uad"].as_str().map(str::to_string),
                        length: k["length"].as_u64().ok_or(format!("pins: {what}: no length"))? as usize,
                        w4: k["w4"].as_u64().map(|w| w as u32),
                        sha256,
                    })
                })
                .collect()
        };
        let modules = v["dsp_modules"]
            .as_array()
            .ok_or("pins: dsp_modules is not a list")?
            .iter()
            .map(|m| {
                let id = m["id"].as_str().and_then(|s| u32::from_str_radix(s, 16).ok()).ok_or("pins: bad module id")?;
                let what = format!("module {id:03x}");
                Ok(ModulePin { id, required: m["required"].as_bool().unwrap_or(true), known: known(m, &what)? })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let images = v["usb_fx3"]
            .as_array()
            .ok_or("pins: usb_fx3 is not a list")?
            .iter()
            .map(|m| {
                let name = m["name"].as_str().ok_or("pins: image without a name")?.to_string();
                Ok(ImagePin { known: known(m, &name)?, name })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Pins {
            strip_headers: strings(&v["strip_headers"], "strip_headers")?,
            sdk: v["sdk"].as_str().ok_or("pins: no sdk")?.to_string(),
            modules,
            images,
        })
    }
}

/// The pinned version `bytes` is, if any.
fn known_match<'a>(known: &'a [Known], bytes: &[u8], sha: &str) -> Option<&'a Known> {
    known.iter().find(|k| k.length == bytes.len() && k.sha256 == sha && k.w4.is_none_or(|w| bill_w4(bytes) == Some(w)))
}

// ---- Bill records ----

/// A Bill record inside a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BillRecord {
    pub offset: usize,
    pub id: u32,
    /// Whole record: header + payload.
    pub len: usize,
    pub w4: u32,
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn bill_w4(rec: &[u8]) -> Option<u32> {
    (rec.len() >= BILL_HDR).then(|| le32(rec, 16))
}

/// Every on-disk Bill record in `data`: 4-byte aligned, `Bill`, an id whose arch
/// byte (offset 7) is 0, word 2 `0x02000000`, and a payload that fits.
pub fn bill_records(data: &[u8]) -> impl Iterator<Item = BillRecord> + '_ {
    (0..data.len().saturating_sub(BILL_HDR - 1)).step_by(4).filter_map(move |i| {
        if &data[i..i + 4] != BILL_MAGIC {
            return None;
        }
        let id = le32(data, i + 4);
        if id >> 24 != 0 || le32(data, i + 8) != BILL_W2 {
            return None;
        }
        let len = BILL_HDR.checked_add(le32(data, i + 12) as usize)?;
        (i.checked_add(len)? <= data.len()).then(|| BillRecord { offset: i, id, len, w4: le32(data, i + 16) })
    })
}

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

// ---- where the inputs are ----

/// Every DLL under `Universal Audio\Powered Plugins`, in either Program Files.
fn powered_dlls(root: &Path) -> Vec<PathBuf> {
    ["Program Files", "Program Files (x86)"]
        .iter()
        .filter_map(|pf| ci_path(root, &[pf, "Universal Audio", "Powered Plugins"]))
        .flat_map(|d| walk(&d, 3, &|p| has_ext(p, "dll") && p.is_file()))
        .collect()
}

fn name_is(p: &Path, name: &str) -> bool {
    p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
}

/// The module search order: the Console strip plug-ins, then `UAD2SDK.dll`,
/// then every other Powered plug-in (only for ids the first two don't have).
fn module_sources(root: &Path, pins: &Pins) -> [Vec<PathBuf>; 3] {
    let dlls = powered_dlls(root);
    let headers: Vec<PathBuf> =
        pins.strip_headers.iter().filter_map(|h| dlls.iter().find(|p| name_is(p, h)).cloned()).collect();
    let sdk: Vec<PathBuf> =
        ci_path(root, &["Windows", "System32", &pins.sdk]).filter(|p| p.is_file()).into_iter().collect();
    let others = dlls.into_iter().filter(|p| !headers.contains(p)).collect();
    [headers, sdk, others]
}

/// Whether `root` holds anything the module search reads.
pub fn has_inputs(root: &Path, pins: &Pins) -> bool {
    let [headers, sdk, _] = module_sources(root, pins);
    !headers.is_empty() || !sdk.is_empty()
}

fn usb_image(root: &Path, name: &str) -> Option<PathBuf> {
    ["Program Files (x86)", "Program Files"]
        .iter()
        .find_map(|pf| ci_path(root, &[pf, "Universal Audio", "Powered Plugins", "Firmware", "USB", name]))
        .filter(|p| p.is_file())
}

// ---- report lines ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// A pinned version, written.
    Ok,
    /// Written although not pinned (`--accept-unknown`), or an optional file that
    /// isn't a known version and so wasn't written.
    Warn,
    Missing,
    /// A required module that isn't a known version; not written.
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Missing => "missing",
            Status::Failed => "failed",
        }
    }
}

/// One output file's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLine {
    /// Relative to `--out`, e.g. `dsp/0da.bill`.
    pub name: String,
    pub status: Status,
    pub sha256: Option<String>,
    pub note: String,
    pub required: bool,
}

impl FileLine {
    /// `NAME  STATUS  [sha256 HEX]  [note]`.
    pub fn render(&self) -> String {
        let sha = self.sha256.as_ref().map(|s| format!("sha256 {s}  ")).unwrap_or_default();
        format!("{:<16} {:<7}  {sha}{}", self.name, self.status.as_str(), self.note).trim_end().to_string()
    }

    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "status": self.status.as_str(), "sha256": self.sha256,
               "note": self.note, "required": self.required})
    }
}

/// Progress, in the installers' line format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Header(String),
    Info(String),
    File(FileLine),
    Ok(String),
    Warn(String),
    Fail(String),
}

impl Event {
    pub fn render(&self) -> String {
        match self {
            Event::Header(h) => format!("── {h} ──"),
            Event::Info(m) => format!("[INFO]  {m}"),
            Event::File(f) => f.render(),
            Event::Ok(m) => format!("[ OK ]  {m}"),
            Event::Warn(m) => format!("[WARN]  {m}"),
            Event::Fail(m) => format!("[FAIL]  {m}"),
        }
    }
}

// ---- extraction ----

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Write files whose hash isn't pinned (marked `verified: false`).
    pub accept_unknown: bool,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub files: Vec<FileLine>,
    /// What was written to `<out>/manifest.json`.
    pub manifest: Value,
    /// 0 done, 1 a required module missing, 2 a required module not a known version.
    pub exit: u8,
}

impl Report {
    pub fn written(&self) -> usize {
        self.manifest["files"].as_array().map_or(0, Vec::len)
    }
}

struct Candidate {
    input: PathBuf,
    offset: usize,
    bytes: Vec<u8>,
    sha256: String,
    verified: bool,
}

struct Input {
    size: usize,
    sha256: String,
}

/// Find each pinned module, in [`module_sources`] order. A pinned version wins
/// over an unknown one; the other Powered plug-ins are searched only for ids
/// that neither the strip plug-ins nor the SDK carry at all.
fn search_modules(
    root: &Path,
    pins: &Pins,
    log: &mut dyn FnMut(Event),
) -> (BTreeMap<u32, Candidate>, BTreeMap<PathBuf, Input>) {
    let [headers, sdk, others] = module_sources(root, pins);
    let mut s = Search { pins, found: BTreeMap::new(), inputs: BTreeMap::new(), seen: BTreeSet::new() };
    let all: BTreeSet<u32> = pins.modules.iter().map(|m| m.id).collect();
    for file in headers.iter().chain(&sdk) {
        if s.done(&all) {
            break;
        }
        s.scan(file, &all, log);
    }
    let absent: BTreeSet<u32> = all.difference(&s.seen).copied().collect();
    if !absent.is_empty() && !others.is_empty() {
        let ids: Vec<String> = absent.iter().map(|id| format!("{id:03x}")).collect();
        log(Event::Info(format!(
            "Looking for module {} in the other Powered plug-ins ({} DLLs)",
            ids.join(", "),
            others.len()
        )));
        for file in &others {
            if s.done(&absent) {
                break;
            }
            s.scan(file, &absent, log);
        }
    }
    (s.found, s.inputs)
}

struct Search<'a> {
    pins: &'a Pins,
    found: BTreeMap<u32, Candidate>,
    inputs: BTreeMap<PathBuf, Input>,
    /// Ids with a record anywhere searched so far, pinned or not.
    seen: BTreeSet<u32>,
}

impl Search<'_> {
    fn done(&self, want: &BTreeSet<u32>) -> bool {
        want.iter().all(|id| self.found.get(id).is_some_and(|c| c.verified))
    }

    /// Take the records of `want`'s ids from one file.
    fn scan(&mut self, file: &Path, want: &BTreeSet<u32>, log: &mut dyn FnMut(Event)) {
        let data = match fs::read(file) {
            Ok(d) => d,
            Err(e) => {
                log(Event::Warn(format!("can't read {}: {e}", file.display())));
                return;
            }
        };
        let mut used = false;
        for rec in bill_records(&data) {
            let Some(pin) = self.pins.modules.iter().find(|m| m.id == rec.id) else { continue };
            if !want.contains(&rec.id) || self.found.get(&rec.id).is_some_and(|c| c.verified) {
                continue;
            }
            self.seen.insert(rec.id);
            let bytes = &data[rec.offset..rec.offset + rec.len];
            let sha256 = sha256_hex(bytes);
            let verified = known_match(&pin.known, bytes, &sha256).is_some();
            if self.found.get(&rec.id).is_none_or(|c| verified && !c.verified) {
                let c = Candidate {
                    input: file.to_path_buf(),
                    offset: rec.offset,
                    bytes: bytes.to_vec(),
                    sha256,
                    verified,
                };
                self.found.insert(rec.id, c);
                used = true;
            }
        }
        if used {
            self.inputs
                .entry(file.to_path_buf())
                .or_insert_with(|| Input { size: data.len(), sha256: sha256_hex(&data) });
        }
    }
}

fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `p` relative to the root, with `/` separators.
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn describe(k: &[Known]) -> String {
    k.iter()
        .map(|k| {
            let w4 = k.w4.map(|w| format!(", w4 {w}")).unwrap_or_default();
            let uad = k.uad.as_deref().map(|u| format!(" (UAD {u})")).unwrap_or_default();
            format!("{} bytes{w4}{uad}", k.length)
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

/// Extract everything `pins` lists from `root` into `out`.
pub fn extract(root: &Path, out: &Path, pins: &Pins, opts: Options, log: &mut dyn FnMut(Event)) -> io::Result<Report> {
    log(Event::Header("Reading your UAD install".into()));
    log(Event::Info(format!("From {} (read-only)", root.display())));
    let uad_version = crate::detect::uad_version(root);
    if let Some(v) = &uad_version {
        log(Event::Info(format!("UAD software {v}")));
    }
    log(Event::Info(format!("To {}", out.display())));

    let mut files: Vec<FileLine> = Vec::new();
    let mut written: Vec<Value> = Vec::new();
    let mut used_inputs: Vec<String> = Vec::new();
    let push = |files: &mut Vec<FileLine>, f: FileLine, log: &mut dyn FnMut(Event)| {
        log(Event::File(f.clone()));
        files.push(f);
    };

    log(Event::Header("DSP modules".into()));
    let (found, inputs) = search_modules(root, pins, log);
    for pin in &pins.modules {
        let name = pin.file_name();
        let line = |status, sha256: Option<&str>, note: String| FileLine {
            name: name.clone(),
            status,
            sha256: sha256.map(str::to_string),
            note,
            required: pin.required,
        };
        let Some(c) = found.get(&pin.id) else {
            let why = if pin.required { "not in this install" } else { "not in this install (optional)" };
            push(&mut files, line(Status::Missing, None, why.into()), log);
            continue;
        };
        let at = format!("({} +{:#x})", file_label(&c.input), c.offset);
        if !c.verified && !opts.accept_unknown {
            let note = format!(
                "{at} not a known version: {} bytes, w4 {}; pinned {}; not written (--accept-unknown writes it)",
                c.bytes.len(),
                bill_w4(&c.bytes).unwrap_or(0),
                describe(&pin.known)
            );
            let status = if pin.required { Status::Failed } else { Status::Warn };
            push(&mut files, line(status, Some(&c.sha256), note), log);
            continue;
        }
        write_atomic(out, &name, &c.bytes)?;
        let input = rel(root, &c.input);
        written.push(json!({
            "name": name, "size": c.bytes.len(), "sha256": c.sha256, "kind": "dsp-module", "verified": c.verified,
            "source": {"input": input, "offset": c.offset, "length": c.bytes.len(), "transform": "none"},
        }));
        used_inputs.push(input);
        let (status, note) = if c.verified {
            (Status::Ok, at)
        } else {
            (Status::Warn, format!("{at} not a known version; written anyway"))
        };
        push(&mut files, line(status, Some(&c.sha256), note), log);
    }

    log(Event::Header("USB boot images".into()));
    let mut image_inputs: BTreeMap<PathBuf, Input> = BTreeMap::new();
    for pin in &pins.images {
        let line = |status, sha256: Option<&str>, note: String| FileLine {
            name: pin.name.clone(),
            status,
            sha256: sha256.map(str::to_string),
            note,
            required: false,
        };
        let Some(path) = usb_image(root, &pin.name) else {
            push(&mut files, line(Status::Missing, None, "not in this install (only USB Apollos need it)".into()), log);
            continue;
        };
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                push(&mut files, line(Status::Missing, None, format!("can't read {}: {e}", path.display())), log);
                continue;
            }
        };
        let sha = sha256_hex(&bytes);
        let verified = known_match(&pin.known, &bytes, &sha).is_some();
        if !verified && !opts.accept_unknown {
            let note = format!(
                "not a known version: {} bytes; pinned {}; not written (--accept-unknown writes it)",
                bytes.len(),
                describe(&pin.known)
            );
            push(&mut files, line(Status::Warn, Some(&sha), note), log);
            continue;
        }
        write_atomic(out, &pin.name, &bytes)?;
        let input = rel(root, &path);
        written.push(json!({
            "name": pin.name, "size": bytes.len(), "sha256": sha, "kind": "usb-fx3", "verified": verified,
            "source": {"input": input, "offset": 0, "length": bytes.len(), "transform": "none"},
        }));
        used_inputs.push(input);
        let (status, note) = if verified {
            (Status::Ok, "(Firmware/USB)".to_string())
        } else {
            (Status::Warn, "(Firmware/USB) not a known version; written anyway".to_string())
        };
        push(&mut files, line(status, Some(&sha), note), log);
        image_inputs.insert(path, Input { size: bytes.len(), sha256: sha });
    }
    let missing_images: Vec<&str> = files
        .iter()
        .filter(|f| !f.name.starts_with("dsp/") && f.status == Status::Missing)
        .map(|f| f.name.as_str())
        .collect();
    if !missing_images.is_empty() {
        log(Event::Warn(format!(
            "{} not in this install: only needed for an Apollo Solo, Twin or Twin X on USB",
            missing_images.join(", ")
        )));
    }

    let inputs: Vec<Value> = inputs
        .iter()
        .chain(image_inputs.iter())
        .map(|(p, i)| (rel(root, p), i))
        .filter(|(r, _)| used_inputs.contains(r))
        .map(|(r, i)| json!({"path": r, "size": i.size, "sha256": i.sha256}))
        .collect();
    let manifest = json!({
        "format": MANIFEST_FORMAT,
        "generator": format!("oa-assets {}", env!("CARGO_PKG_VERSION")),
        "created": utc_now(),
        "from": std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()),
        "uad_version": uad_version,
        "inputs": inputs,
        "files": written,
    });

    log(Event::Header("Manifest".into()));
    if written_any(&manifest) {
        let text = serde_json::to_string_pretty(&manifest).map_err(io::Error::other)? + "\n";
        write_atomic(out, MANIFEST, text.as_bytes())?;
    } else {
        // Keep an earlier run's manifest: it still describes what's there.
        log(Event::Info(format!("Nothing extracted, so {} was left as it was", out.display())));
    }

    let exit = if files.iter().any(|f| f.status == Status::Failed) {
        EXIT_MISMATCH
    } else if files.iter().any(|f| f.required && f.status == Status::Missing) {
        EXIT_MISSING
    } else {
        0
    };
    let report = Report { files, manifest, exit };
    let n = report.written();
    match exit {
        0 if n == 0 => log(Event::Warn("Nothing to extract from this install".into())),
        0 => log(Event::Ok(format!("Wrote {n} files and {MANIFEST} to {}", out.display()))),
        EXIT_MISMATCH => log(Event::Fail(format!(
            "This UAD version has DSP modules this oa-assets doesn't know; wrote {n} files to {}",
            out.display()
        ))),
        _ => log(Event::Fail(format!(
            "Required DSP modules are missing from this install; wrote {n} files to {}",
            out.display()
        ))),
    }
    Ok(report)
}

fn written_any(manifest: &Value) -> bool {
    manifest["files"].as_array().is_some_and(|f| !f.is_empty())
}

/// Write `bytes` to `dir/rel` through a temp file and a rename: 0644 files, 0755 directories.
fn write_atomic(dir: &Path, rel: &str, bytes: &[u8]) -> io::Result<()> {
    let dest = dir.join(rel);
    let parent = dest.parent().unwrap_or(dir).to_path_buf();
    fs::DirBuilder::new().recursive(true).mode(0o755).create(&parent)?;
    // The umask may have trimmed the mode; the layout is ours, so set it.
    let mut d = parent.as_path();
    loop {
        fs::set_permissions(d, fs::Permissions::from_mode(0o755))?;
        if d == dir {
            break;
        }
        match d.parent() {
            Some(p) if p.starts_with(dir) => d = p,
            _ => break,
        }
    }
    let file_name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let res = (|| {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp, &dest)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res.map_err(|e| io::Error::new(e.kind(), format!("can't write {}: {e}", dest.display())))
}

// ---- golden check ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoldenResult {
    Identical,
    NotInInstall,
    Different,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenItem {
    pub name: String,
    pub required: bool,
    pub result: GoldenResult,
    pub sha256: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct Golden {
    pub items: Vec<GoldenItem>,
    pub exit: u8,
}

impl Golden {
    pub fn to_json(&self) -> Value {
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|i| {
                let result = match i.result {
                    GoldenResult::Identical => "identical",
                    GoldenResult::NotInInstall => "not-in-install",
                    GoldenResult::Different => "different",
                };
                json!({"name": i.name, "result": result, "sha256": i.sha256, "note": i.note, "required": i.required})
            })
            .collect();
        json!({"exit": self.exit, "items": items})
    }
}

/// Re-extract into a temp dir and check every pinned file: its hash must be
/// pinned and, when `out` is given, `out` must hold the same bytes.
pub fn verify_golden(root: &Path, pins: &Pins, out: Option<&Path>, log: &mut dyn FnMut(Event)) -> io::Result<Golden> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("oa-assets-golden-{}-{nanos}", std::process::id()));
    // A fresh folder of our own, never one that was already there.
    fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    let result = golden_in(root, pins, out, &tmp, log);
    let _ = fs::remove_dir_all(&tmp);
    result
}

fn golden_in(
    root: &Path,
    pins: &Pins,
    out: Option<&Path>,
    tmp: &Path,
    log: &mut dyn FnMut(Event),
) -> io::Result<Golden> {
    extract(root, tmp, pins, Options { accept_unknown: true }, &mut |_| {})?;
    log(Event::Header("Golden check".into()));
    let wanted = pins
        .modules
        .iter()
        .map(|m| (m.file_name(), m.required, &m.known))
        .chain(pins.images.iter().map(|i| (i.name.clone(), false, &i.known)));
    let mut items = Vec::new();
    for (name, required, known) in wanted {
        let Ok(bytes) = fs::read(tmp.join(&name)) else {
            let item = GoldenItem {
                name,
                required,
                result: GoldenResult::NotInInstall,
                sha256: None,
                note: "not in this install".into(),
            };
            let line = format!("{}  {}", item.name, item.note);
            log(if required { Event::Fail(line) } else { Event::Warn(line) });
            items.push(item);
            continue;
        };
        let sha = sha256_hex(&bytes);
        let pinned = known_match(known, &bytes, &sha).is_some();
        let (result, note) = if !pinned {
            (GoldenResult::Different, format!("DIFFERENT: {} bytes; pinned {}", bytes.len(), describe(known)))
        } else if let Some(o) = out {
            if fs::read(o.join(&name)).is_ok_and(|b| b == bytes) {
                (GoldenResult::Identical, format!("identical to the pin and to {}", o.display()))
            } else {
                (GoldenResult::Different, format!("matches the pin, but {} holds other bytes", o.join(&name).display()))
            }
        } else {
            (GoldenResult::Identical, "identical to the pin".to_string())
        };
        let line = format!("{name}  {note}  sha256 {sha}");
        log(if result == GoldenResult::Identical { Event::Ok(line) } else { Event::Fail(line) });
        items.push(GoldenItem { name, required, result, sha256: Some(sha), note });
    }
    let total = items.len();
    let same = items.iter().filter(|i| i.result == GoldenResult::Identical).count();
    let absent: Vec<&str> =
        items.iter().filter(|i| i.result == GoldenResult::NotInInstall).map(|i| i.name.as_str()).collect();
    let differ = items.iter().filter(|i| i.result == GoldenResult::Different).count();
    let exit = if differ > 0 {
        EXIT_MISMATCH
    } else if items.iter().any(|i| i.required && i.result == GoldenResult::NotInInstall) {
        EXIT_MISSING
    } else {
        0
    };
    let mut summary = format!("golden: {same} of {total} pinned files reproduced byte for byte");
    if differ > 0 {
        summary += &format!("; {differ} different");
    }
    if !absent.is_empty() {
        summary += &format!("; not in this install: {}", absent.join(", "));
    }
    log(if exit == 0 { Event::Ok(summary) } else { Event::Fail(summary) });
    Ok(Golden { items, exit })
}

// ---- misc ----

/// Now, as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_now() -> String {
    utc_string(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
}

fn utc_string(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Whether `out` lies inside `root` (never write into the source).
pub fn inside(out: &Path, root: &Path) -> bool {
    let Ok(root) = fs::canonicalize(root) else { return false };
    let abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    // Canonicalize the deepest existing ancestor; keep the rest as given.
    let mut base = abs.as_path();
    let mut tail = Vec::new();
    while !base.exists() {
        let (Some(parent), Some(name)) = (base.parent(), base.file_name()) else { break };
        tail.push(name.to_os_string());
        base = parent;
    }
    let mut p = fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    p.extend(tail.iter().rev());
    p.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scratch folder, removed on drop.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("oa-assets-fw-{tag}-{}-{n}", std::process::id()));
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

    /// A synthetic on-disk Bill record: made-up payload bytes, never UA's.
    fn bill(id: u32, payload: usize, w4: u32, seed: u8) -> Vec<u8> {
        let mut r = b"Bill".to_vec();
        for w in [id, BILL_W2, payload as u32, w4] {
            r.extend_from_slice(&w.to_le_bytes());
        }
        r.extend((0..payload).map(|i| seed.wrapping_add((i * 7) as u8)));
        r
    }

    /// A fake DLL holding `records`, each 4-byte aligned, between filler.
    fn dll(records: &[&[u8]]) -> Vec<u8> {
        let mut d = b"MZ synthetic test file".to_vec();
        for r in records {
            d.extend_from_slice(&[0xcc; 9]);
            while !d.len().is_multiple_of(4) {
                d.push(0);
            }
            d.extend_from_slice(r);
        }
        d.extend_from_slice(&[0; 16]);
        d
    }

    fn known(rec: &[u8]) -> Known {
        Known { uad: Some("test".into()), length: rec.len(), w4: bill_w4(rec), sha256: sha256_hex(rec) }
    }

    const MIC: &str = "UAD Mic Input Strip Header.dll";
    const INPUT: &str = "UAD Input Strip Header.dll";
    const TALKBACK: &str = "UAD Talkback Input Strip Header.dll";

    fn pins(modules: &[(u32, &[u8], bool)], images: &[(&str, &[u8])]) -> Pins {
        Pins {
            strip_headers: vec![MIC.into(), INPUT.into(), TALKBACK.into()],
            sdk: "UAD2SDK.dll".into(),
            modules: modules
                .iter()
                .map(|(id, rec, req)| ModulePin { id: *id, required: *req, known: vec![known(rec)] })
                .collect(),
            images: images.iter().map(|(n, b)| ImagePin { name: n.to_string(), known: vec![known(b)] }).collect(),
        }
    }

    fn put(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }

    const PP: &str = "Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins";
    const USB: &str = "Program Files (x86)/Universal Audio/Powered Plugins/Firmware/USB";

    fn run(root: &Path, out: &Path, pins: &Pins, opts: Options) -> (Report, Vec<Event>) {
        let mut events = Vec::new();
        let r = extract(root, out, pins, opts, &mut |e| events.push(e)).unwrap();
        (r, events)
    }

    fn line<'a>(r: &'a Report, name: &str) -> &'a FileLine {
        r.files.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no line for {name}"))
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn bill_parser_takes_aligned_disk_records_only() {
        let good = bill(0x0da, 12, 7, 1);
        let mut data = vec![0u8; 4];
        data.extend_from_slice(&good); // offset 4: found
        data.extend_from_slice(&[0; 2]); // the rest is misaligned...
        data.extend_from_slice(&bill(0x0eb, 4, 1, 2)); // ...so not found
        data.extend_from_slice(&[0; 2]);
        let mut pcie = bill(0x0c2, 4, 1, 3);
        pcie[7] = 2; // arch byte set: a sent copy, not the on-disk form
        data.extend_from_slice(&pcie);
        let mut w2 = bill(0x0a5, 4, 1, 4);
        w2[8..12].copy_from_slice(&0u32.to_le_bytes()); // USB form
        data.extend_from_slice(&w2);
        let mut cut = bill(0x12b, 400, 1, 5);
        cut.truncate(40); // size runs past the end of the file
        data.extend_from_slice(&cut);
        let got: Vec<BillRecord> = bill_records(&data).collect();
        assert_eq!(got, [BillRecord { offset: 4, id: 0x0da, len: BILL_HDR + 12, w4: 7 }]);
        assert_eq!(bill_records(b"Bil").count(), 0);
    }

    #[test]
    fn builtin_pins_match_the_driver_table() {
        let p = Pins::builtin();
        assert_eq!(p.strip_headers, [MIC, INPUT, TALKBACK]);
        assert_eq!(p.sdk, "UAD2SDK.dll");
        // ua_strips.c: id, length on disk, code-pool dwords (w4 * 2 / 3), reserved.
        let driver = [
            (0x0eb, 716, 0x06c, false),
            (0x0da, 3044, 0x1f0, false),
            (0x0a5, 1940, 0x130, false),
            (0x0c2, 184, 0x016, false),
            (0x12b, 452, 0x040, false),
            (0x0db, 728, 0x06e, true),
            (0x0c9, 352, 0x032, true),
        ];
        assert_eq!(p.modules.len(), driver.len());
        for (id, len, sram, reserved) in driver {
            let m = p.modules.iter().find(|m| m.id == id).unwrap();
            assert_eq!(m.required, !reserved, "{id:03x}");
            let k = &m.known[0];
            assert_eq!((k.length, k.w4.map(|w| w * 2 / 3)), (len, Some(sram)), "{id:03x}");
        }
        let names: Vec<&str> = p.images.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["ApolloSolo.bin", "ApolloTwin.bin", "ApolloTwinX.bin"]);
    }

    #[test]
    fn extracts_from_a_case_mangled_install() {
        let (a5, da, db, c9) =
            (bill(0x0a5, 40, 30, 1), bill(0x0da, 64, 45, 2), bill(0x0db, 8, 3, 3), bill(0x0c9, 12, 6, 4));
        let fx3 = b"synthetic fx3 image".to_vec();
        let p = pins(
            &[(0x0a5, &a5, true), (0x0da, &da, true), (0x0db, &db, false), (0x0c9, &c9, false)],
            &[("ApolloSolo.bin", &fx3)],
        );
        let t = Tmp::new("mangled");
        let root = t.0.join("win");
        let pp = "program files/UNIVERSAL AUDIO/powered plugins/uad-2 powered plugins";
        put(&root, &format!("{pp}/uad mic input strip header.dll"), &dll(&[&a5, &da]));
        put(&root, &format!("{pp}/UAD INPUT STRIP HEADER.DLL"), &dll(&[&db]));
        put(&root, &format!("{pp}/uad talkback input strip header.dll"), &dll(&[&c9, &a5]));
        put(&root, "PROGRAM FILES (X86)/universal audio/POWERED PLUGINS/firmware/usb/apollosolo.bin", &fx3);
        let out = t.0.join("out");
        // A previous run's file is replaced.
        put(&out, "dsp/0a5.bill", b"old");

        let (r, _) = run(&root, &out, &p, Options::default());
        assert_eq!(r.exit, 0);
        for (name, want) in [("dsp/0a5.bill", &a5), ("dsp/0da.bill", &da), ("dsp/0db.bill", &db), ("dsp/0c9.bill", &c9)]
        {
            assert_eq!(line(&r, name).status, Status::Ok, "{name}");
            assert_eq!(&fs::read(out.join(name)).unwrap(), want, "{name}");
            assert_eq!(mode(&out.join(name)), 0o644);
        }
        assert_eq!(fs::read(out.join("ApolloSolo.bin")).unwrap(), fx3);
        assert_eq!((mode(&out), mode(&out.join("dsp")), mode(&out.join(MANIFEST))), (0o755, 0o755, 0o644));
        assert_eq!(line(&r, "dsp/0a5.bill").note, "(uad mic input strip header.dll +0x20)");
        let leftovers: Vec<_> = [out.clone(), out.join("dsp")]
            .iter()
            .flat_map(|d| fs::read_dir(d).unwrap().flatten())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn search_order_is_strip_headers_then_sdk_then_any_plugin() {
        let (a5, c2, eb, x12b) =
            (bill(0x0a5, 20, 9, 1), bill(0x0c2, 24, 12, 2), bill(0x0eb, 28, 15, 3), bill(0x12b, 8, 6, 4));
        let p = pins(&[(0x0a5, &a5, true), (0x0c2, &c2, true), (0x0eb, &eb, true), (0x12b, &x12b, true)], &[]);
        let t = Tmp::new("order");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&a5]));
        put(&root, "Windows/System32/UAD2SDK.dll", &dll(&[&eb, &a5, &c2]));
        // Earlier in the walk than the SDK would be, but only a last resort.
        put(&root, &format!("{PP}/UAD 1176 Classic.dll"), &dll(&[&c2, &x12b]));
        let (r, events) = run(&root, &t.0.join("out"), &p, Options::default());
        assert_eq!(r.exit, 0);
        assert!(line(&r, "dsp/0a5.bill").note.starts_with(&format!("({MIC} ")));
        assert!(line(&r, "dsp/0c2.bill").note.starts_with("(UAD2SDK.dll "));
        assert!(line(&r, "dsp/0eb.bill").note.starts_with("(UAD2SDK.dll "));
        assert!(line(&r, "dsp/12b.bill").note.starts_with("(UAD 1176 Classic.dll "));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Info(m) if m.contains("module 12b in the other Powered plug-ins")))
        );
        let inputs: Vec<&str> =
            r.manifest["inputs"].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap()).collect();
        assert_eq!(
            inputs,
            [&format!("{PP}/UAD 1176 Classic.dll"), &format!("{PP}/{MIC}"), "Windows/System32/UAD2SDK.dll"]
        );

        // Everything in the strip plug-ins: the SDK isn't even read.
        put(&root, &format!("{PP}/{INPUT}"), &dll(&[&c2, &eb, &x12b]));
        let (r, _) = run(&root, &t.0.join("out2"), &p, Options::default());
        let inputs = r.manifest["inputs"].as_array().unwrap();
        assert!(inputs.iter().all(|i| i["path"].as_str().unwrap().contains("Strip Header")), "{inputs:?}");
    }

    #[test]
    fn a_pinned_version_beats_an_unknown_one() {
        let da = bill(0x0da, 32, 21, 1);
        let other = bill(0x0da, 32, 21, 99);
        let p = pins(&[(0x0da, &da, true)], &[]);
        let t = Tmp::new("prefer");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&other]));
        put(&root, "windows/system32/uad2sdk.dll", &dll(&[&da]));
        let (r, _) = run(&root, &t.0.join("out"), &p, Options::default());
        assert_eq!((r.exit, line(&r, "dsp/0da.bill").status), (0, Status::Ok));
        assert_eq!(fs::read(t.0.join("out/dsp/0da.bill")).unwrap(), da);
    }

    #[test]
    fn unknown_version_fails_with_exit_2_unless_accepted() {
        let pinned = bill(0x0da, 32, 21, 1);
        let newer = bill(0x0da, 36, 24, 2);
        let p = pins(&[(0x0da, &pinned, true)], &[]);
        let t = Tmp::new("mismatch");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&newer]));

        let out = t.0.join("out");
        let (r, events) = run(&root, &out, &p, Options::default());
        assert_eq!(r.exit, 2);
        let l = line(&r, "dsp/0da.bill");
        assert_eq!(l.status, Status::Failed);
        assert_eq!(l.sha256.as_deref(), Some(sha256_hex(&newer).as_str()));
        assert!(l.note.contains("56 bytes, w4 24; pinned 52 bytes, w4 21 (UAD test)"), "{}", l.note);
        assert!(!out.join("dsp/0da.bill").exists());
        assert!(matches!(events.last(), Some(Event::Fail(_))));

        let (r, _) = run(&root, &out, &p, Options { accept_unknown: true });
        assert_eq!((r.exit, line(&r, "dsp/0da.bill").status), (0, Status::Warn));
        assert_eq!(fs::read(out.join("dsp/0da.bill")).unwrap(), newer);
        assert_eq!(r.manifest["files"][0]["verified"], false);
    }

    #[test]
    fn unknown_optional_files_are_warnings() {
        let (da, c9) = (bill(0x0da, 8, 3, 1), bill(0x0c9, 8, 3, 2));
        let fx3 = b"pinned fx3".to_vec();
        let p = pins(&[(0x0da, &da, true), (0x0c9, &c9, false)], &[("ApolloTwin.bin", &fx3)]);
        let t = Tmp::new("optional");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&da, &bill(0x0c9, 12, 3, 9)]));
        put(&root, &format!("{USB}/ApolloTwin.bin"), b"another fx3");
        let out = t.0.join("out");
        let (r, _) = run(&root, &out, &p, Options::default());
        assert_eq!(r.exit, 0);
        assert_eq!(line(&r, "dsp/0c9.bill").status, Status::Warn);
        assert_eq!(line(&r, "ApolloTwin.bin").status, Status::Warn);
        assert!(!out.join("dsp/0c9.bill").exists() && !out.join("ApolloTwin.bin").exists());
    }

    #[test]
    fn missing_required_module_exits_1_and_optional_does_not() {
        let (da, eb, db) = (bill(0x0da, 8, 3, 1), bill(0x0eb, 8, 3, 2), bill(0x0db, 8, 3, 3));
        let p = pins(&[(0x0da, &da, true), (0x0eb, &eb, true), (0x0db, &db, false)], &[]);
        let t = Tmp::new("missing");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&da]));
        let (r, _) = run(&root, &t.0.join("out"), &p, Options::default());
        assert_eq!(r.exit, 1);
        assert_eq!(line(&r, "dsp/0eb.bill").render(), "dsp/0eb.bill     missing  not in this install");
        assert_eq!(line(&r, "dsp/0db.bill").status, Status::Missing);
        assert_eq!(line(&r, "dsp/0da.bill").status, Status::Ok);
        assert!(t.0.join("out/dsp/0da.bill").is_file(), "what was found is still written");

        put(&root, "Windows/System32/UAD2SDK.dll", &dll(&[&eb]));
        let (r, _) = run(&root, &t.0.join("out"), &p, Options::default());
        assert_eq!(r.exit, 0, "only the optional 0db is missing");
    }

    #[test]
    fn an_empty_install_leaves_the_previous_manifest() {
        let da = bill(0x0da, 8, 3, 1);
        let p = pins(&[(0x0da, &da, true)], &[]);
        let t = Tmp::new("empty");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&da]));
        let out = t.0.join("out");
        run(&root, &out, &p, Options::default());
        let before = fs::read(out.join(MANIFEST)).unwrap();
        let (r, _) = run(&t.0.join("nothing-here"), &out, &p, Options::default());
        assert_eq!((r.exit, r.written()), (1, 0));
        assert_eq!(fs::read(out.join(MANIFEST)).unwrap(), before);
        assert!(out.join("dsp/0da.bill").is_file());
        let (r, _) = run(&t.0.join("nothing-here"), &t.0.join("fresh"), &p, Options::default());
        assert_eq!(r.exit, 1);
        assert!(!t.0.join("fresh").exists(), "no empty folder is created");
    }

    #[test]
    fn missing_usb_images_are_only_warnings() {
        let da = bill(0x0da, 8, 3, 1);
        let p = pins(
            &[(0x0da, &da, true)],
            &[("ApolloSolo.bin", b"a"), ("ApolloTwin.bin", b"b"), ("ApolloTwinX.bin", b"c")],
        );
        let t = Tmp::new("nofx3");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&da]));
        put(&root, &format!("{USB}/ApolloTwin.bin"), b"b");
        let (r, events) = run(&root, &t.0.join("out"), &p, Options::default());
        assert_eq!(r.exit, 0);
        assert_eq!(line(&r, "ApolloTwin.bin").status, Status::Ok);
        for name in ["ApolloSolo.bin", "ApolloTwinX.bin"] {
            assert_eq!(
                line(&r, name).render(),
                format!("{name:<16} missing  not in this install (only USB Apollos need it)")
            );
        }
        assert!(events.iter().any(|e| matches!(e, Event::Warn(m) if m.starts_with("ApolloSolo.bin, ApolloTwinX.bin"))));
        assert!(matches!(events.last(), Some(Event::Ok(_))));
    }

    #[test]
    fn manifest_records_inputs_and_files() {
        let da = bill(0x0da, 16, 9, 1);
        let fx3 = b"fx3 boot image (synthetic)".to_vec();
        let p = pins(&[(0x0da, &da, true)], &[("ApolloSolo.bin", &fx3)]);
        let t = Tmp::new("manifest");
        let root = t.0.join("win");
        let mic = dll(&[&da]);
        put(&root, &format!("{PP}/{MIC}"), &mic);
        put(&root, &format!("{USB}/ApolloSolo.bin"), &fx3);
        put(
            &root,
            "Program Files (x86)/Universal Audio/Powered Plugins/UA.xml",
            b"<swReleaseName>12.9.9.1</swReleaseName>",
        );
        let out = t.0.join("out");
        let (r, _) = run(&root, &out, &p, Options::default());
        let m: Value = serde_json::from_slice(&fs::read(out.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(m, r.manifest);
        assert_eq!(m["format"], MANIFEST_FORMAT);
        assert_eq!(m["generator"], format!("oa-assets {}", env!("CARGO_PKG_VERSION")));
        assert_eq!(m["from"], root.to_string_lossy().as_ref());
        assert_eq!(m["uad_version"], "12.9.9.1");
        let created = m["created"].as_str().unwrap();
        assert!(created.len() == 20 && created.ends_with('Z') && created.as_bytes()[10] == b'T', "{created}");
        let off = mic.windows(4).position(|w| w == BILL_MAGIC).unwrap();
        assert_eq!(
            m["inputs"],
            json!([
                {"path": format!("{PP}/{MIC}"), "size": mic.len(), "sha256": sha256_hex(&mic)},
                {"path": format!("{USB}/ApolloSolo.bin"), "size": fx3.len(), "sha256": sha256_hex(&fx3)},
            ])
        );
        assert_eq!(
            m["files"],
            json!([
                {"name": "dsp/0da.bill", "size": da.len(), "sha256": sha256_hex(&da), "kind": "dsp-module", "verified": true,
                 "source": {"input": format!("{PP}/{MIC}"), "offset": off, "length": da.len(), "transform": "none"}},
                {"name": "ApolloSolo.bin", "size": fx3.len(), "sha256": sha256_hex(&fx3), "kind": "usb-fx3", "verified": true,
                 "source": {"input": format!("{USB}/ApolloSolo.bin"), "offset": 0, "length": fx3.len(), "transform": "none"}},
            ])
        );
    }

    #[test]
    fn golden_check_reproduces_pins_and_the_out_copy() {
        let (da, c9) = (bill(0x0da, 16, 9, 1), bill(0x0c9, 8, 3, 2));
        let p = pins(&[(0x0da, &da, true), (0x0c9, &c9, false)], &[("ApolloSolo.bin", b"fx3")]);
        let t = Tmp::new("golden");
        let root = t.0.join("win");
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&da, &c9]));
        let out = t.0.join("out");
        run(&root, &out, &p, Options::default());

        let mut events = Vec::new();
        let g = verify_golden(&root, &p, Some(&out), &mut |e| events.push(e)).unwrap();
        let results: Vec<GoldenResult> = g.items.iter().map(|i| i.result).collect();
        assert_eq!(results, [GoldenResult::Identical, GoldenResult::Identical, GoldenResult::NotInInstall]);
        assert_eq!(g.exit, 0);
        assert!(matches!(events.last(), Some(Event::Ok(m)) if m.starts_with("golden: 2 of 3")));

        fs::write(out.join("dsp/0c9.bill"), b"edited").unwrap();
        let g = verify_golden(&root, &p, Some(&out), &mut |_| {}).unwrap();
        assert_eq!((g.items[1].result, g.exit), (GoldenResult::Different, 2));

        // Without --out only the pins are checked; an unknown version differs.
        put(&root, &format!("{PP}/{MIC}"), &dll(&[&bill(0x0da, 16, 9, 7), &c9]));
        let g = verify_golden(&root, &p, None, &mut |_| {}).unwrap();
        assert_eq!((g.items[0].result, g.exit), (GoldenResult::Different, 2));
        fs::remove_file(root.join(format!("{PP}/{MIC}"))).unwrap();
        let g = verify_golden(&root, &p, None, &mut |_| {}).unwrap();
        assert_eq!(g.exit, 1, "the required module is gone");
    }

    #[test]
    fn file_lines_have_the_wizards_shape() {
        let ok = FileLine {
            name: "dsp/0da.bill".into(),
            status: Status::Ok,
            sha256: Some("ab".repeat(32)),
            note: "(UAD Mic Input Strip Header.dll +0x38d1b0)".into(),
            required: true,
        };
        let text = ok.render();
        let tok: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(&tok[..4], ["dsp/0da.bill", "ok", "sha256", &"ab".repeat(32)]);
        assert_eq!(tok[4..].join(" "), ok.note);
        assert_eq!(Event::Header("DSP modules".into()).render(), "── DSP modules ──");
        assert_eq!(Event::Warn("x".into()).render(), "[WARN]  x");
    }

    #[test]
    fn utc_timestamps() {
        assert_eq!(utc_string(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_string(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_string(1_760_351_706), "2025-10-13T10:35:06Z");
    }

    #[test]
    fn out_inside_the_source_is_detected() {
        let t = Tmp::new("inside");
        let root = t.0.join("win");
        fs::create_dir_all(root.join("Windows")).unwrap();
        assert!(inside(&root.join("Windows/new/dir"), &root));
        assert!(inside(&root.join("x/../Windows"), &root));
        assert!(!inside(&t.0.join("out"), &root));
        assert!(!inside(&t.0.join("winx"), &root));
    }

    #[test]
    fn finds_inputs_only_where_the_module_sources_are() {
        let p = Pins::builtin();
        let t = Tmp::new("inputs");
        assert!(!has_inputs(&t.0, &p));
        put(&t.0, "windows/SYSTEM32/uad2sdk.DLL", b"MZ");
        assert!(has_inputs(&t.0, &p));
    }
}
