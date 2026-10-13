//! Fake installer output for `--demo`: the same line formats the real scripts
//! print, replayed with delays, so the flow can be shown without pkexec,
//! builds, or writes outside the data folder.

use std::path::Path;
use std::time::Duration;

const GREEN: &str = "\x1b[0;32m";
const CYAN: &str = "\x1b[0;36m";
const BOLD: &str = "\x1b[1m";
const NC: &str = "\x1b[0m";

struct Script {
    lines: Vec<(Duration, String)>,
    color: bool,
}

impl Script {
    fn new(color: bool) -> Self {
        Script { lines: Vec::new(), color }
    }
    fn raw(&mut self, ms: u64, s: impl Into<String>) {
        self.lines.push((Duration::from_millis(ms), s.into()));
    }
    fn tag(&mut self, ms: u64, color: &str, tag: &str, m: &str) {
        let s = if self.color { format!("{color}{tag}{NC}  {m}") } else { format!("{tag}  {m}") };
        self.raw(ms, s);
    }
    fn header(&mut self, ms: u64, h: &str) {
        self.raw(ms, "");
        let s = if self.color { format!("{BOLD}── {h} ──{NC}") } else { format!("── {h} ──") };
        self.raw(0, s);
    }
    fn ok(&mut self, ms: u64, m: &str) {
        self.tag(ms, GREEN, "[ OK ]", m);
    }
    fn info(&mut self, ms: u64, m: &str) {
        self.tag(ms, CYAN, "[INFO]", m);
    }
    fn summary(&mut self, ms: u64, color: &str, tag: &str, key: &str) {
        let pad = " ".repeat(8 - tag.len());
        self.raw(ms, if self.color { format!("  {color}{tag}{NC}{pad}{key}") } else { format!("  {tag}{pad}{key}") });
    }
}

/// scripts/install.sh --yes on a machine with an Apollo connected.
pub fn driver(distro: &str, kernel: &str) -> Vec<(Duration, String)> {
    let mut s = Script::new(true);
    s.raw(200, "");
    s.raw(0, format!("{BOLD}Open Apollo — Installer{NC}"));
    s.raw(0, "=======================");
    s.header(300, "Detecting System");
    s.ok(400, &format!("Distro: {distro}"));
    s.info(100, &format!("Kernel: {kernel} | Arch: x86_64 | RAM: 32768MB"));
    s.info(100, "Package manager: pacman");
    s.ok(200, "Apollo detected: 05:00.0 Multimedia audio controller: Universal Audio Inc. Apollo (demo)");
    s.header(300, "Dependencies");
    for dep in ["Kernel headers", "gcc", "make", "python3 (3.13.1)", "dkms", "pipewire (1.4.2)", "wireplumber (0.5.8)"]
    {
        s.ok(120, dep);
    }
    s.ok(150, "All required dependencies present");
    s.header(300, "Building Driver");
    s.ok(2600, "Build successful (0 warning(s))");
    s.header(300, "DKMS Setup");
    s.info(200, "Copying driver source to /usr/src/ua_apollo-0.1.0");
    s.raw(300, "Creating symlink /var/lib/dkms/ua_apollo/0.1.0/source -> /usr/src/ua_apollo-0.1.0");
    s.ok(100, "DKMS: added");
    s.raw(300, "Building module(s)...... done.");
    s.ok(1500, "DKMS: built");
    s.raw(300, "Installing /usr/lib/modules/6.x/updates/dkms/ua_apollo.ko.zst");
    s.ok(400, "DKMS: installed");
    s.ok(100, "Auto-load via udev PCI modalias (no modules-load.d entry needed)");
    s.header(300, "Installing Firmware Blobs");
    s.ok(300, "DSP programs: /lib/firmware/universal-audio (from your UAD install)");
    s.header(300, "Deploying Configs");
    s.ok(700, "PipeWire/WirePlumber/UCM2 configs deployed");
    s.header(300, "Hardware Setup");
    s.info(200, "Waiting up to 60s for Apollo on Thunderbolt bus...");
    s.ok(900, "Apollo detected: 05:00.0 Multimedia audio controller: Universal Audio Inc. Apollo (demo)");
    s.header(300, "Hardware Verification");
    s.info(200, "Stopping PipeWire (prevents race during driver init)...");
    s.ok(500, "PipeWire stopped");
    s.info(100, "Loading driver...");
    s.ok(600, "Driver loaded");
    s.ok(200, "Driver bound to Apollo (0000:05:00.0)");
    s.info(100, "Waiting for device node...");
    s.ok(400, "Device node: /dev/ua_apollo0");
    s.info(100, "Waiting for DSP handshake...");
    s.ok(1200, "DSP handshake complete");
    s.ok(300, "ALSA card registered");
    s.ok(100, "Hardware responding");
    s.info(200, "Restarting PipeWire to load new configs...");
    s.info(900, "Setting up PipeWire audio devices...");
    s.ok(700, "PipeWire virtual devices active (9 Apollo nodes)");
    s.ok(200, "Apollo Monitor L/R set as default audio output");
    s.info(100, "Apollo will auto-configure on future boots and reconnects");
    s.header(300, "Mixer Daemon");
    s.info(200, "Starting mixer daemon...");
    s.ok(900, "Mixer daemon running (TCP:4710, WS:4720)");
    s.header(300, "Summary");
    for key in ["deps", "build", "dkms", "configs", "init", "daemon"] {
        s.summary(60, GREEN, "[OK]", key);
    }
    s.raw(100, "");
    s.raw(0, format!("{GREEN}{BOLD}Installation complete.{NC}"));
    s.header(200, "Install Report");
    s.ok(500, "Report saved: /tmp/open-apollo-install-report.json");
    s.header(100, "Telemetry");
    s.info(100, "No data sent. Report saved locally at /tmp/open-apollo-install-report.json");
    s.lines
}

/// scripts/install-uaconsole.sh (no terminal, so no colors).
pub fn console(home: &Path, resources: &Path) -> Vec<(Duration, String)> {
    let mut s = Script::new(false);
    let home = home.display();
    s.header(200, "Building uaconsole (release)");
    s.ok(300, &format!("Rust 1.89.0 ({home}/.cargo/bin/cargo)"));
    for krate in ["houston", "rack", "ua-engine", "engine-model", "gfx", "plugin-ui", "uaconsole"] {
        s.raw(450, format!("   Compiling {krate} v0.1.0"));
    }
    s.raw(600, "    Finished `release` profile [optimized + debuginfo] target(s) in 1m 04s");
    s.header(200, "Installing");
    s.ok(200, &format!("{home}/.local/bin/uaconsole"));
    s.ok(
        100,
        &format!(
            "{home}/.local/bin/open-apollo-console (opens uaconsole, or the Wine Console if uaconsole is missing)"
        ),
    );
    s.ok(100, &format!("{home}/.local/share/applications/uaconsole.desktop"));
    s.header(200, "Console resources");
    s.ok(300, &format!("Found UAD Console resources: {}", resources.display()));
    s.header(200, "Fonts");
    s.ok(150, "Nimbus Sans");
    s.ok(100, "Liberation Sans");
    s.header(200, "GPU");
    s.ok(200, "Vulkan driver found (/usr/share/vulkan/icd.d)");
    s.header(200, "Done");
    s.raw(
        100,
        "  Open the console:        uaconsole   (or open-apollo-console, or 'Open Apollo Console' in your menu)",
    );
    s.lines
}

/// `oa-assets firmware` reading `root`: the real command's headers and file
/// names (`dsp/<id>.bill` DSP modules, `Apollo*.bin` USB boot images).
pub fn firmware(root: &Path, powered: usize, out: &str) -> Vec<(Duration, String)> {
    let mut s = Script::new(true);
    s.header(200, "Reading your UAD install");
    s.info(300, &format!("From {} (read-only)", root.display()));
    s.info(150, "UAD software 12.0.0.602");
    s.info(150, &format!("To {out}"));
    s.header(300, "DSP modules");
    s.info(400, &format!("Looking for module 12b in the other Powered plug-ins ({powered} DLLs)"));
    let modules = [
        ("dsp/0a5.bill", "(UAD Mic Input Strip Header.dll +0x2e1c0)"),
        ("dsp/0c2.bill", "(UAD Input Strip Header.dll +0x1b9a0)"),
        ("dsp/0da.bill", "(UAD Mic Input Strip Header.dll +0x2e960)"),
        ("dsp/0db.bill", "(UAD Input Strip Header.dll +0x1ba60)"),
        ("dsp/0eb.bill", "(UAD Input Strip Header.dll +0x1c650)"),
        ("dsp/12b.bill", "(UAD 1176 Classic.dll +0x3c4a0)"),
        ("dsp/0c9.bill", "(UAD Talkback Input Strip Header.dll +0x1f3e0)"),
    ];
    let images = ["ApolloSolo.bin", "ApolloTwin.bin", "ApolloTwinX.bin"];
    for (name, note) in modules {
        s.raw(450, file_line(name, note));
    }
    s.header(300, "USB boot images");
    for name in images {
        s.raw(350, file_line(name, "(Firmware/USB)"));
    }
    s.header(300, "Manifest");
    s.ok(400, &format!("Wrote {} files and manifest.json to {out}", modules.len() + images.len()));
    s.lines
}

/// One file row as `oa-assets firmware` prints it: `NAME  STATUS  sha256 HEX  note`.
fn file_line(name: &str, note: &str) -> String {
    format!("{name:<16} {:<7}  sha256 {}  {note}", "ok", fake_sha256(name))
}

/// A stable 64-hex-digit stand-in for a hash (FNV-1a, stretched); demo only.
fn fake_sha256(name: &str) -> String {
    (0u64..4)
        .map(|round| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ round;
            for b in name.bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
            format!("{h:016x}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installlog::{INSTALL_SH, INSTALL_UACONSOLE_SH, Progress};
    use crate::steps::Status;

    #[test]
    fn demo_driver_run_parses_as_a_clean_install() {
        let mut p = Progress::new(INSTALL_SH);
        for (_, l) in driver("Arch Linux", "6.9") {
            p.feed(&l);
        }
        p.finish(Some(0), true);
        assert!(p.substeps.iter().all(|s| s.status == Status::Done), "{:?}", p.substeps);
        assert_eq!(p.outcome.unwrap().status, Status::Done);
    }

    #[test]
    fn demo_console_run_parses_as_a_clean_install() {
        let mut p = Progress::new(INSTALL_UACONSOLE_SH);
        for (_, l) in console(Path::new("/home/u"), Path::new("/home/u/.local/share/open-apollo/console-resources")) {
            p.feed(&l);
        }
        p.finish(Some(0), false);
        assert!(p.substeps.iter().all(|s| s.status == Status::Done), "{:?}", p.substeps);
    }

    #[test]
    fn demo_firmware_lines_parse_as_files() {
        let lines = firmware(Path::new("/mnt/w"), 223, crate::firmware::FIRMWARE_DIR);
        let files: Vec<_> = lines.iter().filter_map(|(_, l)| crate::firmware::parse_file_line(l)).collect();
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "dsp/0a5.bill",
                "dsp/0c2.bill",
                "dsp/0da.bill",
                "dsp/0db.bill",
                "dsp/0eb.bill",
                "dsp/12b.bill",
                "dsp/0c9.bill",
                "ApolloSolo.bin",
                "ApolloTwin.bin",
                "ApolloTwinX.bin"
            ]
        );
        assert!(files.iter().all(|f| f.status == Status::Done && f.sha256.as_ref().is_some_and(|h| h.len() == 64)));
        assert_eq!(crate::firmware::outcome(Some(0), &files, true), Status::Done);
        let summary = crate::installlog::strip_ansi(&lines.last().unwrap().1);
        assert_eq!(summary, format!("[ OK ]  Wrote 10 files and manifest.json to {}", crate::firmware::FIRMWARE_DIR));
    }
}
