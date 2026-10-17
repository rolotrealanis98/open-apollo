//! The `ua-content://` scheme UAD Console 1.3.1 (macOS) loads lunacomponent GUIs from, as a pure
//! resolver and response builder. The wry host in [`crate::webview`] and Apple hosts
//! (`WKURLSchemeHandler`, see `console-rs/apple/README.md`) answer the scheme with the same rules.
//!
//! What Console 1.3.1 does (Mac `Rack` 1.3.1 x86_64, REA evidence `ev_c165e9a8…`, `ev_69f64119…`,
//! `ev_7afd3171…`; disassembly of the `UAPWSchemeHandler` methods):
//!
//! - **URL.** `Rack_PluginWebViewHelper::GetSelectedInsertURL` (0x1df630), for an installed
//!   component (source mode 5): when the GUI base is a `file://` URL, the bundle folder goes to an
//!   out-parameter and the URL becomes `"ua-content"` (0x1a44280) + `"://gui"` + `/index.html` +
//!   `?host=<client host>&port=<client port>&path=<insert path, trailing / trimmed>&plugin_id=<id>`.
//!   [obs] `plugin_id` is appended because the out-parameter is set. The Windows 1.2.x layout
//!   keeps `file://…/index.html` with the same keys and no `plugin_id` ([`LunaComponent::url`]).
//! - **Handler.** One `UAPWSchemeHandler` per web view, registered with
//!   `setURLSchemeHandler:forURLScheme:` (0x1a43360), holding `mRootPath` (the component's
//!   `html` folder), `mAllowedRoots` and `mActiveTasks`.
//!   - `resolvePathForURL:` (0x1a3e620): `NSURLComponents.path` (percent-decoded; the host
//!     `gui` is ignored), `stringByStandardizingPath`, appended to the root; the result must equal
//!     the root or lie under `root + "/"`, and the root must lie under an allowed root.
//!   - `webView:startURLSchemeTask:` (0x1a3ea10): reads the file off the main thread
//!     (`dataWithContentsOfFile:`), then answers `NSHTTPURLResponse` **200**, `HTTP/1.1`, with
//!     `Content-Type` (by lowercased extension, [`mime_type`]; default
//!     `application/octet-stream`), `Content-Length`, `Access-Control-Allow-Origin: *` and
//!     `Cache-Control: no-cache`.
//!   - A path that does not resolve, or a file that cannot be read, fails the task with
//!     `NSURLErrorDomain` −1100 (`NSURLErrorFileDoesNotExist`): "No content root or invalid path",
//!     "Failed to read file". A wry handler must return a response, so [`ContentRoots::respond`]
//!     answers 404 there.
//!
//! console-rs additions (not in Console): `ua-content://gui/<component id>/…` addresses one of
//! several components from a single handler (a host serving a library), and an empty path serves
//! `index.html`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::web::{GuiQuery, LunaComponent, LunaLibrary, percent_decode};

/// The scheme name Console registers for its content.
pub const SCHEME: &str = "ua-content";
/// The URL host Console writes (`"://gui"`); the handler ignores it.
pub const HOST: &str = "gui";

/// `ua-content://gui/index.html?<query>`: Console 1.3.1's URL for an installed component. The
/// handler serving it must be rooted at that component's `html` folder
/// ([`ContentRoots::for_component`]).
pub fn page_url(query: &GuiQuery) -> String {
    format!("{SCHEME}://{HOST}/index.html?{}", query.to_query())
}

/// `ua-content://gui/<component id>/index.html?<query>`: the component-qualified form, for a
/// handler that serves a whole library ([`ContentRoots::for_library`]). Relative loads stay in the
/// component (`bundle.js` → `ua-content://gui/<id>/bundle.js`).
pub fn component_url(component_id: &str, query: &GuiQuery) -> String {
    format!("{SCHEME}://{HOST}/{component_id}/index.html?{}", query.to_query())
}

/// Console 1.3.1's `Content-Type` table (`UAPWSchemeHandler`, keyed by the lowercased extension).
pub fn mime_type(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = match name.rsplit_once('.') {
        Some((_, e)) => e.to_ascii_lowercase(),
        None => String::new(),
    };
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// A scheme reply: status, headers and body, independent of any web view API.
#[derive(Clone, Debug, PartialEq)]
pub struct ContentResponse {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl ContentResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn not_found() -> Self {
        Self { status: 404, headers: vec![("Cache-Control", "no-cache".into())], body: Vec::new() }
    }
}

/// What one `ua-content` handler may serve.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContentRoots {
    /// `mRootPath`: the `html` folder `ua-content://gui/<file>` resolves in.
    pub root: Option<PathBuf>,
    /// `mAllowedRoots`: when non-empty, a root must lie in one of these folders.
    pub allowed: Vec<PathBuf>,
    /// Components addressable as `ua-content://gui/<id>/<file>` (id → `html` folder).
    pub components: BTreeMap<String, PathBuf>,
}

impl ContentRoots {
    /// Rooted at one component's `html` folder, as Console sets up each plug-in web view.
    pub fn for_component(c: &LunaComponent) -> Self {
        let mut components = BTreeMap::new();
        components.insert(c.id.clone(), c.html_dir.clone());
        Self { root: Some(c.html_dir.clone()), allowed: vec![c.root.clone()], components }
    }

    /// Every component of `lib` by id, limited to the library's folders; `root` (if any) also
    /// answers the unqualified Console form.
    pub fn for_library(lib: &LunaLibrary, root: Option<&LunaComponent>) -> Self {
        Self {
            root: root.map(|c| c.html_dir.clone()),
            allowed: lib.folders.clone(),
            components: lib.components.iter().map(|c| (c.id.clone(), c.html_dir.clone())).collect(),
        }
    }

    /// The file a request path (`/index.html`, `/<id>/images/x.png`, percent-encoded) names, or
    /// None when it leaves its root, its root is not allowed, or no root applies. The query and
    /// fragment are ignored if present.
    pub fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let raw = url_path.split(['?', '#']).next().unwrap_or("");
        let segments = standardize(&percent_decode(raw))?;
        let (root, rest) = match segments.first().and_then(|s| self.components.get(s)) {
            Some(dir) => (dir, &segments[1..]),
            None => (self.root.as_ref()?, &segments[..]),
        };
        if !self.allowed.is_empty() && !self.allowed.iter().any(|a| within(root, a)) {
            return None;
        }
        let mut path = root.clone();
        if rest.is_empty() {
            path.push("index.html");
        }
        path.extend(rest);
        // Defence in depth against symlinks out of the bundle: compare real paths when both exist.
        if let (Ok(real), Ok(real_root)) = (path.canonicalize(), root.canonicalize())
            && !within(&real, &real_root)
        {
            return None;
        }
        Some(path)
    }

    /// The reply Console's handler gives for `url_path` (200 with its headers, or 404).
    pub fn respond(&self, url_path: &str) -> ContentResponse {
        let Some(path) = self.resolve(url_path) else { return ContentResponse::not_found() };
        let Ok(body) = std::fs::read(&path) else { return ContentResponse::not_found() };
        ContentResponse {
            status: 200,
            headers: vec![
                ("Content-Type", mime_type(&path.to_string_lossy()).to_string()),
                ("Content-Length", body.len().to_string()),
                ("Access-Control-Allow-Origin", "*".into()),
                ("Cache-Control", "no-cache".into()),
            ],
            body,
        }
    }
}

/// `stringByStandardizingPath` on the URL path, lexically: drops empty and `.` segments and folds
/// `..`; None when `..` would climb above the root.
fn standardize(path: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for seg in path.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => {
                // A segment must stay one path component (no drive prefixes, no NUL).
                if s.contains('\0')
                    || Path::new(s).components().count() != 1
                    || !matches!(Path::new(s).components().next(), Some(Component::Normal(_)))
                {
                    return None;
                }
                out.push(s.to_string());
            }
        }
    }
    Some(out)
}

fn within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::WindowSize;

    fn bundle(dir: &Path, id: &str) -> LunaComponent {
        let root = dir.join(format!("{id}.lunacomponent"));
        let html = root.join("gui.bundle/Contents/Resources/html");
        std::fs::create_dir_all(html.join("images")).unwrap();
        std::fs::write(html.join("index.html"), format!("<html>{id}</html>")).unwrap();
        std::fs::write(html.join("bundle.js"), "console.log(1)").unwrap();
        std::fs::write(html.join("images/knob@2x.png"), [0x89u8, b'P', b'N', b'G']).unwrap();
        std::fs::write(html.join("hbgf_dev_features.json"), "{}").unwrap();
        std::fs::write(root.join("secret.txt"), "outside html").unwrap();
        LunaComponent {
            id: id.into(),
            root: root.clone(),
            html_dir: html,
            name: id.into(),
            short_names: vec![],
            window: WindowSize { width: 950.0, height: 175.0 },
            client_interface_version: None,
            version: None,
            has_images: true,
            colors: BTreeMap::new(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("plugin-ui-content-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn console_url_form() {
        let q = GuiQuery::console("127.0.0.1", 4710, "/devices/0/inputs/2/effects/3/", "uaudio_ua_1176_rev_a");
        assert_eq!(
            page_url(&q),
            "ua-content://gui/index.html?host=127.0.0.1&port=4710&path=/devices/0/inputs/2/effects/3&plugin_id=uaudio_ua_1176_rev_a"
        );
        assert_eq!(
            component_url("uaudio_ua_1176_rev_a", &GuiQuery::new("127.0.0.1", 9000, "/p")),
            "ua-content://gui/uaudio_ua_1176_rev_a/index.html?host=127.0.0.1&port=9000&path=/p"
        );
    }

    #[test]
    fn resolves_like_uapw_scheme_handler() {
        let dir = temp("resolve");
        let a = bundle(&dir, "uaudio_ua_1176_rev_a");
        let b = bundle(&dir, "uaudio_ssl_g_bus_compressor");
        let one = ContentRoots::for_component(&a);
        assert_eq!(one.resolve("/index.html"), Some(a.html_dir.join("index.html")));
        assert_eq!(one.resolve("/images/knob%402x.png"), Some(a.html_dir.join("images/knob@2x.png")));
        assert_eq!(one.resolve("/./images/../bundle.js"), Some(a.html_dir.join("bundle.js")));
        assert_eq!(one.resolve(""), Some(a.html_dir.join("index.html")));
        // Out of the html folder: refused, encoded or not.
        assert_eq!(one.resolve("/../../../secret.txt"), None);
        assert_eq!(one.resolve("/%2e%2e/%2e%2e/%2e%2e/secret.txt"), None);
        assert_eq!(one.resolve("/..%2f..%2f..%2fsecret.txt"), None);
        // A component-qualified path reaches only registered components.
        assert_eq!(one.resolve("/uaudio_ua_1176_rev_a/bundle.js"), Some(a.html_dir.join("bundle.js")));
        assert_eq!(
            one.resolve("/uaudio_ssl_g_bus_compressor/bundle.js"),
            Some(a.html_dir.join("uaudio_ssl_g_bus_compressor/bundle.js"))
        );

        let lib = LunaLibrary { folders: vec![dir.clone()], components: vec![a.clone(), b.clone()] };
        let all = ContentRoots::for_library(&lib, None);
        assert_eq!(all.resolve("/uaudio_ssl_g_bus_compressor/index.html"), Some(b.html_dir.join("index.html")));
        assert_eq!(all.resolve("/index.html"), None, "no default root");
        assert_eq!(
            all.resolve("/uaudio_ssl_g_bus_compressor/../uaudio_ua_1176_rev_a/index.html"),
            Some(a.html_dir.join("index.html"))
        );
        // A root outside the allowed folders is refused.
        let mut stray = ContentRoots::for_component(&a);
        stray.allowed = vec![dir.join("elsewhere")];
        assert_eq!(stray.resolve("/index.html"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn responses_carry_consoles_headers() {
        let dir = temp("respond");
        let a = bundle(&dir, "uaudio_ua_1176_rev_a");
        let roots = ContentRoots::for_component(&a);
        let r = roots.respond("/bundle.js");
        assert_eq!(r.status, 200);
        assert_eq!(r.header("content-type"), Some("text/javascript"));
        assert_eq!(r.header("Content-Length"), Some("14"));
        assert_eq!(r.header("Access-Control-Allow-Origin"), Some("*"));
        assert_eq!(r.header("Cache-Control"), Some("no-cache"));
        assert_eq!(r.body, b"console.log(1)");
        assert_eq!(roots.respond("/hbgf_dev_features.json").header("Content-Type"), Some("application/json"));
        assert_eq!(roots.respond("/images/knob@2x.png").header("Content-Type"), Some("image/png"));
        assert_eq!(roots.respond("/missing.js").status, 404);
        assert_eq!(roots.respond("/../secret.txt").status, 404);
        assert_eq!(mime_type("a/B.WOFF2"), "font/woff2");
        assert_eq!(mime_type("bundle.js.map"), "application/json");
        assert_eq!(mime_type("LICENSE"), "application/octet-stream");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
