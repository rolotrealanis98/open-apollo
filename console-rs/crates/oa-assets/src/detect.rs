//! What UAD software a root holds.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::ci_path;
use crate::discover::{Root, Source};

#[derive(Clone, Debug)]
pub struct Console {
    pub dir: PathBuf,
    /// The folder console-rs reads (`appearance/Appearance.json`, `layouts/`, `images/`, `fonts/`).
    pub resources: PathBuf,
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Install {
    pub root: Root,
    pub console: Option<Console>,
    pub mixer_engine: Option<(PathBuf, Option<String>)>,
    /// UAD-2 Powered plug-in DLLs.
    pub powered_plugins: Vec<PathBuf>,
    /// UA VST3 bundles (UADx native plug-ins).
    pub vst3: Vec<PathBuf>,
    /// UA kernel drivers and their DriverStore packages.
    pub drivers: Vec<PathBuf>,
}

impl Install {
    pub fn found_anything(&self) -> bool {
        self.console.is_some()
            || self.mixer_engine.is_some()
            || !self.powered_plugins.is_empty()
            || !self.vst3.is_empty()
            || !self.drivers.is_empty()
    }

    pub fn to_json(&self) -> Value {
        let src = match &self.root.source {
            Source::Given => json!({"kind": "given"}),
            Source::Volume { device, fstype } => {
                json!({"kind": "volume", "device": device, "fstype": fstype})
            }
            Source::WinePrefix => json!({"kind": "wine-prefix"}),
        };
        json!({
            "root": self.root.path,
            "source": src,
            "console": self.console.as_ref().map(|c| json!({
                "dir": c.dir, "resources": c.resources, "version": c.version,
            })),
            "mixer_engine": self.mixer_engine.as_ref().map(|(p, v)| json!({"path": p, "version": v})),
            "powered_plugins": self.powered_plugins.len(),
            "vst3": self.vst3.len(),
            "drivers": self.drivers,
        })
    }
}

/// Look for UAD software under a root that stands in for `C:\`.
pub fn inspect(root: &Root) -> Install {
    let r = &root.path;
    Install {
        root: root.clone(),
        console: find_console(r),
        mixer_engine: ["Program Files", "Program Files (x86)"].iter().find_map(|pf| {
            let exe = ci_path(r, &[pf, "Universal Audio", "UAMixerEngine.exe"])?;
            Some((exe.clone(), crate::pe::file_version(&exe)))
        }),
        powered_plugins: ci_path(r, &["Program Files", "Universal Audio", "Powered Plugins"])
            .map(|d| walk(&d, 4, &|p| has_ext(p, "dll")))
            .unwrap_or_default(),
        vst3: ci_path(r, &["Program Files", "Common Files", "VST3"])
            .map(|d| walk(&d, 3, &|p| has_ext(p, "vst3") && p.strip_prefix(&d).is_ok_and(is_ua_name)))
            .unwrap_or_default(),
        drivers: find_drivers(r),
    }
}

fn find_console(r: &Path) -> Option<Console> {
    for pf in ["Program Files (x86)", "Program Files"] {
        let Some(dir) = ci_path(r, &[pf, "Universal Audio", "UAD Console"]) else {
            continue;
        };
        let Some(resources) = ci_path(&dir, &["Resources"]) else {
            continue;
        };
        if ci_path(&resources, &["appearance", "Appearance.json"]).is_none() {
            continue;
        }
        let version = ci_path(&dir, &["UAD Console.exe"]).and_then(|exe| crate::pe::file_version(&exe));
        return Some(Console { dir, resources, version });
    }
    None
}

fn find_drivers(r: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = ci_path(r, &["Windows", "System32", "drivers"]) {
        out.extend(list(&d).into_iter().filter(|p| has_ext(p, "sys") && starts_with_ci(p, "uad")));
    }
    if let Some(d) = ci_path(r, &["Windows", "System32", "DriverStore", "FileRepository"]) {
        out.extend(list(&d).into_iter().filter(|p| starts_with_ci(p, "uad")));
    }
    out.sort();
    out
}

fn list(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

/// Files (and bundle directories) matching `want`, at most `depth` levels down.
/// A matched directory, such as a `.vst3` bundle, isn't descended into.
fn walk(dir: &Path, depth: usize, want: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in list(dir) {
        if want(&p) {
            out.push(p);
        } else if depth > 0 && p.is_dir() {
            out.extend(walk(&p, depth - 1, want));
        }
    }
    out.sort();
    out
}

fn name_lower(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn has_ext(p: &Path, ext: &str) -> bool {
    name_lower(p).ends_with(&format!(".{ext}"))
}

fn starts_with_ci(p: &Path, prefix: &str) -> bool {
    name_lower(p).starts_with(prefix)
}

/// UA's bundles are named "UAD ...", or sit in a "Universal Audio"/"UADx" folder.
/// `p` is relative to the VST3 folder.
fn is_ua_name(p: &Path) -> bool {
    p.components().any(|c| {
        let s = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        s.starts_with("uad") || s.contains("universal audio")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_a_case_mangled_install() {
        let t = std::env::temp_dir().join(format!("oa-assets-detect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let res = t.join("program files (x86)/universal audio/UAD Console/Resources/appearance");
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join("Appearance.json"), "{}").unwrap();
        let pp = t.join("Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins");
        fs::create_dir_all(&pp).unwrap();
        fs::write(pp.join("UAD Pultec EQP-1A.dll"), b"MZ").unwrap();
        fs::write(pp.join("readme.txt"), b"").unwrap();
        let vst = t.join("Program Files/Common Files/VST3/UADx 1176 Compressor.vst3");
        fs::create_dir_all(&vst).unwrap();
        fs::create_dir_all(t.join("Program Files/Common Files/VST3/Other Vendor.vst3")).unwrap();
        let drv = t.join("Windows/System32/drivers");
        fs::create_dir_all(&drv).unwrap();
        fs::write(drv.join("UAD2Pcie.sys"), b"").unwrap();
        fs::write(drv.join("other.sys"), b"").unwrap();

        let got = inspect(&Root { path: t.clone(), source: Source::Given });
        assert!(got.console.is_some());
        assert_eq!(got.powered_plugins.len(), 1);
        assert_eq!(got.vst3.len(), 1);
        assert_eq!(got.drivers.len(), 1);
        assert!(got.found_anything());
        fs::remove_dir_all(&t).unwrap();
    }
}
