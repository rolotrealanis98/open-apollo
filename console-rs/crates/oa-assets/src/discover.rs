//! Finding places a UAD install could live: mounted Windows volumes,
//! Windows partitions that aren't mounted yet, and Wine prefixes.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Passed with `--from`.
    Given,
    /// A mounted NTFS volume (dual boot or an attached disk).
    Volume { device: String, fstype: String },
    /// A Wine prefix's `drive_c`.
    WinePrefix,
}

/// A directory that plays the role of `C:\`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub source: Source,
}

/// A Windows partition that isn't mounted, so it can't be scanned yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmounted {
    pub device: String,
    pub fstype: String,
    pub label: Option<String>,
    pub size: Option<String>,
}

impl Unmounted {
    /// BitLocker volumes need unlocking in Windows (or with the recovery key) first.
    pub fn is_bitlocker(&self) -> bool {
        self.fstype.eq_ignore_ascii_case("bitlocker")
    }
}

/// A `--from` path: accepts `C:\`'s mount point, a Wine prefix, or its `drive_c`.
pub fn given(path: &Path) -> Root {
    let drive_c = path.join("drive_c");
    let path = if drive_c.is_dir() { drive_c } else { path.to_path_buf() };
    Root { path, source: Source::Given }
}

/// Mounted NTFS volumes that look like a Windows system drive or data drive.
pub fn mounted_volumes() -> Vec<Root> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    parse_mountinfo(&text).into_iter().filter(|r| looks_like_windows_root(&r.path)).collect()
}

pub(crate) fn parse_mountinfo(text: &str) -> Vec<Root> {
    let mut out = Vec::new();
    for line in text.lines() {
        // id parent maj:min root mountpoint opts [optional...] - fstype source superopts
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mountpoint) = pre.split(' ').nth(4) else {
            continue;
        };
        let mut post = post.split(' ');
        let (Some(fstype), Some(device)) = (post.next(), post.next()) else {
            continue;
        };
        if !matches!(fstype, "ntfs" | "ntfs3" | "fuseblk") {
            continue;
        }
        out.push(Root {
            path: PathBuf::from(unescape_mount(mountpoint)),
            source: Source::Volume { device: device.to_string(), fstype: fstype.to_string() },
        });
    }
    out
}

/// mountinfo escapes space, tab, newline and backslash as octal.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn looks_like_windows_root(p: &Path) -> bool {
    crate::ci_path(p, &["Windows"]).is_some() || crate::ci_path(p, &["Program Files"]).is_some()
}

/// Wine prefixes: `$WINEPREFIX`, `~/.wine`, and `~/wine*` folders with a `drive_c`.
pub fn wine_prefixes() -> Vec<Root> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("WINEPREFIX").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(p));
    }
    dirs.push(home.join(".wine"));
    if let Ok(rd) = std::fs::read_dir(&home) {
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("wine"))
            .map(|e| e.path())
            .collect();
        found.sort();
        dirs.extend(found);
    }
    let mut out: Vec<Root> = Vec::new();
    for d in dirs {
        let c = d.join("drive_c");
        if c.is_dir() && !out.iter().any(|r| r.path == c) {
            out.push(Root { path: c, source: Source::WinePrefix });
        }
    }
    out
}

/// NTFS and BitLocker partitions with no mount point, from `lsblk`.
pub fn unmounted_partitions() -> Vec<Unmounted> {
    let Ok(out) = Command::new("lsblk").args(["-J", "-o", "PATH,FSTYPE,PARTTYPE,LABEL,SIZE,MOUNTPOINTS"]).output()
    else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    if let Some(devs) = json.get("blockdevices").and_then(|v| v.as_array()) {
        for d in devs {
            collect_unmounted(d, &mut found);
        }
    }
    found
}

/// GPT types of Windows' own service partitions, which never hold an install:
/// recovery environment and Microsoft reserved.
const SERVICE_PARTTYPES: [&str; 2] = ["de94bba4-06d1-4d40-a16a-bfd50179d6ac", "e3c9e316-0b5c-4db8-817d-f92df00215ae"];

fn collect_unmounted(dev: &serde_json::Value, out: &mut Vec<Unmounted>) {
    let s = |k: &str| dev.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let fstype = s("fstype").unwrap_or_default();
    let mounted = dev.get("mountpoints").and_then(|v| v.as_array()).is_some_and(|m| m.iter().any(|x| x.is_string()));
    let service_partition = s("parttype").is_some_and(|t| SERVICE_PARTTYPES.contains(&t.to_ascii_lowercase().as_str()));
    if !mounted
        && !service_partition
        && (fstype == "ntfs" || fstype.eq_ignore_ascii_case("bitlocker"))
        && let Some(device) = s("path")
    {
        out.push(Unmounted { device, fstype, label: s("label"), size: s("size") });
    }
    if let Some(children) = dev.get("children").and_then(|v| v.as_array()) {
        for c in children {
            collect_unmounted(c, out);
        }
    }
}

/// Mount a partition read-only through udisks (asks for authorization when needed).
/// Returns the mount point.
pub fn mount_readonly(device: &str) -> Result<PathBuf, String> {
    let out = Command::new("udisksctl")
        .args(["mount", "--block-device", device, "--options", "ro", "--no-user-interaction"])
        .output()
        .map_err(|e| format!("udisksctl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // "Mounted /dev/sda3 at /run/media/user/Windows"
    text.split(" at ")
        .nth(1)
        .map(|p| PathBuf::from(p.trim().trim_end_matches('.')))
        .ok_or_else(|| format!("unexpected udisksctl output: {}", text.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_picks_ntfs_and_unescapes() {
        let text = "\
36 25 8:3 / /run/media/rolo/OS\\040Disk rw,nosuid - ntfs3 /dev/sda3 rw,uid=1000
37 25 8:4 / /home rw - btrfs /dev/nvme0n1p2 rw
38 25 8:5 / /mnt/data rw - fuseblk /dev/sdb1 rw,user_id=0";
        let roots = parse_mountinfo(text);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].path, PathBuf::from("/run/media/rolo/OS Disk"));
        assert_eq!(roots[0].source, Source::Volume { device: "/dev/sda3".into(), fstype: "ntfs3".into() });
        assert_eq!(roots[1].path, PathBuf::from("/mnt/data"));
    }

    #[test]
    fn lsblk_tree_finds_unmounted_ntfs_and_bitlocker() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"blockdevices":[{"path":"/dev/nvme1n1","fstype":null,"mountpoints":[null],"children":[
                {"path":"/dev/nvme1n1p1","fstype":"vfat","mountpoints":[null]},
                {"path":"/dev/nvme1n1p3","fstype":"ntfs","label":"Windows","size":"931G","mountpoints":[null]},
                {"path":"/dev/nvme1n1p4","fstype":"BitLocker","mountpoints":[null]},
                {"path":"/dev/nvme1n1p5","fstype":"ntfs","mountpoints":["/mnt/d"]},
                {"path":"/dev/nvme1n1p6","fstype":"ntfs","parttype":"de94bba4-06d1-4d40-a16a-bfd50179d6ac","mountpoints":[null]}]}]}"#,
        )
        .unwrap();
        let mut out = Vec::new();
        collect_unmounted(&json["blockdevices"][0], &mut out);
        let devs: Vec<_> = out.iter().map(|u| u.device.as_str()).collect();
        assert_eq!(devs, ["/dev/nvme1n1p3", "/dev/nvme1n1p4"]);
        assert!(out[1].is_bitlocker());
        assert_eq!(out[0].label.as_deref(), Some("Windows"));
    }
}
