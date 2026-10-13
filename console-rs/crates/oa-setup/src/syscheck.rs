//! The read-only system check: what the installer will meet on this machine.
//! Nothing here changes the system; warnings carry fix hints, they never block.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::installlog::{Line, classify};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Info,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub title: &'static str,
    pub value: String,
    pub level: Level,
    pub hint: Option<String>,
    /// Commands that fix it, for the user to copy.
    pub commands: Vec<String>,
}

impl Check {
    fn new(title: &'static str, level: Level, value: impl Into<String>) -> Self {
        Check { title, value: value.into(), level, hint: None, commands: Vec::new() }
    }
    fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
    fn cmds(mut self, c: &[&str]) -> Self {
        self.commands.extend(c.iter().map(|s| s.to_string()));
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Apollo {
    /// Thunderbolt Apollo (PCI 1a00:0002): its address and bound driver.
    pub pci: Vec<(String, Option<String>)>,
    /// USB Apollos (vendor 2b5a): product names.
    pub usb: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
    pub distro: String,
    pub kernel: String,
    pub apollo: Apollo,
    /// `ua_apollo` is installed (`Some(true)` loaded, `Some(false)` on disk only).
    pub driver: Option<bool>,
}

impl Report {
    pub fn warnings(&self) -> usize {
        self.checks.iter().filter(|c| c.level == Level::Warn).count()
    }
}

/// Run every check. `repo` enables `scripts/apollo-hostcheck.sh` when it exists.
pub fn run(repo: Option<&Path>) -> Report {
    let sys = Path::new("/");
    let kernel = read_trim("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into());
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let distro = os_field(&os, "PRETTY_NAME").or_else(|| os_field(&os, "ID")).unwrap_or_else(|| "unknown".into());
    let pm = package_manager(&os);
    let hostcheck = repo.map(|r| r.join("scripts/apollo-hostcheck.sh")).filter(|p| p.is_file());

    let mut checks = Vec::new();
    checks.push(Check::new("Linux kernel", Level::Ok, &kernel));
    checks.push(match pm {
        Some(pm) => Check::new("Distribution", Level::Ok, format!("{distro} (packages: {pm})")),
        None => Check::new("Distribution", Level::Warn, &distro)
            .hint("The installer can't install packages here by itself. Install gcc, make, kernel headers, dkms, python3 and PipeWire with your package manager first."),
    });
    checks.push(kernel_headers(&kernel, pm));
    checks.push(build_tools(&kernel));
    checks.push(match in_path("dkms") {
        Some(_) => Check::new("DKMS", Level::Ok, "installed"),
        None => Check::new("DKMS", Level::Info, "not installed")
            .hint("The installer adds it. DKMS rebuilds the driver automatically after kernel updates."),
    });
    checks.push(secure_boot(sys, hostcheck.as_deref()));
    checks.push(thunderbolt(Path::new("/sys/bus/thunderbolt/devices"), hostcheck.as_deref()));
    let apollo = find_apollo(Path::new("/sys/bus"));
    checks.push(apollo_check(&apollo));
    let driver = driver_state();
    checks.push(match driver {
        Some(true) => Check::new(
            "open-apollo driver",
            Level::Ok,
            format!(
                "loaded{}",
                read_trim("/sys/module/ua_apollo/version").map(|v| format!(" ({v})")).unwrap_or_default()
            ),
        ),
        Some(false) => {
            Check::new("open-apollo driver", Level::Info, "installed, not loaded (loads when the Apollo is connected)")
        }
        None => Check::new("open-apollo driver", Level::Info, "not installed yet"),
    });
    checks.push(iommu());
    checks.push(match in_path("pkexec") {
        Some(_) => Check::new("Administrator prompt", Level::Ok, "pkexec (polkit)"),
        None => Check::new("Administrator prompt", Level::Warn, "pkexec not found")
            .hint("Installing the driver needs administrator rights. Install polkit, or run the installer in a terminal with sudo."),
    });
    checks.push(match in_path("udisksctl") {
        Some(_) => Check::new("Disk mounting", Level::Ok, "udisks2"),
        None => Check::new("Disk mounting", Level::Warn, "udisksctl not found")
            .hint("Needed to open a Windows VM's disk image read-only. Install udisks2, or mount it yourself and use Choose a folder."),
    });
    checks.push(match command_output("cargo", &["--version"]) {
        Some(v) => Check::new("Rust toolchain", Level::Ok, v),
        None => Check::new("Rust toolchain", Level::Info, "not installed")
            .hint("Needed to build the Console. Install Rust from rustup.rs before the Console step.")
            .cmds(&["curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"]),
    });
    Report { checks, distro, kernel, apollo, driver }
}

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn os_field(os_release: &str, key: &str) -> Option<String> {
    os_release.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.strip_prefix('=')?;
        Some(v.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|v| !v.is_empty())
    })
}

/// The package manager install.sh will use: by ID, then ID_LIKE (Omarchy is ID_LIKE=arch).
pub fn package_manager(os_release: &str) -> Option<&'static str> {
    let by_id = |id: &str| match id {
        "fedora" | "rhel" | "centos" => Some("dnf"),
        "ubuntu" | "debian" | "pop" | "linuxmint" => Some("apt"),
        "arch" | "manjaro" | "endeavouros" | "cachyos" => Some("pacman"),
        "nixos" => Some("nix"),
        s if s.starts_with("opensuse") || s == "sles" || s == "suse" => Some("zypper"),
        _ => None,
    };
    let id = os_field(os_release, "ID").unwrap_or_default();
    by_id(&id).or_else(|| os_field(os_release, "ID_LIKE")?.split_whitespace().find_map(by_id))
}

/// `cmd` on PATH.
pub fn in_path(cmd: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(cmd)).find(|p| p.is_file())
}

fn command_output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn kernel_headers(kernel: &str, pm: Option<&str>) -> Check {
    if Path::new("/lib/modules").join(kernel).join("build").exists() {
        return Check::new("Kernel headers", Level::Ok, "installed");
    }
    let pkg = match pm {
        Some("apt") => format!("linux-headers-{kernel}"),
        Some("pacman") => "linux-headers (matching your kernel package)".into(),
        Some("dnf") | Some("zypper") => "kernel-devel".into(),
        _ => "your kernel's headers package".into(),
    };
    Check::new("Kernel headers", Level::Warn, "missing")
        .hint(format!("The installer installs {pkg}. If your kernel was just updated, restart first so the running kernel matches the headers."))
}

fn build_tools(kernel: &str) -> Check {
    let clang = Path::new("/lib/modules").join(kernel).join("build/include/config/CC_IS_CLANG").exists();
    let cc = if clang { "clang" } else { "gcc" };
    let missing: Vec<&str> = [cc, "make", "python3"].into_iter().filter(|t| in_path(t).is_none()).collect();
    if missing.is_empty() {
        Check::new("Build tools", Level::Ok, format!("{cc}, make, python3"))
    } else {
        Check::new("Build tools", Level::Info, format!("missing: {}", missing.join(", ")))
            .hint("The installer installs them with your package manager.")
    }
}

/// Secure Boot from mokutil, else the SecureBoot EFI variable (4 attribute bytes + value).
fn secure_boot(root: &Path, hostcheck: Option<&Path>) -> Check {
    let state =
        command_output("mokutil", &["--sb-state"]).map(|s| s.to_ascii_lowercase().contains("enabled")).or_else(|| {
            let dir = root.join("sys/firmware/efi/efivars");
            let var = std::fs::read_dir(&dir)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().starts_with("SecureBoot-"))?;
            std::fs::read(var.path()).ok().and_then(|b| b.get(4).copied()).map(|v| v == 1)
        });
    let efi = root.join("sys/firmware/efi").is_dir();
    let mut c = match state {
        Some(false) => Check::new("Secure Boot", Level::Ok, "off"),
        None if !efi => Check::new("Secure Boot", Level::Ok, "not used (legacy BIOS boot)"),
        None => Check::new("Secure Boot", Level::Info, "unknown")
            .hint("If Secure Boot is on, the kernel refuses the self-built driver until you enroll a signing key (MOK) once."),
        Some(true) => Check::new("Secure Boot", Level::Warn, "on")
            .hint("The driver is built on this computer, so the kernel only loads it after you enroll its signing key once: create a DKMS key, import it, restart, and pick 'Enroll MOK' in the blue screen that appears.")
            .cmds(&["sudo dkms generate_mok", "sudo mokutil --import /var/lib/dkms/mok.pub"]),
    };
    if state == Some(true)
        && let Some(hc) = hostcheck
    {
        merge_hostcheck(&mut c, hc, "sb");
    }
    c
}

/// Thunderbolt peripherals waiting for authorization keep the Apollo off the PCI bus.
pub fn thunderbolt(devices: &Path, hostcheck: Option<&Path>) -> Check {
    let Ok(rd) = std::fs::read_dir(devices) else {
        return Check::new("Thunderbolt", Level::Info, "no Thunderbolt controller found")
            .hint("Thunderbolt Apollos need a Thunderbolt 3/4 or USB4 port. USB Apollos (Solo USB, Twin USB) don't.");
    };
    let mut levels = Vec::new();
    let mut waiting = Vec::new();
    let mut peripherals = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if name.starts_with("domain") {
            levels.extend(read_trim(p.join("security")));
            continue;
        }
        let Some(auth) = read_trim(p.join("authorized")) else { continue };
        // The host router (N-0) has `authorized` on some kernels; only count peripherals.
        if name.ends_with("-0") {
            continue;
        }
        peripherals += 1;
        if auth == "0" {
            let label = [read_trim(p.join("vendor_name")), read_trim(p.join("device_name"))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            waiting.push((if label.is_empty() { name } else { label }, read_trim(p.join("unique_id"))));
        }
    }
    let security = if levels.is_empty() { String::new() } else { format!(", security level {}", levels.join("/")) };
    if waiting.is_empty() {
        return Check::new(
            "Thunderbolt",
            Level::Ok,
            if peripherals == 0 {
                format!("controller present, nothing connected{security}")
            } else {
                format!("{peripherals} device(s) connected, all authorized{security}")
            },
        );
    }
    let names: Vec<_> = waiting.iter().map(|(n, _)| n.as_str()).collect();
    let mut c = Check::new("Thunderbolt", Level::Warn, format!("waiting for authorization: {}", names.join(", ")))
        .hint("A Thunderbolt device must be authorized before the Apollo appears. Allow it permanently with bolt, then unplug and replug the Apollo.");
    for (_, uuid) in &waiting {
        c.commands.push(match uuid {
            Some(u) => format!("boltctl enroll --policy auto {u}"),
            None => "boltctl list".into(),
        });
    }
    if let Some(hc) = hostcheck {
        merge_hostcheck(&mut c, hc, "tb");
    }
    c
}

/// Replace the hint and commands with `apollo-hostcheck.sh MODE`'s own guidance
/// (read-only; it prints exact commands and never runs them).
fn merge_hostcheck(c: &mut Check, script: &Path, mode: &str) {
    let Ok(out) = Command::new("bash").arg(script).arg(mode).output() else { return };
    let (hint, cmds) = parse_hostcheck(&String::from_utf8_lossy(&out.stdout));
    if !hint.is_empty() || !cmds.is_empty() {
        c.hint = Some(hint.join(" "));
        c.commands = cmds;
    }
}

/// apollo-hostcheck.sh output: `[WARN]`/`[INFO]` lines become the hint, indented lines commands.
pub fn parse_hostcheck(text: &str) -> (Vec<String>, Vec<String>) {
    let mut hint = Vec::new();
    let mut cmds = Vec::new();
    for l in text.lines() {
        match classify(l) {
            Line::Warn(m) | Line::Info(m) | Line::Fail(m) => hint.push(m),
            Line::Command(c) => cmds.push(c),
            _ => {}
        }
    }
    (hint, cmds)
}

/// UA devices on the PCI and USB buses (sysfs under `bus`).
pub fn find_apollo(bus: &Path) -> Apollo {
    let mut a = Apollo::default();
    if let Ok(rd) = std::fs::read_dir(bus.join("pci/devices")) {
        for e in rd.flatten() {
            let p = e.path();
            if read_trim(p.join("vendor")).as_deref() == Some("0x1a00")
                && read_trim(p.join("device")).as_deref() == Some("0x0002")
            {
                let driver = std::fs::read_link(p.join("driver"))
                    .ok()
                    .and_then(|l| l.file_name().map(|n| n.to_string_lossy().into_owned()));
                a.pci.push((e.file_name().to_string_lossy().into_owned(), driver));
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(bus.join("usb/devices")) {
        for e in rd.flatten() {
            let p = e.path();
            if read_trim(p.join("idVendor")).as_deref() != Some("2b5a") {
                continue;
            }
            let product = read_trim(p.join("idProduct")).unwrap_or_default();
            a.usb.push(
                usb_name(&product)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Universal Audio USB device 2b5a:{product}")),
            );
        }
    }
    a.pci.sort();
    a.usb.sort();
    a
}

/// From configs/udev/99-apollo-usb.rules: firmware-loader and live product ids.
fn usb_name(product: &str) -> Option<&'static str> {
    Some(match product {
        "000c" | "000d" => "Apollo Solo USB",
        "0001" | "0002" => "Twin USB",
        "000e" | "000f" => "Twin X USB",
        _ => return None,
    })
}

fn apollo_check(a: &Apollo) -> Check {
    if let Some((addr, driver)) = a.pci.first() {
        let drv = match driver.as_deref() {
            Some("ua_apollo") => "driver loaded".to_string(),
            Some(other) => format!("claimed by {other}"),
            None => "no driver yet".to_string(),
        };
        let c = Check::new("Apollo", Level::Ok, format!("Thunderbolt Apollo at {addr} ({drv})"));
        return match driver.as_deref() {
            Some(d) if d != "ua_apollo" => c.hint(format!(
                "Another driver ({d}) owns the Apollo; the installer can't verify it until that driver lets go."
            )),
            _ => c,
        };
    }
    if let Some(name) = a.usb.first() {
        return Check::new("Apollo", Level::Info, format!("{name} connected"))
            .hint("USB Apollos use the USB installer (scripts/install-usb.sh); this wizard's driver step is for Thunderbolt models.");
    }
    Check::new("Apollo", Level::Warn, "not connected")
        .hint("You can set everything up now. To have the installer verify the driver too, power on the Apollo and connect it before the Driver step (Thunderbolt takes ~20 s to appear).")
}

/// `Some(true)` loaded, `Some(false)` installed but not loaded, `None` not installed.
fn driver_state() -> Option<bool> {
    if Path::new("/sys/module/ua_apollo").exists() {
        return Some(true);
    }
    command_output("modinfo", &["-n", "ua_apollo"]).map(|_| false)
}

/// install.sh adds iommu=pt when an IOMMU is active without it (Intel VT-d blocks the Apollo's registers).
fn iommu() -> Check {
    let units: Vec<String> = std::fs::read_dir("/sys/class/iommu")
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if units.is_empty() {
        return Check::new("IOMMU", Level::Ok, "off");
    }
    if cmdline.split_whitespace().any(|a| a == "iommu=pt") {
        return Check::new("IOMMU", Level::Ok, "on, passthrough mode (iommu=pt)");
    }
    let intel = units.iter().any(|u| u.starts_with("dmar"));
    let c = Check::new(
        "IOMMU",
        if intel { Level::Warn } else { Level::Info },
        if intel { "Intel VT-d on, without iommu=pt" } else { "on" },
    );
    if intel {
        c.hint("With VT-d on, the Apollo's registers read back empty unless the kernel uses passthrough mode. The installer adds iommu=pt to GRUB for you (other boot loaders: add it yourself); a restart is needed after.")
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("oa-setup-sys-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(&t).unwrap();
        t
    }

    #[test]
    fn package_manager_follows_id_like() {
        assert_eq!(package_manager("ID=omarchy\nID_LIKE=arch\n"), Some("pacman"));
        assert_eq!(package_manager("ID=\"pop\"\n"), Some("apt"));
        assert_eq!(package_manager("ID=opensuse-tumbleweed\n"), Some("zypper"));
        assert_eq!(package_manager("ID=weird\nID_LIKE=\"rhel fedora\"\n"), Some("dnf"));
        assert_eq!(package_manager("ID=gentoo\n"), None);
        assert_eq!(os_field("PRETTY_NAME=\"Arch Linux\"\nID=arch", "PRETTY_NAME").as_deref(), Some("Arch Linux"));
        assert_eq!(os_field("ID_LIKE=arch\nID=x", "ID").as_deref(), Some("x"));
    }

    #[test]
    fn thunderbolt_flags_unauthorized_peripherals() {
        let t = tmp("tb");
        fs::create_dir_all(t.join("domain0")).unwrap();
        fs::write(t.join("domain0/security"), "user\n").unwrap();
        fs::create_dir_all(t.join("0-0")).unwrap();
        fs::write(t.join("0-0/authorized"), "1\n").unwrap();
        assert_eq!(thunderbolt(&t, None).level, Level::Ok);
        let dev = t.join("0-1");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join("authorized"), "0\n").unwrap();
        fs::write(dev.join("vendor_name"), "Universal Audio\n").unwrap();
        fs::write(dev.join("device_name"), "Apollo x4\n").unwrap();
        fs::write(dev.join("unique_id"), "abcd-1234\n").unwrap();
        let c = thunderbolt(&t, None);
        assert_eq!(c.level, Level::Warn);
        assert!(c.value.contains("Universal Audio Apollo x4"), "{}", c.value);
        assert_eq!(c.commands, ["boltctl enroll --policy auto abcd-1234"]);
        assert_eq!(thunderbolt(&t.join("missing"), None).level, Level::Info);
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn finds_thunderbolt_and_usb_apollos() {
        let t = tmp("bus");
        let pci = t.join("pci/devices/0000:05:00.0");
        fs::create_dir_all(&pci).unwrap();
        fs::write(pci.join("vendor"), "0x1a00\n").unwrap();
        fs::write(pci.join("device"), "0x0002\n").unwrap();
        let uad2 = t.join("pci/devices/0000:06:00.0");
        fs::create_dir_all(&uad2).unwrap();
        fs::write(uad2.join("vendor"), "0x1a00\n").unwrap();
        fs::write(uad2.join("device"), "0x0001\n").unwrap();
        let usb = t.join("usb/devices/3-2");
        fs::create_dir_all(&usb).unwrap();
        fs::write(usb.join("idVendor"), "2b5a\n").unwrap();
        fs::write(usb.join("idProduct"), "000f\n").unwrap();
        let a = find_apollo(&t);
        assert_eq!(a.pci, [("0000:05:00.0".to_string(), None)]);
        assert_eq!(a.usb, ["Twin X USB"]);
        assert_eq!(apollo_check(&a).level, Level::Ok);
        assert_eq!(apollo_check(&Apollo::default()).level, Level::Warn);
        fs::remove_dir_all(&t).unwrap();
    }

    #[test]
    fn hostcheck_output_becomes_hint_and_commands() {
        let text = "[WARN]  Secure Boot is enabled and the ua_apollo module is not signed.\n\
                    [INFO]  Enroll the signing key once:\n        sudo mokutil --import /var/lib/dkms/mok.pub\n";
        let (hint, cmds) = parse_hostcheck(text);
        assert_eq!(hint.len(), 2);
        assert_eq!(cmds, ["sudo mokutil --import /var/lib/dkms/mok.pub"]);
    }
}
