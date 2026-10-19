//! Bring-your-own-install asset importer.
//!
//! open-apollo ships no UA art. `oa-assets` finds the user's own UAD install
//! (a dual-boot Windows partition, a mounted Windows volume, or a Wine prefix),
//! reports what it contains, and copies the pieces console-rs needs into
//! `~/.local/share/open-apollo/`. `oa-assets firmware` likewise extracts the
//! DSP modules and USB boot images the Apollo needs into
//! `/lib/firmware/universal-audio/`, and `oa-assets dsp` collects every
//! Powered plug-in's DSP modules under its `plugins/` folder ([`dsp`]).
//! `oa-assets plugins` regenerates plug-in panel art from the install's
//! Powered plug-in DLLs ([`plugins`]).
//! Everything is read-only toward the source.

pub mod catalogue;
pub mod detect;
pub mod discover;
pub mod dsp;
pub mod firmware;
pub mod import;
pub mod pe;
pub mod plugins;

use std::path::{Path, PathBuf};

/// Resolve `parts` under `root` ignoring ASCII case, the way Windows does.
/// NTFS mounted on Linux is case-sensitive, so `Program Files (x86)` and
/// `program files (x86)` would otherwise be different paths.
pub fn ci_path(root: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut cur = root.to_path_buf();
    for part in parts {
        let exact = cur.join(part);
        if exact.exists() {
            cur = exact;
            continue;
        }
        let entry = std::fs::read_dir(&cur)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        cur = entry.path();
    }
    Some(cur)
}

/// Where imported assets live, e.g. `~/.local/share/open-apollo`.
pub fn data_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(x).join("open-apollo");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/share/open-apollo")
}
