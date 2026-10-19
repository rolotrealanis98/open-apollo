//! `oa-assets catalogue`: UA's plug-in catalogue and factory preset menus, rebuilt from the
//! user's own UAD install into `<data>/ua-catalogue/` (the files `mixer-engine/ua_catalogue.py`
//! overlays on the shipped device map at daemon start):
//!
//! - `plugins.json`: name, short names, categories, VST id, render id, plug-in id and the
//!   factory preset tree of every plug-in (`UA.xml`, `Manifest/*/manifest.json`, `Presets/`)
//! - `channel-presets.json`: the factory channel-strip preset tree (`Presets/Channel Strip`, or
//!   for an install that never saw an Apollo, the `Channel Strip.zip` inside the cached
//!   `apollo-32bit.msi`; `source.via` says which)
//! - `io-presets.json`: the factory I/O-map preset names (`Presets/IOPresets`)
//!
//! Names only: no preset file, no UA code and no licence state leaves the install. Without
//! this step the daemon's `/plugins` list is empty and the console's browser shows nothing.
//! Port of the private Python prototype (`ua_import_proto.py`); the Mixer Helper help text it
//! also produced is left out (the helper runs without it).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::ci_path;

pub const DIR: &str = "ua-catalogue";
const FORMAT: &str = "open-apollo-ua-catalogue";
const VERSION: u32 = 1;

/// UAD's tools folder (`UA.xml`, `Manifest/`, `Presets/`): Windows' Powered Plugins folder or
/// macOS' Application Support folder.
pub fn tools_dir(root: &Path) -> Option<PathBuf> {
    ["Program Files (x86)", "Program Files"]
        .iter()
        .find_map(|pf| ci_path(root, &[pf, "Universal Audio", "Powered Plugins", "UA.xml"]))
        .or_else(|| ci_path(root, &["Library", "Application Support", "Universal Audio", "UA.xml"]))
        .and_then(|x| x.parent().map(Path::to_path_buf))
}

pub struct Written {
    pub dir: PathBuf,
    pub plugins: usize,
    pub channel_presets: usize,
    pub io_presets: usize,
    pub uad_version: Option<String>,
}

/// Writes the catalogue of the install under `root` into `<data>/ua-catalogue`.
pub fn write(root: &Path, data: &Path) -> Result<Written, String> {
    let tools = tools_dir(root).ok_or("no UAD install (UA.xml) under that folder")?;
    let xml = std::fs::read_to_string(tools.join("UA.xml")).map_err(|e| format!("UA.xml: {e}"))?;
    let ua = UaXml::parse(&xml);
    let presets = ci_path(&tools, &["Presets"]);
    let mut plugins = Vec::new();
    let manifests = ci_path(&tools, &["Manifest"]).map(|d| list(&d)).unwrap_or_default();
    for dir in manifests {
        let Some(m) = read_json(&dir.join("manifest.json")) else { continue };
        let Some(name) = m["name"].as_str().filter(|n| !n.is_empty()) else { continue };
        // Console's input-strip headers and the internal module are not user plug-ins.
        if name.ends_with("Input Strip Header") || name == "UAD Internal" {
            continue;
        }
        let x = ua.plugins.get(name);
        let vst_id = m["vstID"]
            .as_str()
            .or_else(|| m["vstids"].as_str())
            .or_else(|| x.and_then(|p| p.vstids.as_deref()))
            .map(|s| s.split(',').next().unwrap_or("").to_string())
            .unwrap_or_default();
        let render_id =
            m["renderID"].as_u64().or_else(|| m["renderid"].as_u64()).or_else(|| x.and_then(|p| p.renderid));
        let tree = presets.as_ref().and_then(|p| ci_path(p, &[name])).map(|d| dir_tree(&d, ".fxp")).unwrap_or_default();
        let mut categories = ua.categories.get(name).cloned().unwrap_or_default();
        if categories.is_empty() {
            // macOS manifests carry the categories; Windows ones rely on UA.xml.
            categories = m["categories"]
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
        }
        categories.sort();
        categories.dedup();
        let mut short_names = ua.short_names.get(name).cloned().unwrap_or_default();
        short_names.sort();
        short_names.dedup();
        plugins.push(json!({
            "name": name,
            "short_names": short_names,
            "categories": categories,
            "vst_id": vst_id,
            "render_id": render_id,
            "plugin_id": m["plugin_id"].as_str(),
            // Which plug-ins load in a Unison slot is in no install file; the daemon's own list decides.
            "unison": Value::Null,
            "presets": tree,
        }));
    }
    plugins.sort_by(|a, b| natural_cmp(a["name"].as_str().unwrap_or(""), b["name"].as_str().unwrap_or("")));
    let (channel, via) = match presets.as_ref().and_then(|p| ci_path(p, &["Channel Strip"])).filter(|d| d.is_dir()) {
        Some(d) => (dir_tree(&d, ".uadchannel"), Some("installed".to_string())),
        None => match msi_channel_presets(root) {
            Some((folder, tree)) => (tree, Some(format!("msi:{folder}"))),
            None => (json!([]), None),
        },
    };
    let mut io: Vec<String> = presets
        .as_ref()
        .and_then(|p| ci_path(p, &["IOPresets"]))
        .map(|d| {
            list(&d)
                .into_iter()
                .filter_map(|f| f.file_name()?.to_str()?.strip_suffix(".uadio").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    io.sort_by(|a, b| natural_cmp(a, b));

    let dir = data.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let source = json!({ "uad": ua.release, "from": root });
    let doc = |part: &str, key: &str, source: &Value, body: Value| -> Result<(), String> {
        let v = json!({ "format": format!("{FORMAT}/{part}"), "version": VERSION, "source": source, key: body });
        let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{part}.json"));
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    let written = Written {
        dir: dir.clone(),
        plugins: plugins.len(),
        channel_presets: count(&channel),
        io_presets: io.len(),
        uad_version: ua.release.clone(),
    };
    doc("plugins", "plugins", &source, Value::Array(plugins))?;
    doc("channel-presets", "presets", &json!({ "uad": ua.release, "from": root, "via": via }), channel)?;
    doc("io-presets", "presets", &source, json!(io))?;
    crate::import::record(
        data,
        "ua_catalogue",
        json!({ "from": root, "version": ua.release, "plugins": written.plugins, "channel_presets": written.channel_presets, "io_presets": written.io_presets, "imported_at": crate::import::unix_now() }),
    )
    .map_err(|e| format!("manifest: {e}"))?;
    Ok(written)
}

fn count(tree: &Value) -> usize {
    tree.as_array()
        .map(|a| a.iter().map(|e| if e["type"] == "folder" { count(&e["children"]) } else { 1 }).sum())
        .unwrap_or(0)
}

fn list(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The engine's preset tree of a folder: folders and `ext` files interleaved in the engine's
/// natural order, every factory entry locked.
fn dir_tree(dir: &Path, ext: &str) -> Value {
    let mut entries: Vec<(bool, String, PathBuf)> = list(dir)
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_owned();
            if p.is_dir() {
                Some((true, name, p))
            } else {
                name.to_ascii_lowercase().ends_with(ext).then_some((false, name, p))
            }
        })
        .collect();
    entries.sort_by(|a, b| natural_cmp(&a.1, &b.1));
    Value::Array(
        entries
            .into_iter()
            .map(|(folder, name, p)| {
                if folder {
                    json!({ "value": name, "type": "folder", "locked": true, "children": dir_tree(&p, ext) })
                } else {
                    json!({ "value": &name[..name.len() - ext.len()], "type": "file", "locked": true })
                }
            })
            .collect(),
    )
}

/// Apollo installer's package id in `ProgramData/Package Cache/<id>v<version>/`.
const APOLLO_MSI_GUID: &str = "{2ADBD732-9983-4171-97C7-1FCFE0D28F81}";
/// Never read a zip bigger than this into memory while looking for the channel strips.
const MAX_ZIP: u32 = 64 << 20;

/// The factory channel-strip tree out of the cached Apollo installer, for installs without
/// `Presets/Channel Strip`: `(cache folder name, tree)` of the newest cached version that has it.
fn msi_channel_presets(root: &Path) -> Option<(String, Value)> {
    let cache = ci_path(root, &["ProgramData", "Package Cache"])?;
    let mut dirs: Vec<PathBuf> = list(&cache)
        .into_iter()
        .filter(|d| {
            let n = d.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
            n.starts_with(&format!("{}v", APOLLO_MSI_GUID.to_ascii_lowercase()))
        })
        .collect();
    dirs.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    dirs.iter().rev().find_map(|d| {
        let names = channel_strip_names(&ci_path(d, &["apollo-32bit.msi"])?)?;
        Some((d.file_name()?.to_str()?.to_owned(), names_tree(&names, ".uadchannel")))
    })
}

/// The `Channel Strip/...` member names (below that folder) of the `Channel Strip.zip` in an MSI.
/// The MSI is a CFB file whose streams are the embedded cabinets; the zip is a cabinet member
/// (or, rarely, a stream). The File table that maps cabinet keys to real names is not read: any
/// zip holding `Channel Strip/` entries is the one.
// ponytail: cabinets embedded in the MSI only (not `Data1.cab` beside it); each candidate member is
// re-decoded from its folder start, fine for an installer with a few dozen files.
fn channel_strip_names(msi: &Path) -> Option<Vec<Vec<String>>> {
    let mut cfb = cfb::open(msi).ok()?;
    let streams: Vec<PathBuf> = cfb.walk().filter(|e| e.is_stream()).map(|e| e.path().to_path_buf()).collect();
    for path in streams {
        let mut s = cfb.open_stream(&path).ok()?;
        let mut magic = [0u8; 4];
        let head = s.read_exact(&mut magic).is_ok();
        s.seek(SeekFrom::Start(0)).ok()?;
        if !head {
            continue;
        }
        if &magic == b"PK\x03\x04" {
            let mut zip = Vec::new();
            if s.take(MAX_ZIP as u64).read_to_end(&mut zip).is_ok()
                && let Some(n) = zip_channel_strips(&zip)
            {
                return Some(n);
            }
        } else if &magic == b"MSCF" {
            let Ok(mut cab) = cab::Cabinet::new(s) else { continue };
            let members: Vec<String> = cab
                .folder_entries()
                .flat_map(|f| f.file_entries())
                .filter(|f| (22..=MAX_ZIP).contains(&f.uncompressed_size()))
                .map(|f| f.name().to_owned())
                .collect();
            for name in members {
                let mut zip = Vec::new();
                if cab.read_file(&name).and_then(|mut r| r.read_to_end(&mut zip)).is_ok()
                    && zip.starts_with(b"PK\x03\x04")
                    && let Some(n) = zip_channel_strips(&zip)
                {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// File names under `Channel Strip/` from a zip's central directory (names only; nothing is
/// inflated). `None` if it is not a zip or has no such folder. No zip64.
fn zip_channel_strips(zip: &[u8]) -> Option<Vec<Vec<String>>> {
    let le16 = |o: usize| zip.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let le32 = |o: usize| zip.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let eocd =
        (0..=zip.len().checked_sub(22)?).rev().take(22 + 0xFFFF).find(|&o| zip[o..].starts_with(b"PK\x05\x06"))?;
    let (count, mut at) = (le16(eocd + 10)?, le32(eocd + 16)?);
    let mut out = Vec::new();
    for _ in 0..count {
        if zip.get(at..at + 4)? != b"PK\x01\x02" {
            return None;
        }
        let (n, extra, comment) = (le16(at + 28)?, le16(at + 30)?, le16(at + 32)?);
        let name = String::from_utf8_lossy(zip.get(at + 46..at + 46 + n)?).replace('\\', "/");
        at += 46 + n + extra + comment;
        let mut parts = name.split('/');
        if parts.next().is_some_and(|p| p.eq_ignore_ascii_case("Channel Strip")) {
            let rest: Vec<String> = parts.map(str::to_owned).collect();
            if rest.last().is_some_and(|l| !l.is_empty()) {
                out.push(rest);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `dir_tree` for a list of relative paths: only `ext` files, folders from their paths.
fn names_tree(paths: &[Vec<String>], ext: &str) -> Value {
    let mut folders: BTreeMap<&str, Vec<Vec<String>>> = BTreeMap::new();
    let mut entries: Vec<(bool, &str)> = Vec::new();
    for p in paths {
        match p.as_slice() {
            [f] if f.to_ascii_lowercase().ends_with(ext) => entries.push((false, f)),
            [d, rest @ ..] if !rest.is_empty() => folders.entry(d).or_default().push(rest.to_vec()),
            _ => {}
        }
    }
    entries.extend(folders.keys().map(|d| (true, *d)));
    entries.sort_by(|a, b| natural_cmp(a.1, b.1));
    entries.dedup();
    Value::Array(
        entries
            .into_iter()
            .map(|(folder, name)| {
                if folder {
                    json!({ "value": name, "type": "folder", "locked": true, "children": names_tree(&folders[name], ext) })
                } else {
                    json!({ "value": &name[..name.len() - ext.len()], "type": "file", "locked": true })
                }
            })
            .collect(),
    )
}

/// The engine's menu order: case-insensitive, whitespace runs as one space, digit runs by
/// value, the rest character by character.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn norm(s: &str) -> String {
        s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
    }
    let (a, b) = (norm(a), norm(b));
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = 0u128;
                while let Some(c) = x.peek().filter(|c| c.is_ascii_digit()) {
                    n = n * 10 + c.to_digit(10).unwrap() as u128;
                    x.next();
                }
                let mut m = 0u128;
                while let Some(d) = y.peek().filter(|d| d.is_ascii_digit()) {
                    m = m * 10 + d.to_digit(10).unwrap() as u128;
                    y.next();
                }
                if n != m {
                    return n.cmp(&m);
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
}

#[derive(Default)]
struct UaPlugin {
    renderid: Option<u64>,
    vstids: Option<String>,
}

/// The parts of `UA.xml` the catalogue uses: `<verinfo><swReleaseName>`, `<plugins><plugin
/// title renderid vstids>`, `<categories><category title><plugin title>` and
/// `<shortnames><names title><name title>`.
#[derive(Default)]
struct UaXml {
    release: Option<String>,
    plugins: BTreeMap<String, UaPlugin>,
    categories: BTreeMap<String, Vec<String>>,
    short_names: BTreeMap<String, Vec<String>>,
}

impl UaXml {
    // ponytail: a tag scanner over UA.xml's fixed shape, not an XML parser; swap for one if UA
    // changes the file.
    fn parse(text: &str) -> Self {
        let mut out = Self::default();
        if let Some(s) = text.find("<swReleaseName>") {
            let s = s + "<swReleaseName>".len();
            if let Some(e) = text[s..].find('<') {
                out.release = Some(text[s..s + e].trim().to_string());
            }
        }
        let mut section = "";
        let mut parent = String::new();
        let mut rest = text;
        while let Some(start) = rest.find('<') {
            let Some(end) = rest[start..].find('>') else { break };
            let tag = &rest[start + 1..start + end];
            rest = &rest[start + end + 1..];
            let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
            match name {
                "plugins" | "categories" | "shortnames" => section = if tag.starts_with('/') { "" } else { name },
                "plugin" if section == "plugins" => {
                    if let Some(title) = attr(tag, "title") {
                        out.plugins.insert(
                            title,
                            UaPlugin {
                                renderid: attr(tag, "renderid").and_then(|v| v.parse().ok()),
                                vstids: attr(tag, "vstids"),
                            },
                        );
                    }
                }
                "category" if section == "categories" => parent = attr(tag, "title").unwrap_or_default(),
                "plugin" if section == "categories" => {
                    if let Some(title) = attr(tag, "title") {
                        out.categories.entry(title).or_default().push(parent.clone());
                    }
                }
                "names" if section == "shortnames" => parent = attr(tag, "title").unwrap_or_default(),
                "name" if section == "shortnames" => {
                    if let Some(short) = attr(tag, "title").filter(|s| !s.is_empty())
                        && !parent.is_empty()
                    {
                        out.short_names.entry(parent.clone()).or_default().push(short);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// The value of attribute `key` in `tag` (XML entities unescaped).
fn attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let s = tag.find(&pat)? + pat.len();
    let e = tag[s..].find('"')?;
    Some(
        tag[s..s + e]
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn natural_order_and_entities() {
        assert_eq!(natural_cmp("UAD 610", "UAD 1176"), Ordering::Less);
        assert_eq!(natural_cmp("Snare Patz B", "Snare Patz"), Ordering::Greater);
        assert_eq!(natural_cmp("a  b", "A B"), Ordering::Equal);
        assert_eq!(attr(r#"plugin title="Guitar &amp; Bass" x="1""#, "title").as_deref(), Some("Guitar & Bass"));
    }

    #[test]
    fn writes_the_catalogue_from_a_windows_layout() {
        let t = std::env::temp_dir().join(format!("oa-assets-catalogue-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let pp = t.join("c/Program Files (x86)/Universal Audio/Powered Plugins");
        fs::create_dir_all(pp.join("Manifest/UAD Test Comp")).unwrap();
        fs::create_dir_all(pp.join("Manifest/UAD Console Input Strip Header")).unwrap();
        fs::create_dir_all(pp.join("Presets/UAD Test Comp/Vocals")).unwrap();
        fs::create_dir_all(pp.join("Presets/Channel Strip/Bass")).unwrap();
        fs::create_dir_all(pp.join("Presets/IOPresets")).unwrap();
        fs::write(
            pp.join("UA.xml"),
            r#"<?xml version="1.0"?><UA><verinfo><swReleaseName>12.0.0.602</swReleaseName></verinfo>
<plugins><plugin id="0" title="UAD Test Comp" renderid="2096" vstids="58AU"/></plugins>
<categories><category title="Compressors &amp; Limiters"><plugin title="UAD Test Comp"/></category></categories>
<shortnames><names title="UAD Test Comp"><name title="Test Comp"/><name title="TC"/></names></shortnames></UA>"#,
        )
        .unwrap();
        fs::write(
            pp.join("Manifest/UAD Test Comp/manifest.json"),
            r#"{"name":"UAD Test Comp","plugin_id":"uaudio_test_comp"}"#,
        )
        .unwrap();
        fs::write(
            pp.join("Manifest/UAD Console Input Strip Header/manifest.json"),
            r#"{"name":"UAD Console Input Strip Header"}"#,
        )
        .unwrap();
        fs::write(pp.join("Presets/UAD Test Comp/Default.fxp"), "").unwrap();
        fs::write(pp.join("Presets/UAD Test Comp/Vocals/Lead 10.fxp"), "").unwrap();
        fs::write(pp.join("Presets/UAD Test Comp/Vocals/Lead 9.fxp"), "").unwrap();
        fs::write(pp.join("Presets/Channel Strip/Bass/Fat Bass.uadchannel"), "").unwrap();
        fs::write(pp.join("Presets/IOPresets/Default.uadio"), "").unwrap();
        let data = t.join("data");
        let w = write(&t.join("c"), &data).unwrap();
        assert_eq!((w.plugins, w.channel_presets, w.io_presets), (1, 1, 1));
        let p: Value = serde_json::from_slice(&fs::read(data.join("ua-catalogue/plugins.json")).unwrap()).unwrap();
        assert_eq!(p["format"], "open-apollo-ua-catalogue/plugins");
        assert_eq!(p["source"]["uad"], "12.0.0.602");
        let e = &p["plugins"][0];
        assert_eq!(e["name"], "UAD Test Comp");
        assert_eq!(e["short_names"], json!(["TC", "Test Comp"]));
        assert_eq!(e["categories"], json!(["Compressors & Limiters"]));
        assert_eq!(
            (e["vst_id"].as_str(), e["render_id"].as_u64(), e["plugin_id"].as_str()),
            (Some("58AU"), Some(2096), Some("uaudio_test_comp"))
        );
        assert_eq!(e["presets"][0]["value"], "Default");
        assert_eq!(e["presets"][1]["type"], "folder");
        assert_eq!(e["presets"][1]["children"][0]["value"], "Lead 9");
        let c: Value =
            serde_json::from_slice(&fs::read(data.join("ua-catalogue/channel-presets.json")).unwrap()).unwrap();
        assert_eq!(c["presets"][0]["children"][0]["value"], "Fat Bass");
        let m = crate::import::read_manifest(&data);
        assert_eq!(m["ua_catalogue"]["plugins"], 1);
        fs::remove_dir_all(&t).ok();
    }

    /// An Apollo-less install: no `Presets/Channel Strip`, the strips only in the cached MSI
    /// (CFB -> cabinet stream -> `Channel Strip.zip`, beside a decoy member).
    fn fake_msi(path: &Path, names: &[&str], cab_compression: cab::CompressionType) {
        use std::io::Write;
        let mut zip = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut zip);
        for n in names {
            w.start_file(
                *n,
                zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();
        let mut b = cab::CabinetBuilder::new();
        let f = b.add_folder(cab_compression);
        f.add_file("driver.sys");
        f.add_file("ChannelStrip.zip_1");
        let mut cw = b.build(std::io::Cursor::new(Vec::new())).unwrap();
        while let Some(mut file) = cw.next_file().unwrap() {
            if file.file_name() == "driver.sys" {
                file.write_all(&[7u8; 4096]).unwrap()
            } else {
                file.write_all(zip.get_ref()).unwrap()
            }
        }
        let cabinet = cw.finish().unwrap().into_inner();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut c = cfb::create(path).unwrap();
        c.create_stream("/Data1.cab").unwrap().write_all(&cabinet).unwrap();
        c.create_stream("/Summary").unwrap().write_all(b"not a cabinet").unwrap();
        c.flush().unwrap();
    }

    #[test]
    fn channel_presets_fall_back_to_the_cached_msi() {
        let t = std::env::temp_dir().join(format!("oa-assets-msi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let root = t.join("c");
        let pp = root.join("Program Files (x86)/Universal Audio/Powered Plugins");
        fs::create_dir_all(&pp).unwrap();
        fs::write(pp.join("UA.xml"), "<UA></UA>").unwrap();
        let cache = root.join("ProgramData/Package Cache");
        fake_msi(
            &cache.join(format!("{APOLLO_MSI_GUID}v9.0.0/apollo-32bit.msi")),
            &["Channel Strip/Old/Old.uadchannel"],
            cab::CompressionType::None,
        );
        fake_msi(
            &cache.join(format!("{APOLLO_MSI_GUID}v12.0.0/apollo-32bit.msi")),
            &[
                "Channel Strip/",
                "Channel Strip/Vocals/",
                "Channel Strip/Vocals/Lead 10.uadchannel",
                "Channel Strip/Vocals/Lead 9.uadchannel",
                "Channel Strip/Init.uadchannel",
                "Channel Strip/Vocals/readme.txt",
                "Other/x.uadchannel",
            ],
            cab::CompressionType::MsZip,
        );
        let chan = |data: &Path| -> Value {
            serde_json::from_slice(&fs::read(data.join("ua-catalogue/channel-presets.json")).unwrap()).unwrap()
        };

        let data = t.join("data");
        let w = write(&root, &data).unwrap();
        assert_eq!(w.channel_presets, 3);
        let c = chan(&data);
        assert_eq!(c["source"]["via"], format!("msi:{APOLLO_MSI_GUID}v12.0.0"));
        assert_eq!(
            c["presets"],
            json!([
                { "value": "Init", "type": "file", "locked": true },
                { "value": "Vocals", "type": "folder", "locked": true, "children": [
                    { "value": "Lead 9", "type": "file", "locked": true },
                    { "value": "Lead 10", "type": "file", "locked": true },
                ] },
            ])
        );

        // The install's own folder wins.
        fs::create_dir_all(pp.join("Presets/Channel Strip")).unwrap();
        fs::write(pp.join("Presets/Channel Strip/Own.uadchannel"), "").unwrap();
        let data = t.join("data2");
        write(&root, &data).unwrap();
        let c = chan(&data);
        assert_eq!((c["source"]["via"].as_str(), c["presets"][0]["value"].as_str()), (Some("installed"), Some("Own")));

        // Neither: empty, via null.
        fs::remove_dir_all(pp.join("Presets")).unwrap();
        fs::remove_dir_all(&cache).unwrap();
        let data = t.join("data3");
        assert_eq!(write(&root, &data).unwrap().channel_presets, 0);
        assert_eq!(chan(&data)["source"]["via"], Value::Null);
        fs::remove_dir_all(&t).ok();
    }
}
