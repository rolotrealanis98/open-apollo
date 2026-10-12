//! UA's own HTML/JS plug-in GUIs ("lunacomponents"): discovery, matching an engine `EffectName`
//! to a component, and the URL the webview loads. The webview itself is behind the `webview`
//! feature ([`crate::webview`]); the socket bridge the GUI talks to is described in WEBBRIDGE.md.
//!
//! A component is `<id>.lunacomponent/` with
//! - `gui.bundle/Contents/Resources/html/{index.html, bundle.js, manifest.json, images/}`
//! - `algo.bundle/Contents/Resources/{manifest.json, preset_categories.json, presets/}`
//!
//! They are UA's files, read from the user's install and never bundled.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use houston::Rgba;
use serde::Deserialize;

/// Where lunacomponents are looked for; every existing folder is scanned and the first copy of a
/// component id wins:
/// 1. `$UACONSOLE_LUNA_COMPONENTS`
/// 2. `~/.local/share/open-apollo/lunacomponents` (a copied `Plug-Ins` folder)
/// 3. `/Library/Application Support/Universal Audio/Plug-Ins` (macOS install)
/// 4. `~/wine-ua-work/lunacomponents` (development copy)
pub struct LunaLocator;

impl LunaLocator {
    pub const ENV: &'static str = "UACONSOLE_LUNA_COMPONENTS";

    pub fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(p) = std::env::var(Self::ENV) {
            if !p.is_empty() {
                out.push(PathBuf::from(p));
            }
        }
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        out.push(home.join(".local/share/open-apollo/lunacomponents"));
        out.push(PathBuf::from("/Library/Application Support/Universal Audio/Plug-Ins"));
        out.push(home.join("wine-ua-work/lunacomponents"));
        out
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub struct WindowSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Default, Deserialize)]
struct GuiAppearance {
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct GuiManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    appearance: GuiAppearance,
    #[serde(default)]
    window: Option<WindowSize>,
    #[serde(default)]
    client_interface_support_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AlgoManifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    plugin_id: String,
    #[serde(default)]
    short_names: Vec<String>,
    /// The plug-in's version (`"1.0.5"`), shown in the Help & Video panel.
    #[serde(default)]
    version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LunaComponent {
    /// `uaudio_ssl_g_bus_compressor`.
    pub id: String,
    /// The `.lunacomponent` folder.
    pub root: PathBuf,
    /// `gui.bundle/Contents/Resources/html`.
    pub html_dir: PathBuf,
    /// Display name ("SSL G Bus Compressor").
    pub name: String,
    /// Other names from the algo manifest ("SSL G Bus Comp", "SSL G Comp", …).
    pub short_names: Vec<String>,
    /// The GUI's natural size in points (`manifest.json` `window`).
    pub window: WindowSize,
    pub client_interface_version: Option<String>,
    /// The algo manifest's `version` ("1.0.5"): the Help & Video panel's "v1.0.5".
    pub version: Option<String>,
    /// False when `html/images` is missing (the GUI then cannot draw its art).
    pub has_images: bool,
    /// `manifest.json` `appearance.colors` ("window-background", "scrollbar-thumbnail", …):
    /// Console tints the toolbar and footer of HTML plug-ins with them.
    pub colors: BTreeMap<String, Rgba>,
}

impl LunaComponent {
    /// Reads a `.lunacomponent` folder; None when it has no `index.html`.
    pub fn open(root: &Path) -> Option<Self> {
        let html_dir = root.join("gui.bundle/Contents/Resources/html");
        if !html_dir.join("index.html").is_file() {
            return None;
        }
        let read = |p: PathBuf| std::fs::read_to_string(p).ok();
        let gui: GuiManifest = read(html_dir.join("manifest.json")).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let algo: AlgoManifest =
            read(root.join("algo.bundle/Contents/Resources/manifest.json")).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let stem = root.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let id = if algo.plugin_id.is_empty() { stem } else { algo.plugin_id };
        let name = [gui.name, algo.name].into_iter().find(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
        Some(Self {
            id,
            root: root.to_path_buf(),
            has_images: html_dir.join("images").is_dir(),
            html_dir,
            name,
            short_names: algo.short_names,
            version: algo.version.filter(|v| !v.is_empty()),
            window: gui.window.unwrap_or(WindowSize { width: 300.0, height: 600.0 }),
            client_interface_version: gui.client_interface_support_version,
            colors: gui.appearance.colors.iter().filter_map(|(k, v)| Some((k.clone(), Rgba::parse(v)?))).collect(),
        })
    }

    pub fn index_html(&self) -> PathBuf {
        self.html_dir.join("index.html")
    }

    /// `file://…/index.html?<query>`: the Windows (Console 1.2.x) layout, `GetSelectedInsertURL`
    /// with a `file://` GUI base (WEBBRIDGE.md §6).
    pub fn url(&self, query: &GuiQuery) -> String {
        format!("file://{}?{}", percent_path(&self.index_html()), query.to_query())
    }

    /// `ua-content://gui/index.html?<query>`: Console 1.3.1's macOS form, served from this
    /// component's `html` folder by a `ua-content` handler ([`crate::content`]).
    pub fn content_url(&self, query: &GuiQuery) -> String {
        crate::content::page_url(query)
    }

    /// True when `effect_name` (engine `EffectName`, e.g. "UAD SSL G Bus Compressor") names this
    /// component: equal after normalisation to its name, a short name, or its id.
    pub fn matches_effect(&self, effect_name: &str) -> bool {
        let wanted = effect_keys(effect_name);
        let mut have = vec![normalize(&self.name), normalize(self.id.strip_prefix("uaudio_").unwrap_or(&self.id))];
        have.extend(self.short_names.iter().map(|s| normalize(s)));
        wanted.iter().any(|w| !w.is_empty() && have.contains(w))
    }
}

/// Query parameters the GUI's `@uaudio/client` reads, with Console's keys `host`, `port`, `path`
/// and (1.3.1, `ua-content` URLs) `plugin_id` (WEBBRIDGE.md §1). `path` must not be the first
/// key: one of the GUI's parsers glues the first key to the page URL.
#[derive(Clone, Debug, PartialEq)]
pub struct GuiQuery {
    /// Host of the WebSocket the GUI connects to (`host`).
    pub ip: String,
    /// Its port (`port`; the client's default is 4718).
    pub port: u16,
    /// The plug-in slot in the engine tree (`path`), e.g. `/devices/0/inputs/0/effects/3`.
    pub path: String,
    /// `plugin_id`: Console 1.3.1 appends it to `ua-content` URLs (the component id,
    /// `uaudio_ua_1176_rev_a`). The 1.7.0 and 1.9.0 clients do not read it [obs].
    pub plugin_id: Option<String>,
    /// Extra `key=value` pairs, appended in order.
    pub extra: Vec<(String, String)>,
}

/// The client's default WebSocket port (`"4718"`, HB deviced).
pub const CLIENT_DEFAULT_PORT: u16 = 4718;

impl GuiQuery {
    /// `host`, `port`, `path`. Like `GetSelectedInsertURL`, one trailing `/` of `path` is dropped.
    pub fn new(ip: &str, port: u16, path: &str) -> Self {
        Self { ip: ip.into(), port, path: trim_slash(path).into(), plugin_id: None, extra: vec![] }
    }

    /// Console 1.3.1's keys for an installed component (`ua-content` URL): `host`, `port`, `path`
    /// (one trailing `/` dropped), `plugin_id`, in that order (Mac `GetSelectedInsertURL`
    /// 0x1df630).
    pub fn console(ip: &str, port: u16, path: &str, plugin_id: &str) -> Self {
        Self::new(ip, port, path).with_plugin_id(plugin_id)
    }

    pub fn with_plugin_id(mut self, plugin_id: &str) -> Self {
        self.plugin_id = (!plugin_id.is_empty()).then(|| plugin_id.to_string());
        self
    }

    pub fn to_query(&self) -> String {
        let mut parts = vec![format!("host={}", encode(&self.ip)), format!("port={}", self.port), format!("path={}", encode(&self.path))];
        if let Some(id) = &self.plugin_id {
            parts.push(format!("plugin_id={}", encode(id)));
        }
        parts.extend(self.extra.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))));
        parts.join("&")
    }

    /// What the GUI bundle makes of a page URL (`ua-content://…`, `file://…` or a bare query), by
    /// its own two parsers (WEBBRIDGE.md §1):
    /// - `MixerBase.doConnect` splits `location.search` on `&`/`=`, keeps pairs with exactly one
    ///   `=`, and takes the host from `ip`, then `host`;
    /// - `Application.getURLParams` runs `URLSearchParams` over the **whole href**, so the first
    ///   key is glued to the URL and lost; it supplies `deviceId` (the host fallback), `port`,
    ///   `transport` and `path`, and its `path` wins.
    ///
    /// `port` defaults to 4718 and one trailing `/` is dropped from `path`. None when the GUI
    /// would find no `path` (it then starts offline, without connecting): no `path` key, or
    /// `path` as the first key of an href.
    pub fn parse(href: &str) -> Option<Self> {
        let search = match href.split_once('?') {
            Some((_, q)) => q,
            None if href.contains('=') && !href.contains("://") => href,
            None => "",
        };
        let search = search.split('#').next().unwrap_or("");
        // doConnect: `M(window.location.search)`.
        let connect: Vec<(String, String)> = search
            .split('&')
            .filter_map(|kv| {
                let mut it = kv.split('=');
                let (k, v) = (it.next()?, it.next()?);
                it.next().is_none().then(|| (percent_decode(k), percent_decode(v)))
            })
            .collect();
        // getURLParams: `new URLSearchParams(window.location.href)`; the first key carries the
        // href's prefix, so it never matches.
        let whole = if href.contains('?') || href.contains("://") { href } else { "" };
        let url_params: Vec<(String, String)> = whole
            .split('#')
            .next()
            .unwrap_or("")
            .split('&')
            .map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (form_decode(k), form_decode(v))
            })
            .collect();
        let get = |pairs: &[(String, String)], key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        // The GUI goes offline when getURLParams finds no `path` (`n2 = !t2.path`); a bare query
        // has no href, so doConnect's copy is all there is.
        let path = if whole.is_empty() { get(&connect, "path") } else { get(&url_params, "path") }.filter(|p| !p.is_empty())?;
        let ip = get(&connect, "ip").or_else(|| get(&connect, "host")).or_else(|| get(&url_params, "deviceId")).unwrap_or_default();
        let port = get(&url_params, "port").or_else(|| get(&connect, "port")).and_then(|p| p.parse().ok()).unwrap_or(CLIENT_DEFAULT_PORT);
        let plugin_id = get(&connect, "plugin_id").or_else(|| get(&url_params, "plugin_id")).filter(|p| !p.is_empty());
        let known = ["ip", "host", "deviceId", "port", "path", "plugin_id"];
        let extra = connect.iter().filter(|(k, _)| !known.contains(&k.as_str())).cloned().collect();
        Some(Self { ip, port, path: trim_slash(&path).into(), plugin_id, extra })
    }
}

/// `GetSelectedInsertURL` drops one trailing `/` from the insert path.
fn trim_slash(path: &str) -> &str {
    path.strip_suffix('/').filter(|p| !p.is_empty()).unwrap_or(path)
}

/// Every component found in the [`LunaLocator`] folders.
#[derive(Clone, Debug, Default)]
pub struct LunaLibrary {
    pub folders: Vec<PathBuf>,
    pub components: Vec<LunaComponent>,
}

impl LunaLibrary {
    pub fn locate() -> Self {
        Self::scan(&LunaLocator::candidates())
    }

    pub fn scan(folders: &[PathBuf]) -> Self {
        let mut lib = Self::default();
        for folder in folders {
            let Ok(dir) = std::fs::read_dir(folder) else { continue };
            let mut found: Vec<PathBuf> =
                dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "lunacomponent")).collect();
            if found.is_empty() {
                continue;
            }
            found.sort();
            lib.folders.push(folder.clone());
            for root in found {
                if let Some(c) = LunaComponent::open(&root) {
                    if !lib.components.iter().any(|x| x.id == c.id) {
                        lib.components.push(c);
                    }
                }
            }
        }
        lib
    }

    /// The component for an engine `EffectName`, preferring one with its images present.
    pub fn component_for_effect(&self, effect_name: &str) -> Option<&LunaComponent> {
        let mut hits = self.components.iter().filter(|c| c.matches_effect(effect_name));
        let first = hits.next()?;
        if first.has_images { Some(first) } else { hits.find(|c| c.has_images).or(Some(first)) }
    }

    /// By id (`uaudio_ssl_g_bus_compressor` or `ssl_g_bus_compressor`), name, or effect name.
    pub fn named(&self, name: &str) -> Option<&LunaComponent> {
        let id = name.strip_suffix(".lunacomponent").unwrap_or(name);
        self.components
            .iter()
            .find(|c| c.id == id || c.id.strip_prefix("uaudio_") == Some(id))
            .or_else(|| self.component_for_effect(name))
    }
}

/// Lowercase alphanumerics only.
fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// `EffectName` keys: as given, without "UAD ", and without "UAD UA " (spec names are
/// "UAD UA 1176 Rev A" where the component is "1176 Rev A").
fn effect_keys(effect_name: &str) -> Vec<String> {
    let mut keys = vec![normalize(effect_name)];
    if let Some(rest) = effect_name.strip_prefix("UAD ") {
        keys.push(normalize(rest));
        if let Some(rest) = rest.strip_prefix("UA ") {
            keys.push(normalize(rest));
        }
    }
    keys
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_path(p: &Path) -> String {
    encode(&p.to_string_lossy())
}

/// `decodeURIComponent`, leniently: invalid escapes stay as written.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `URLSearchParams` decoding: `+` is a space, then percent escapes.
fn form_decode(s: &str) -> String {
    percent_decode(&s.replace('+', " "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str, id: &str, short: &[&str]) -> LunaComponent {
        LunaComponent {
            id: id.into(),
            root: PathBuf::from(format!("/x/{id}.lunacomponent")),
            html_dir: PathBuf::from(format!("/x/{id}.lunacomponent/gui.bundle/Contents/Resources/html")),
            name: name.into(),
            short_names: short.iter().map(|s| s.to_string()).collect(),
            window: WindowSize { width: 300.0, height: 621.0 },
            client_interface_version: None,
            version: None,
            has_images: true,
            colors: BTreeMap::new(),
        }
    }

    #[test]
    fn effect_names_match_components() {
        let ssl = component("SSL G Bus Compressor", "uaudio_ssl_g_bus_compressor", &["SSL G Bus Comp", "SSL G"]);
        assert!(ssl.matches_effect("UAD SSL G Bus Compressor"));
        assert!(ssl.matches_effect("SSL G Bus Comp"));
        assert!(!ssl.matches_effect("UAD SSL E Channel Strip"));
        let a = component("1176 Rev A", "uaudio_ua_1176_rev_a", &["1176 Rev A", "1176 A"]);
        assert!(a.matches_effect("UAD UA 1176 Rev A"));
        assert!(!a.matches_effect("UAD UA 1176LN Rev E"));
        let lib = LunaLibrary { folders: vec![], components: vec![ssl, a] };
        assert_eq!(lib.named("ssl_g_bus_compressor").map(|c| c.name.as_str()), Some("SSL G Bus Compressor"));
        assert_eq!(lib.component_for_effect("UAD UA 1176 Rev A").map(|c| c.id.as_str()), Some("uaudio_ua_1176_rev_a"));
    }

    #[test]
    fn console_131_keys_survive_both_bundle_parsers() {
        let c = component("1176 Rev A", "uaudio_ua_1176_rev_a", &[]);
        let q = GuiQuery::console("127.0.0.1", 41234, "/devices/0/inputs/2/effects/3/", &c.id);
        let url = c.content_url(&q);
        assert_eq!(url, "ua-content://gui/index.html?host=127.0.0.1&port=41234&path=/devices/0/inputs/2/effects/3&plugin_id=uaudio_ua_1176_rev_a");
        // The GUI's view of it: host from doConnect, port/path from getURLParams.
        assert_eq!(GuiQuery::parse(&url), Some(q.clone()));
        // The file:// (1.2.x) form reads the same.
        let file = c.url(&GuiQuery::new("127.0.0.1", 41234, "/devices/0/inputs/2/effects/3"));
        let back = GuiQuery::parse(&file).unwrap();
        assert_eq!((back.ip.as_str(), back.port, back.path.as_str(), back.plugin_id), ("127.0.0.1", 41234, "/devices/0/inputs/2/effects/3", None));
        // `ip` beats `host`; `deviceId` is the fallback; the default port is 4718.
        let p = GuiQuery::parse("ua-content://gui/index.html?x=1&deviceId=10.0.0.2&path=%2Fa%2Fb").unwrap();
        assert_eq!((p.ip.as_str(), p.port, p.path.as_str()), ("10.0.0.2", 4718, "/a/b"));
        let p = GuiQuery::parse("ua-content://gui/index.html?host=h&ip=i&path=/a").unwrap();
        assert_eq!(p.ip, "i");
        // `path` first, or missing: the GUI starts offline.
        assert_eq!(GuiQuery::parse("ua-content://gui/index.html?path=/a&host=h&port=1"), None);
        assert_eq!(GuiQuery::parse("ua-content://gui/index.html?host=h&port=1"), None);
        // A bare query (no href) is read like location.search.
        assert_eq!(GuiQuery::parse("host=h&port=9&path=/z/").map(|q| q.path), Some("/z".into()));
    }

    #[test]
    fn url_is_file_with_query() {
        let c = component("SSL G Bus Compressor", "uaudio_ssl_g_bus_compressor", &[]);
        let url = c.url(&GuiQuery::new("127.0.0.1", 4718, "/devices/0/inputs/2/effects/3"));
        assert_eq!(
            url,
            "file:///x/uaudio_ssl_g_bus_compressor.lunacomponent/gui.bundle/Contents/Resources/html/index.html?host=127.0.0.1&port=4718&path=/devices/0/inputs/2/effects/3"
        );
    }
}
