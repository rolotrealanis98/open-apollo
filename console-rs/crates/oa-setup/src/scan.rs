//! Finding UAD installs for the "Find your UAD install" step, on top of
//! oa-assets' discovery: where each one lives (partition, VM disk image, Wine
//! prefix, folder), Windows VM disk images that could be opened, and which
//! install to suggest.

use std::path::{Path, PathBuf};

use oa_assets::detect::{self, Install};
use oa_assets::discover::{self, Root, Source, Unmounted};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A Windows partition (dual boot or another disk).
    Partition {
        device: String,
    },
    /// A partition of a VM disk image attached as a loop device.
    VmImage {
        image: PathBuf,
        device: String,
    },
    WinePrefix,
    /// A folder the user picked.
    Folder,
}

impl Origin {
    pub fn label(&self) -> &'static str {
        match self {
            Origin::Partition { .. } => "Windows partition",
            Origin::VmImage { .. } => "Windows VM disk image",
            Origin::WinePrefix => "Wine prefix",
            Origin::Folder => "Folder",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Found {
    pub install: Install,
    pub origin: Origin,
}

impl Found {
    pub fn path(&self) -> &Path {
        &self.install.root.path
    }

    pub fn has_console(&self) -> bool {
        self.install.console.is_some()
    }
}

/// A VM disk image that isn't attached yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmImage {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub found: Vec<Found>,
    pub unmounted: Vec<Unmounted>,
    pub images: Vec<VmImage>,
}

impl Scan {
    /// The install to suggest: a UAD Console first, then the most plug-ins.
    pub fn best(&self) -> Option<usize> {
        best_of(&self.found)
    }

    /// Partitions large enough to hold Windows; smaller NTFS ones are usually recovery or data.
    pub fn likely_windows(&self) -> impl Iterator<Item = &Unmounted> {
        self.unmounted.iter().filter(|u| u.size.as_deref().and_then(parse_size).is_none_or(|b| b >= 16 << 30))
    }

    pub fn other_partitions(&self) -> impl Iterator<Item = &Unmounted> {
        self.unmounted.iter().filter(|u| u.size.as_deref().and_then(parse_size).is_some_and(|b| b < 16 << 30))
    }
}

pub fn best_of(found: &[Found]) -> Option<usize> {
    found
        .iter()
        .enumerate()
        .filter(|(_, f)| f.install.found_anything())
        .max_by_key(|(i, f)| {
            (f.has_console(), f.install.powered_plugins.len(), f.install.drivers.len(), usize::MAX - i)
        })
        .map(|(i, _)| i)
}

/// Look everywhere: mounted Windows volumes, Wine prefixes, `extra` folders the
/// user picked, unmounted Windows partitions and VM disk images.
pub fn scan(extra: &[Root]) -> Scan {
    let mut roots = discover::mounted_volumes();
    roots.extend(discover::wine_prefixes());
    roots.extend(extra.iter().cloned());
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut found = Vec::new();
    for root in roots {
        let canon = root.path.canonicalize().unwrap_or_else(|_| root.path.clone());
        if seen.contains(&canon) {
            continue;
        }
        seen.push(canon);
        let origin = origin_of(&root);
        found.push(Found { install: detect::inspect(&root), origin });
    }
    Scan { found, unmounted: discover::unmounted_partitions(), images: vm_images() }
}

pub fn origin_of(root: &Root) -> Origin {
    match &root.source {
        Source::Volume { device, .. } => match loop_backing(device) {
            Some(image) => Origin::VmImage { image, device: device.clone() },
            None => Origin::Partition { device: device.clone() },
        },
        Source::WinePrefix => Origin::WinePrefix,
        Source::Given => Origin::Folder,
    }
}

/// The loop device behind a partition device: `/dev/loop0p3` -> `loop0`.
pub fn loop_name(device: &str) -> Option<&str> {
    let name = device.strip_prefix("/dev/")?;
    let digits = name.strip_prefix("loop")?;
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    (end > 0).then(|| &name[..4 + end])
}

/// The image file backing a loop partition, e.g. `~/.windows/data.img`.
pub fn loop_backing(device: &str) -> Option<PathBuf> {
    let name = loop_name(device)?;
    let text = std::fs::read_to_string(format!("/sys/block/{name}/loop/backing_file")).ok()?;
    Some(PathBuf::from(text.trim())).filter(|p| !p.as_os_str().is_empty())
}

/// Where VM tools keep Windows disk images: Omarchy's Windows VM, GNOME Boxes,
/// libvirt (session and system). Images already attached are left out.
pub fn vm_images() -> Vec<VmImage> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut paths = vec![home.join(".windows/data.img")];
    for dir in [
        home.join(".local/share/gnome-boxes/images"),
        home.join(".local/share/libvirt/images"),
        PathBuf::from("/var/lib/libvirt/images"),
    ] {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            paths.extend(rd.flatten().map(|e| e.path()));
        }
    }
    paths
        .into_iter()
        .filter_map(|p| {
            let md = std::fs::metadata(&p).ok().filter(|m| m.is_file() && m.len() >= 4 << 30)?;
            Some(VmImage { path: p, size: md.len() })
        })
        .filter(|i| discover::existing_loop(&i.path).is_none())
        .collect()
}

/// lsblk's human sizes ("1.8T", "735M", "512") in bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let n: f64 = num.trim().parse().ok()?;
    let mult: u64 = match unit.trim_end_matches(['i', 'B']).to_ascii_uppercase().as_str() {
        "" => 1,
        "K" => 1 << 10,
        "M" => 1 << 20,
        "G" => 1 << 30,
        "T" => 1 << 40,
        "P" => 1 << 50,
        _ => return None,
    };
    Some((n * mult as f64) as u64)
}

/// `1.8 TB`-style size for display.
pub fn human_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < units.len() {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", units[u])
    } else {
        format!("{v:.0} {}", units[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oa_assets::detect::Console;

    fn install(console: bool, plugins: usize) -> Found {
        let root = Root { path: PathBuf::from("/x"), source: Source::Given };
        Found {
            install: Install {
                root,
                console: console.then(|| Console { dir: "/c".into(), resources: "/c/Resources".into(), version: None }),
                mixer_engine: None,
                powered_plugins: vec![PathBuf::from("/p.dll"); plugins],
                vst3: Vec::new(),
                drivers: Vec::new(),
                uad_version: None,
                sdk_dll: None,
                firmware: Vec::new(),
                plugin_icons: None,
            },
            origin: Origin::Folder,
        }
    }

    #[test]
    fn suggests_console_installs_first_then_plugins() {
        assert_eq!(best_of(&[]), None);
        assert_eq!(best_of(&[install(false, 0)]), None, "an empty Windows volume isn't suggested");
        assert_eq!(best_of(&[install(false, 200), install(true, 3)]), Some(1));
        assert_eq!(best_of(&[install(true, 3), install(true, 200)]), Some(1));
        assert_eq!(best_of(&[install(true, 5), install(true, 5)]), Some(0), "ties go to the first");
        assert_eq!(best_of(&[install(false, 2)]), Some(0), "plug-ins alone still feed the firmware step");
    }

    #[test]
    fn loop_partition_names() {
        assert_eq!(loop_name("/dev/loop0p3"), Some("loop0"));
        assert_eq!(loop_name("/dev/loop12p1"), Some("loop12"));
        assert_eq!(loop_name("/dev/loop7"), Some("loop7"));
        assert_eq!(loop_name("/dev/nvme0n1p3"), None);
        assert_eq!(loop_name("/dev/loopx"), None);
    }

    #[test]
    fn lsblk_sizes() {
        assert_eq!(parse_size("1.8T"), Some((1.8 * (1u64 << 40) as f64) as u64));
        assert_eq!(parse_size("735M"), Some(735 << 20));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("2GiB"), Some(2 << 30));
        assert_eq!(parse_size("huge"), None);
        assert_eq!(human_size(64_000_000_000), "64 GB");
        assert_eq!(human_size(1_800_000_000_000), "1.8 TB");
        assert_eq!(human_size(12), "12 B");
    }

    #[test]
    fn splits_windows_sized_partitions_from_small_ones() {
        let u = |size: &str| Unmounted {
            device: format!("/dev/{size}"),
            fstype: "ntfs".into(),
            label: None,
            size: Some(size.into()),
        };
        let s = Scan { unmounted: vec![u("1.8T"), u("735M"), u("64G")], ..Default::default() };
        assert_eq!(s.likely_windows().count(), 2);
        assert_eq!(s.other_partitions().map(|u| u.device.as_str()).collect::<Vec<_>>(), ["/dev/735M"]);
    }
}
