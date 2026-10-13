//! Bring-your-own-install asset importer.
//!
//! open-apollo ships no UA art. `oa-assets` finds the user's own UAD install
//! (a dual-boot Windows partition, a mounted Windows volume, or a Wine prefix),
//! reports what it contains, and copies the pieces console-rs needs into
//! `~/.local/share/open-apollo/`. Everything is read-only toward the source.

pub mod detect;
pub mod discover;
pub mod import;
pub mod pe;

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
