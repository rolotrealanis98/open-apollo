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

/// Mount a partition read-only through udisks. udisks asks for authorization
/// (a password dialog) when the policy needs it, e.g. for internal disks.
/// Returns the mount point.
pub fn mount_readonly(device: &str) -> Result<PathBuf, String> {
    let text = udisksctl(&["mount", "--block-device", device, "--options", "ro"])?;
    // "Mounted /dev/sda3 at /run/media/user/Windows"
    text.split(" at ")
        .nth(1)
        .map(|p| PathBuf::from(p.trim().trim_end_matches('.')))
        .ok_or_else(|| format!("unexpected udisksctl output: {}", text.trim()))
}

fn udisksctl(args: &[&str]) -> Result<String, String> {
    let out = Command::new("udisksctl").args(args).output().map_err(|e| format!("udisksctl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A VM disk image attached read-only as a loop device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attached {
    pub loop_device: String,
    /// Mount points of its Windows partitions.
    pub roots: Vec<Root>,
}

/// Disk image formats udisks can't attach directly.
fn non_raw_format(head: &[u8]) -> Option<&'static str> {
    match head {
        [b'Q', b'F', b'I', 0xfb, ..] => Some("qcow2"),
        [b'v', b'h', b'd', b'x', b'f', b'i', b'l', b'e', ..] => Some("vhdx"),
        [b'K', b'D', b'M', b'V', ..] => Some("vmdk"),
        [b'c', b'o', b'n', b'e', b'c', b't', b'i', b'x', ..] => Some("vhd"),
        _ if head.len() >= 0x44 && head[0x40..0x44] == [0x7f, 0x10, 0xda, 0xbe] => Some("vdi"),
        _ => None,
    }
}

/// Attach a raw VM disk image (e.g. the Omarchy Windows VM's `~/.windows/data.img`)
/// read-only and mount its Windows partitions read-only. Shut the VM down first:
/// a running guest keeps changing the disk underneath.
pub fn attach_image(image: &Path) -> Result<Attached, String> {
    let mut head = [0u8; 0x48];
    let n = std::io::Read::read(
        &mut std::fs::File::open(image).map_err(|e| format!("{}: {e}", image.display()))?,
        &mut head,
    )
    .map_err(|e| e.to_string())?;
    if let Some(fmt) = non_raw_format(&head[..n]) {
        return Err(format!(
            "{} is a {fmt} image; convert it to raw first: qemu-img convert -O raw '{}' windows.img",
            image.display(),
            image.display()
        ));
    }
    let loop_device = match existing_loop(image) {
        Some(dev) => dev,
        None => {
            let file = image.to_string_lossy();
            let text = udisksctl(&["loop-setup", "--read-only", "--file", &file])?;
            // "Mapped file /path/data.img as /dev/loop0."
            text.split(" as ")
                .nth(1)
                .map(|d| d.trim().trim_end_matches('.').to_string())
                .ok_or_else(|| format!("unexpected udisksctl output: {}", text.trim()))?
        }
    };
    // Partitions appear, and get their filesystem type from udev, a moment after the loop device.
    let _ = Command::new("udevadm").args(["settle", "--timeout=10"]).status();
    let mut parts = Vec::new();
    for _ in 0..100 {
        parts = loop_partitions(&loop_device);
        if !parts.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let mut roots = Vec::new();
    for (device, mountpoint) in parts {
        let path = match mountpoint {
            Some(p) => p,
            // udisks may auto-mount the partition while we ask; take its mount point then.
            None => mount_readonly(&device).or_else(|e| {
                loop_partitions(&loop_device).into_iter().find(|(d, _)| *d == device).and_then(|(_, m)| m).ok_or(e)
            })?,
        };
        if looks_like_windows_root(&path) {
            roots.push(Root { path, source: Source::Volume { device, fstype: "ntfs".into() } });
        }
    }
    Ok(Attached { loop_device, roots })
}

/// Unmount an attached image's partitions and release the loop device.
pub fn detach(loop_device: &str) -> Result<(), String> {
    for (device, mountpoint) in loop_partitions(loop_device) {
        if mountpoint.is_some() {
            udisksctl(&["unmount", "--block-device", &device])?;
        }
    }
    udisksctl(&["loop-delete", "--block-device", loop_device])?;
    // The kernel drops the backing file asynchronously; wait so a following attach starts clean.
    let backing = format!("/sys/block/{}/loop/backing_file", loop_device.trim_start_matches("/dev/"));
    for _ in 0..50 {
        if !Path::new(&backing).exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("{loop_device} is still in use"))
}

/// The loop device already backed by `image`, if any.
pub fn existing_loop(image: &Path) -> Option<String> {
    let out = Command::new("losetup").args(["-j"]).arg(image).args(["-O", "NAME", "-n"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

/// NTFS partitions of a loop device that could hold Windows: (device, mount point).
fn loop_partitions(loop_device: &str) -> Vec<(String, Option<PathBuf>)> {
    let Ok(out) = Command::new("lsblk").args(["-J", "-o", "PATH,FSTYPE,PARTTYPE,MOUNTPOINTS", loop_device]).output()
    else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    partitions_of(&json, loop_device)
}

/// Partitions in `lsblk -J` output for `device`. lsblk nests them under the disk
/// for real disks but lists a loop device's partitions next to it, so take both.
fn partitions_of(json: &serde_json::Value, device: &str) -> Vec<(String, Option<PathBuf>)> {
    fn flatten<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
        for d in v.as_array().into_iter().flatten() {
            out.push(d);
            flatten(&d["children"], out);
        }
    }
    let mut all = Vec::new();
    flatten(&json["blockdevices"], &mut all);
    all.into_iter()
        .filter(|c| c["path"] != device)
        .filter(|c| c["fstype"] == "ntfs")
        .filter(|c| {
            !c["parttype"].as_str().is_some_and(|t| SERVICE_PARTTYPES.contains(&t.to_ascii_lowercase().as_str()))
        })
        .filter_map(|c| {
            let device = c["path"].as_str()?.to_string();
            let mountpoint =
                c["mountpoints"].as_array().and_then(|m| m.iter().find_map(|x| x.as_str())).map(PathBuf::from);
            Some((device, mountpoint))
        })
        .collect()
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
    fn loop_partitions_flat_or_nested() {
        let flat: serde_json::Value = serde_json::from_str(
            r#"{"blockdevices":[{"path":"/dev/loop0","fstype":null,"mountpoints":[]},
                {"path":"/dev/loop0p1","fstype":"vfat","mountpoints":[]},
                {"path":"/dev/loop0p3","fstype":"ntfs","parttype":"ebd0a0a2-b9e5-4433-87c0-68b6b72699c7","mountpoints":["/run/media/u/Windows"]},
                {"path":"/dev/loop0p4","fstype":"ntfs","parttype":"de94bba4-06d1-4d40-a16a-bfd50179d6ac","mountpoints":[]}]}"#,
        )
        .unwrap();
        let want = vec![("/dev/loop0p3".to_string(), Some(PathBuf::from("/run/media/u/Windows")))];
        assert_eq!(partitions_of(&flat, "/dev/loop0"), want);
        let nested: serde_json::Value = serde_json::from_str(
            r#"{"blockdevices":[{"path":"/dev/loop0","fstype":null,"mountpoints":[],"children":[
                {"path":"/dev/loop0p3","fstype":"ntfs","mountpoints":["/run/media/u/Windows"]}]}]}"#,
        )
        .unwrap();
        assert_eq!(partitions_of(&nested, "/dev/loop0"), want);
    }

    #[test]
    fn recognizes_image_formats_udisks_cannot_attach() {
        assert_eq!(non_raw_format(b"QFI\xfb\0\0\0\x03"), Some("qcow2"));
        assert_eq!(non_raw_format(b"vhdxfile\0"), Some("vhdx"));
        let mut vdi = vec![0u8; 0x48];
        vdi[0x40..0x44].copy_from_slice(&[0x7f, 0x10, 0xda, 0xbe]);
        assert_eq!(non_raw_format(&vdi), Some("vdi"));
        // A raw GPT disk starts with a protective MBR.
        let mut raw = vec![0u8; 0x48];
        raw[0] = 0xeb;
        assert_eq!(non_raw_format(&raw), None);
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
