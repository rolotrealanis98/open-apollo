//! `oa-assets catalogue`: UA's plug-in catalogue and factory preset menus, rebuilt from the
//! user's own UAD install into `<data>/ua-catalogue/` (the files `mixer-engine/ua_catalogue.py`
//! overlays on the shipped device map at daemon start):
//!
//! - `plugins.json`: name, short names, categories, VST id, render id, plug-in id and the
//!   factory preset tree of every plug-in (`UA.xml`, `Manifest/*/manifest.json`, `Presets/`)
//! - `channel-presets.json`: the factory channel-strip preset tree (`Presets/Channel Strip`)
//! - `io-presets.json`: the factory I/O-map preset names (`Presets/IOPresets`)
//!
//! Names only: no preset file, no UA code and no licence state leaves the install. Without
//! this step the daemon's `/plugins` list is empty and the console's browser shows nothing.
//! Port of the private Python prototype (`ua_import_proto.py`); the Mixer Helper help text it
//! also produced is left out (the helper runs without it).

use std::cmp::Ordering;
use std::collections::BTreeMap;
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
        let render_id = m["renderID"]
            .as_u64()
            .or_else(|| m["renderid"].as_u64())
            .or_else(|| x.and_then(|p| p.renderid));
        let tree = presets.as_ref().and_then(|p| ci_path(p, &[name])).map(|d| dir_tree(&d, ".fxp")).unwrap_or_default();
        let mut categories = ua.categories.get(name).cloned().unwrap_or_default();
        if categories.is_empty() {
            // macOS manifests carry the categories; Windows ones rely on UA.xml.
            categories = m["categories"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect()).unwrap_or_default();
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
    let channel = presets.as_ref().and_then(|p| ci_path(p, &["Channel Strip"])).map(|d| dir_tree(&d, ".uadchannel")).unwrap_or_default();
    let mut io: Vec<String> = presets
        .as_ref()
        .and_then(|p| ci_path(p, &["IOPresets"]))
        .map(|d| list(&d).into_iter().filter_map(|f| f.file_name()?.to_str()?.strip_suffix(".uadio").map(str::to_owned)).collect())
        .unwrap_or_default();
    io.sort_by(|a, b| natural_cmp(a, b));

    let dir = data.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let source = json!({ "uad": ua.release, "from": root });
    let doc = |part: &str, key: &str, body: Value| -> Result<(), String> {
        let v = json!({ "format": format!("{FORMAT}/{part}"), "version": VERSION, "source": source, key: body });
        let text = serde_json::to_string(&v).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{part}.json"));
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    let written = Written { dir: dir.clone(), plugins: plugins.len(), channel_presets: count(&channel), io_presets: io.len(), uad_version: ua.release.clone() };
    doc("plugins", "plugins", Value::Array(plugins))?;
    doc("channel-presets", "presets", channel)?;
    doc("io-presets", "presets", json!(io))?;
    crate::import::record(
        data,
        "ua_catalogue",
        json!({ "from": root, "version": ua.release, "plugins": written.plugins, "channel_presets": written.channel_presets, "io_presets": written.io_presets, "imported_at": crate::import::unix_now() }),
    )
    .map_err(|e| format!("manifest: {e}"))?;
    Ok(written)
}

fn count(tree: &Value) -> usize {
    tree.as_array().map(|a| a.iter().map(|e| if e["type"] == "folder" { count(&e["children"]) } else { 1 }).sum()).unwrap_or(0)
}

fn list(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
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
                name.to_ascii_lowercase().ends_with(ext).then(|| (false, name, p))
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
                        out.plugins.insert(title, UaPlugin { renderid: attr(tag, "renderid").and_then(|v| v.parse().ok()), vstids: attr(tag, "vstids") });
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
                    if let Some(short) = attr(tag, "title").filter(|s| !s.is_empty()) {
                        if !parent.is_empty() {
                            out.short_names.entry(parent.clone()).or_default().push(short);
                        }
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
    Some(tag[s..s + e].replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'"))
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
        fs::write(pp.join("UA.xml"), r#"<?xml version="1.0"?><UA><verinfo><swReleaseName>12.0.0.602</swReleaseName></verinfo>
<plugins><plugin id="0" title="UAD Test Comp" renderid="2096" vstids="58AU"/></plugins>
<categories><category title="Compressors &amp; Limiters"><plugin title="UAD Test Comp"/></category></categories>
<shortnames><names title="UAD Test Comp"><name title="Test Comp"/><name title="TC"/></names></shortnames></UA>"#).unwrap();
        fs::write(pp.join("Manifest/UAD Test Comp/manifest.json"), r#"{"name":"UAD Test Comp","plugin_id":"uaudio_test_comp"}"#).unwrap();
        fs::write(pp.join("Manifest/UAD Console Input Strip Header/manifest.json"), r#"{"name":"UAD Console Input Strip Header"}"#).unwrap();
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
        assert_eq!((e["vst_id"].as_str(), e["render_id"].as_u64(), e["plugin_id"].as_str()), (Some("58AU"), Some(2096), Some("uaudio_test_comp")));
        assert_eq!(e["presets"][0]["value"], "Default");
        assert_eq!(e["presets"][1]["type"], "folder");
        assert_eq!(e["presets"][1]["children"][0]["value"], "Lead 9");
        let c: Value = serde_json::from_slice(&fs::read(data.join("ua-catalogue/channel-presets.json")).unwrap()).unwrap();
        assert_eq!(c["presets"][0]["children"][0]["value"], "Fat Bass");
        let m = crate::import::read_manifest(&data);
        assert_eq!(m["ua_catalogue"]["plugins"], 1);
        fs::remove_dir_all(&t).ok();
    }
}
