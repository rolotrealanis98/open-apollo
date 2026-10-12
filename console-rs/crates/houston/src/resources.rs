use std::path::{Path, PathBuf};

/// The user's installed UAD Console `Resources/` folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resources {
    pub root: PathBuf,
    /// Console version, when an Info.plist sits next to (macOS) or inside (copy) the folder.
    pub console_version: Option<String>,
}

impl Resources {
    pub const ENV: &'static str = "CONSOLE_RESOURCES";

    /// Candidate folders in priority order (see ARCHITECTURE.md).
    pub fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(p) = std::env::var(Self::ENV) {
            if !p.is_empty() {
                out.push(PathBuf::from(p));
            }
        }
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let prefix = std::env::var("WINEPREFIX").map(PathBuf::from).unwrap_or_else(|_| home.join("wine-ua"));
        out.push(prefix.join("drive_c/Program Files (x86)/Universal Audio/UAD Console/Resources"));
        out.push(PathBuf::from("/Applications/Universal Audio/UAD Console.app/Contents/Resources"));
        out.push(home.join(".local/share/open-apollo/console-resources"));
        out
    }

    pub fn locate() -> Option<Self> {
        Self::candidates().into_iter().find_map(|p| Self::at(&p))
    }

    /// `path` if it looks like a Console Resources folder.
    pub fn at(path: &Path) -> Option<Self> {
        if !path.join("appearance/Appearance.json").is_file() {
            return None;
        }
        let version = [path.parent().map(|p| p.join("Info.plist")), Some(path.join("Info.plist"))]
            .into_iter()
            .flatten()
            .find_map(|plist| plist_short_version(&plist));
        Some(Self { root: path.to_path_buf(), console_version: version })
    }

    pub fn appearance_dir(&self) -> PathBuf {
        self.root.join("appearance")
    }
    pub fn layouts_dir(&self) -> PathBuf {
        self.root.join("layouts")
    }
    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }
    pub fn fonts_dir(&self) -> PathBuf {
        self.root.join("fonts")
    }

    /// `Plug-In Icons/<EffectName>.png`: inside Resources on Windows, system-wide on macOS.
    pub fn plugin_icons_dir(&self) -> PathBuf {
        let inside = self.root.join("Plug-In Icons");
        if inside.is_dir() {
            return inside;
        }
        PathBuf::from("/Library/Application Support/Universal Audio/Plug-In Icons")
    }
}

/// Minimal XML-plist scrape for `CFBundleShortVersionString` (binary plists are skipped).
fn plist_short_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let key = text.find("<key>CFBundleShortVersionString</key>")?;
    let rest = &text[key..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    Some(rest[start..start + end].trim().to_string())
}
