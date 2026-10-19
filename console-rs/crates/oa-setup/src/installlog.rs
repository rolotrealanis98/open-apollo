//! Reading the output of open-apollo's shell installers.
//!
//! `scripts/install.sh` and `scripts/install-uaconsole.sh` print a `── Header ──`
//! line per phase and `[ OK ]` / `[INFO]` / `[WARN]` / `[FAIL]` lines inside it
//! (install.sh colors them; the ANSI codes are stripped here), then install.sh
//! prints a summary block (`  [OK]    deps`, `  [FAIL]  build — detail`).
//! [`Progress`] turns that stream into sub-steps with a status each.

use crate::steps::Status;

/// What one output line is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Header(String),
    Ok(String),
    Info(String),
    Warn(String),
    Fail(String),
    /// An indented command the user may want to copy (hostcheck's fixes, package installs).
    Command(String),
    /// A row of install.sh's final summary.
    Summary {
        key: String,
        status: Status,
        detail: Option<String>,
    },
    Plain(String),
}

impl Line {
    pub fn text(&self) -> &str {
        match self {
            Line::Header(t)
            | Line::Ok(t)
            | Line::Info(t)
            | Line::Warn(t)
            | Line::Fail(t)
            | Line::Command(t)
            | Line::Plain(t) => t,
            Line::Summary { key, .. } => key,
        }
    }
}

/// Remove ANSI escape sequences (CSI `ESC [ ... final`, and lone two-byte escapes).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // Parameters and intermediates, then one final byte in @..~.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Classify one raw output line (may carry ANSI codes and a trailing `\r`).
pub fn classify(raw: &str) -> Line {
    let clean = strip_ansi(raw);
    let clean = clean.trim_end_matches(['\r', '\n']);
    let trimmed = clean.trim();
    if let Some(h) = trimmed.strip_prefix("── ").and_then(|t| t.strip_suffix(" ──")) {
        return Line::Header(h.trim().to_string());
    }
    for (tag, make) in [
        ("[ OK ]", Line::Ok as fn(String) -> Line),
        ("[INFO]", Line::Info),
        ("[WARN]", Line::Warn),
        ("[FAIL]", Line::Fail),
    ] {
        if let Some(rest) = clean.strip_prefix(tag) {
            return make(rest.trim().to_string());
        }
    }
    // install.sh's summary rows are indented and use [OK]/[FAIL]/[SKIP].
    if clean.starts_with("  ") {
        for (tag, status) in [("[OK]", Status::Done), ("[FAIL]", Status::Failed), ("[SKIP]", Status::Skipped)] {
            if let Some(rest) = trimmed.strip_prefix(tag) {
                let rest = rest.trim();
                let (key, detail) = match rest.split_once(" — ") {
                    Some((k, d)) => (k.trim(), Some(d.trim().to_string())),
                    None => (rest, None),
                };
                return Line::Summary { key: key.to_string(), status, detail };
            }
        }
    }
    // hostcheck's hc_cmd and the installers' fix-it commands are indented 8+ spaces.
    if clean.starts_with("        ") && !trimmed.is_empty() {
        return Line::Command(trimmed.to_string());
    }
    Line::Plain(clean.to_string())
}

/// One expected phase of a script: the header(s) that open it, its label, and
/// the summary key (install.sh) that reports its final status.
#[derive(Clone, Copy, Debug)]
pub struct Phase {
    pub headers: &'static [&'static str],
    pub label: &'static str,
    pub summary_key: Option<&'static str>,
}

const fn phase(headers: &'static [&'static str], label: &'static str, key: Option<&'static str>) -> Phase {
    Phase { headers, label, summary_key: key }
}

/// scripts/install.sh, in order. "Audio Test" only runs on a terminal, so it's not expected.
pub const INSTALL_SH: &[Phase] = &[
    phase(&["Detecting System"], "Detect system", None),
    phase(&["Dependencies"], "Dependencies", Some("deps")),
    phase(&["Building Driver"], "Build the driver", Some("build")),
    phase(&["DKMS Setup"], "DKMS (rebuilds on kernel updates)", Some("dkms")),
    phase(&["Installing Firmware Blobs"], "Firmware files", None),
    phase(&["Deploying Configs"], "Audio configs (PipeWire, WirePlumber, UCM2)", Some("configs")),
    phase(&["Hardware Setup"], "Look for the Apollo", None),
    phase(&["Hardware Verification"], "Load and verify the driver", Some("init")),
    phase(&["Mixer Daemon"], "Mixer daemon", Some("daemon")),
    phase(&["Summary"], "Summary", None),
    phase(&["Install Report", "Telemetry"], "Install report", None),
];

/// Phases install.sh runs only when asked (`--plugin-hosting`): not expected, so not listed
/// as skipped when absent, but labelled and summarised like the others when they run.
pub const OPTIONAL: &[Phase] =
    &[phase(&["Plug-ins on the Apollo (experimental)"], "Plug-ins on the Apollo (mapping)", Some("plugins"))];

/// scripts/uninstall.sh, in order. "USB install" (with uninstall-usb.sh's own
/// headers) only runs when a USB install is there.
pub const UNINSTALL_SH: &[Phase] = &[
    phase(&["Earlier install"], "Look for the earlier install", None),
    phase(&["Mixer daemon"], "Stop and remove the mixer daemon", None),
    phase(&["Unloading the driver"], "Unload the driver", None),
    phase(&["Console"], "Console (uaconsole)", None),
    phase(&["Audio settings"], "PipeWire, WirePlumber and UCM2 settings", None),
    phase(&["udev rules and helpers"], "udev rules and helpers", None),
    phase(&["Old tray and launcher"], "Old tray and launcher", None),
    phase(
        &[
            "USB install",
            "Stopping init processes",
            "Removing udev rules",
            "Removing installed scripts",
            "Restoring stock snd-usb-audio",
            "Cleaning build cache",
            "Removing WirePlumber override",
            "Removing install reports",
            "Purging firmware (--purge)",
            "Uninstall Complete",
            "Uninstall PARTIAL — REBOOT REQUIRED",
            "Uninstall PARTIAL — NO USB AUDIO DRIVER LOADED",
        ],
        "USB Apollo install",
        None,
    ),
    phase(&["Driver (DKMS)"], "Driver, DKMS and built modules", None),
    phase(&["Firmware and imported art"], "Extracted firmware and imported art", None),
    phase(&["Check"], "Check that nothing is left", None),
];

/// scripts/install-uaconsole.sh, in order.
pub const INSTALL_UACONSOLE_SH: &[Phase] = &[
    phase(&["Building uaconsole (release)"], "Build uaconsole", None),
    phase(&["Installing"], "Install into ~/.local", None),
    phase(&["Console resources"], "Find Console art", None),
    phase(&["Fonts"], "Fonts", None),
    phase(&["GPU"], "GPU driver", None),
    phase(&["Done"], "Done", None),
];

#[derive(Clone, Debug)]
pub struct SubStep {
    pub label: String,
    pub status: Status,
    /// The first warning or failure seen in this phase.
    pub note: Option<String>,
}

/// How a finished run went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: Status,
    pub message: String,
}

/// Sub-step tracking for one script run.
#[derive(Clone, Debug)]
pub struct Progress {
    plan: Vec<Phase>,
    pub substeps: Vec<SubStep>,
    current: Option<usize>,
    pub lines: Vec<Line>,
    pub reboot_needed: bool,
    /// install.sh's closing verdict line, if seen.
    pub verdict: Option<String>,
    pub outcome: Option<Outcome>,
    /// The latest `::progress` marker (the plug-in mapper's count and ETA), shown as a bar.
    pub counter: Option<Counter>,
}

/// `::progress <what> <done> <total> <eta seconds, -1 unknown> <text>`: a long phase's count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter {
    pub what: String,
    pub done: u32,
    pub total: u32,
    pub eta_seconds: Option<u32>,
    pub text: String,
}

impl Counter {
    pub fn parse(line: &str) -> Option<Counter> {
        let clean = strip_ansi(line);
        let rest = clean.trim_end().strip_prefix("::progress ")?;
        let mut parts = rest.splitn(5, ' ');
        let what = parts.next()?.to_string();
        let done = parts.next()?.parse().ok()?;
        let total = parts.next()?.parse().ok()?;
        let eta: i64 = parts.next()?.parse().ok()?;
        let text = parts.next().unwrap_or("").trim().to_string();
        Some(Counter { what, done, total, eta_seconds: u32::try_from(eta).ok(), text })
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.done as f32 / self.total as f32).min(1.0) }
    }

    /// "12/37 · ~4 min left".
    pub fn summary(&self) -> String {
        let left = match self.eta_seconds {
            None if self.done < self.total => " · estimating time left".to_string(),
            Some(s) if self.done < self.total && s < 90 => format!(" · ~{} s left", s.max(1)),
            Some(s) if self.done < self.total => format!(" · ~{} min left", (s + 30) / 60),
            _ => String::new(),
        };
        format!("{}/{}{left}", self.done, self.total)
    }
}

/// Keep the log view bounded; a kernel build can print a lot.
const MAX_LINES: usize = 20_000;

impl Progress {
    pub fn new(plan: &[Phase]) -> Self {
        Progress {
            plan: plan.to_vec(),
            substeps: plan
                .iter()
                .map(|p| SubStep { label: p.label.into(), status: Status::Pending, note: None })
                .collect(),
            current: None,
            lines: Vec::new(),
            reboot_needed: false,
            verdict: None,
            outcome: None,
            counter: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    /// The phase running now.
    pub fn current(&self) -> Option<&SubStep> {
        self.current.map(|i| &self.substeps[i])
    }

    pub fn feed(&mut self, raw: &str) {
        if let Some(c) = Counter::parse(raw) {
            self.counter = Some(c);
            return;
        }
        let line = classify(raw);
        let text = line.text().to_string();
        if text.contains("REBOOT REQUIRED") {
            self.reboot_needed = true;
        }
        if text.starts_with("Installation complete") || text.starts_with("Installation finished") {
            self.verdict = Some(text.trim().to_string());
        }
        match &line {
            Line::Header(h) => self.enter(h),
            Line::Warn(m) => self.mark(Status::Warning, m),
            Line::Fail(m) => self.mark(Status::Failed, m),
            Line::Summary { key, status, detail } => {
                if let Some(i) = self.plan.iter().position(|p| p.summary_key == Some(key.as_str())) {
                    let s = &mut self.substeps[i];
                    s.status = match status {
                        // A phase that warned and then succeeded keeps its warning.
                        Status::Done if s.status == Status::Warning => Status::Warning,
                        other => *other,
                    };
                    if s.note.is_none() {
                        s.note = detail.clone();
                    }
                }
            }
            _ => {}
        }
        if self.lines.len() < MAX_LINES {
            self.lines.push(line);
        } else if self.lines.len() == MAX_LINES {
            self.lines.push(Line::Info("(log truncated here; the rest is not shown)".into()));
        }
    }

    fn enter(&mut self, header: &str) {
        let found = self.plan.iter().position(|p| p.headers.iter().any(|h| h.eq_ignore_ascii_case(header)));
        let idx = match found {
            Some(i) => i,
            None => {
                // An optional or unknown phase: shown where it ran, after the current one.
                let at = self.current.map_or(self.substeps.len(), |i| i + 1);
                let known = OPTIONAL.iter().find(|p| p.headers.iter().any(|h| h.eq_ignore_ascii_case(header)));
                let (p, label) = match known {
                    Some(p) => (*p, p.label.to_string()),
                    None => (phase(&[], "", None), header.to_string()),
                };
                self.plan.insert(at, p);
                self.substeps.insert(at, SubStep { label, status: Status::Pending, note: None });
                at
            }
        };
        if self.current == Some(idx) {
            return;
        }
        self.close_current();
        self.current = Some(idx);
        let s = &mut self.substeps[idx];
        if !s.status.is_finished() {
            s.status = Status::Running;
        }
    }

    fn close_current(&mut self) {
        if let Some(i) = self.current.take()
            && self.substeps[i].status == Status::Running
        {
            self.substeps[i].status = Status::Done;
        }
    }

    fn mark(&mut self, status: Status, msg: &str) {
        let Some(i) = self.current else { return };
        let s = &mut self.substeps[i];
        // The first failure replaces an earlier warning as the phase's note.
        let first_failure = status == Status::Failed && s.status != Status::Failed;
        s.status = s.status.worst(status);
        if s.note.is_none() || first_failure {
            s.note = Some(msg.to_string());
        }
    }

    /// The process ended with `exit` (`None`: killed by a signal). `pkexec` is
    /// true when the script ran through pkexec, whose own errors use 126/127.
    pub fn finish(&mut self, exit: Option<i32>, pkexec: bool) {
        self.close_current();
        for s in &mut self.substeps {
            if s.status == Status::Pending || s.status == Status::Running {
                s.status = Status::Skipped;
            }
        }
        let any_failed = self.substeps.iter().any(|s| s.status == Status::Failed);
        let any_warned = self.substeps.iter().any(|s| s.status == Status::Warning);
        let outcome = match exit {
            Some(126) if pkexec => Outcome {
                status: Status::Failed,
                message: "The password dialog was cancelled, so nothing was installed.".into(),
            },
            Some(127) if pkexec => Outcome {
                status: Status::Failed,
                message: "Administrator rights weren't granted (wrong password, or no password dialog is running)."
                    .into(),
            },
            Some(0) if self.reboot_needed => {
                Outcome { status: Status::Warning, message: "Finished. Restart the computer to finish setup.".into() }
            }
            Some(0) if any_failed || self.verdict.as_deref().is_some_and(|v| v.contains("issues")) => Outcome {
                status: Status::Warning,
                message: "Finished with problems: see the steps marked in red or amber.".into(),
            },
            Some(0) if any_warned => {
                Outcome { status: Status::Warning, message: "Finished, with warnings worth a look.".into() }
            }
            Some(0) => Outcome { status: Status::Done, message: "Finished successfully.".into() },
            Some(code) => {
                Outcome { status: Status::Failed, message: format!("Stopped with an error (exit code {code}).") }
            }
            None => Outcome { status: Status::Failed, message: "Stopped before finishing.".into() },
        };
        self.outcome = Some(outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREEN: &str = "\x1b[0;32m";
    const YELLOW: &str = "\x1b[1;33m";
    const RED: &str = "\x1b[0;31m";
    const BOLD: &str = "\x1b[1m";
    const NC: &str = "\x1b[0m";

    fn ok(m: &str) -> String {
        format!("{GREEN}[ OK ]{NC}  {m}")
    }
    fn warn(m: &str) -> String {
        format!("{YELLOW}[WARN]{NC}  {m}")
    }
    fn fail(m: &str) -> String {
        format!("{RED}[FAIL]{NC}  {m}")
    }
    fn header(h: &str) -> String {
        format!("{BOLD}── {h} ──{NC}")
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip_ansi("\x1b[0;32m[ OK ]\x1b[0m  gcc"), "[ OK ]  gcc");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("a\x1b[1;33mb\x1b[0m\x1b"), "ab");
    }

    #[test]
    fn classifies_installer_lines() {
        assert_eq!(classify(&header("Building Driver")), Line::Header("Building Driver".into()));
        assert_eq!(
            classify(&ok("Build successful (0 warning(s))")),
            Line::Ok("Build successful (0 warning(s))".into())
        );
        assert_eq!(classify("\x1b[0;36m[INFO]\x1b[0m  Kernel: 6.9"), Line::Info("Kernel: 6.9".into()));
        assert_eq!(
            classify(&warn("dkms not installed — skipping")),
            Line::Warn("dkms not installed — skipping".into())
        );
        assert_eq!(classify("[FAIL]  Build failed\r"), Line::Fail("Build failed".into()));
        assert_eq!(
            classify(&format!("  {RED}[FAIL]{NC}  build — make: *** Error 2")),
            Line::Summary { key: "build".into(), status: Status::Failed, detail: Some("make: *** Error 2".into()) }
        );
        assert_eq!(
            classify(&format!("  {GREEN}[OK]{NC}    deps")),
            Line::Summary { key: "deps".into(), status: Status::Done, detail: None }
        );
        assert_eq!(
            classify(&format!("          {BOLD}boltctl enroll --policy auto 1234{NC}")),
            Line::Command("boltctl enroll --policy auto 1234".into())
        );
        assert_eq!(classify("  CC [M]  ua_core.o"), Line::Plain("  CC [M]  ua_core.o".into()));
        assert_eq!(classify(""), Line::Plain(String::new()));
    }

    /// install.sh on a machine with no Apollo connected, DKMS present, IOMMU passthrough already on.
    fn no_hardware_run() -> Vec<String> {
        vec![
            String::new(),
            format!("{BOLD}Open Apollo — Installer{NC}"),
            String::new(),
            header("Detecting System"),
            ok("Distro: Arch Linux"),
            "\x1b[0;36m[INFO]\x1b[0m  Kernel: 6.9.1 | Arch: x86_64 | RAM: 32000MB".into(),
            warn("No Apollo hardware detected (lspci -d 1a00:)"),
            header("Dependencies"),
            ok("Kernel headers"),
            ok("All required dependencies present"),
            header("Building Driver"),
            ok("Build successful (0 warning(s))"),
            header("DKMS Setup"),
            ok("DKMS: added"),
            ok("DKMS: built"),
            ok("DKMS: installed"),
            header("Installing Firmware Blobs"),
            ok("plugin-chain firmware: /lib/firmware/ua-apollo-plugin-chain.bin"),
            header("Deploying Configs"),
            ok("PipeWire/WirePlumber/UCM2 configs deployed"),
            header("Hardware Setup"),
            "\x1b[0;36m[INFO]\x1b[0m  Waiting up to 60s for Apollo on Thunderbolt bus...".into(),
            warn("Apollo not detected"),
            header("Hardware Verification"),
            "\x1b[0;36m[INFO]\x1b[0m  No Apollo hardware detected on Thunderbolt bus".into(),
            header("Summary"),
            format!("  {GREEN}[OK]{NC}    deps"),
            format!("  {GREEN}[OK]{NC}    build"),
            format!("  {GREEN}[OK]{NC}    dkms"),
            format!("  {GREEN}[OK]{NC}    configs"),
            format!("  {YELLOW}[SKIP]{NC}  init"),
            format!("  {YELLOW}[SKIP]{NC}  daemon"),
            String::new(),
            format!("{GREEN}{BOLD}Installation complete.{NC}"),
            header("Install Report"),
            ok("Report saved: /tmp/open-apollo-install-report.json"),
            header("Telemetry"),
            "\x1b[0;36m[INFO]\x1b[0m  No data sent. Report saved locally at /tmp/open-apollo-install-report.json"
                .into(),
        ]
    }

    #[test]
    fn progress_markers_feed_a_counter_not_the_log() {
        let mut p = Progress::new(INSTALL_SH);
        p.feed(&header("Mixer Daemon"));
        p.feed(&header("Plug-ins on the Apollo (experimental)"));
        let labels: Vec<_> = p.substeps.iter().map(|s| s.label.as_str()).collect();
        let at = labels.iter().position(|l| *l == "Plug-ins on the Apollo (mapping)").unwrap();
        assert_eq!(labels[at - 1], "Mixer daemon", "shown right after the phase it followed");
        assert_eq!(p.current().unwrap().label, "Plug-ins on the Apollo (mapping)");
        let lines = p.lines.len();
        p.feed("::progress plugins 12 37 240 UAD Pultec EQP-1A");
        assert_eq!(p.lines.len(), lines);
        let c = p.counter.clone().unwrap();
        assert_eq!((c.what.as_str(), c.done, c.total, c.eta_seconds), ("plugins", 12, 37, Some(240)));
        assert_eq!(c.text, "UAD Pultec EQP-1A");
        assert_eq!(c.summary(), "12/37 · ~4 min left");
        p.feed("::progress plugins 13 37 -1 x");
        assert_eq!(p.counter.as_ref().unwrap().summary(), "13/37 · estimating time left");
        p.feed("::progress plugins 37 37 0 done");
        assert_eq!(p.counter.as_ref().unwrap().summary(), "37/37");
        assert!(Counter::parse("::progress plugins x 37 0").is_none());
        p.feed("  [OK]    plugins");
        assert_eq!(p.substeps[at].status, Status::Done);
    }

    #[test]
    fn without_plugin_hosting_no_plugin_phase_is_listed() {
        let p = Progress::new(INSTALL_SH);
        assert!(p.substeps.iter().all(|s| !s.label.starts_with("Plug-ins")));
    }

    fn status_of(p: &Progress, label_start: &str) -> Status {
        p.substeps.iter().find(|s| s.label.starts_with(label_start)).unwrap().status
    }

    #[test]
    fn maps_a_run_without_hardware_to_substeps() {
        let mut p = Progress::new(INSTALL_SH);
        let lines = no_hardware_run();
        for l in &lines[..12] {
            p.feed(l);
        }
        assert_eq!(status_of(&p, "Build the driver"), Status::Running);
        assert_eq!(p.current().unwrap().label, "Build the driver");
        assert_eq!(status_of(&p, "Detect system"), Status::Warning);
        assert_eq!(status_of(&p, "Dependencies"), Status::Done);
        assert_eq!(status_of(&p, "DKMS"), Status::Pending);
        for l in &lines[12..] {
            p.feed(l);
        }
        p.finish(Some(0), true);
        assert_eq!(status_of(&p, "DKMS"), Status::Done);
        assert_eq!(status_of(&p, "Look for the Apollo"), Status::Warning);
        assert_eq!(status_of(&p, "Load and verify"), Status::Skipped, "summary [SKIP] init");
        assert_eq!(status_of(&p, "Mixer daemon"), Status::Skipped, "summary row for a phase that never ran");
        assert_eq!(status_of(&p, "Install report"), Status::Done, "two headers, one sub-step");
        assert_eq!(p.verdict.as_deref(), Some("Installation complete."));
        let out = p.outcome.clone().unwrap();
        assert_eq!(out.status, Status::Warning, "the missing Apollo is a warning");
        assert!(!p.reboot_needed);
        assert_eq!(p.substeps.len(), INSTALL_SH.len());
    }

    #[test]
    fn a_failed_build_fails_its_substep_and_the_run() {
        let mut p = Progress::new(INSTALL_SH);
        for l in [
            header("Detecting System"),
            ok("Distro: Fedora"),
            header("Building Driver"),
            fail("Build failed"),
            "ua_core.c:12: error: foo".into(),
            fail("Build failed — cannot continue"),
            header("Install Report"),
        ] {
            p.feed(&l);
        }
        p.finish(Some(1), true);
        let build = p.substeps.iter().find(|s| s.label == "Build the driver").unwrap();
        assert_eq!(build.status, Status::Failed);
        assert_eq!(build.note.as_deref(), Some("Build failed"));
        assert_eq!(status_of(&p, "DKMS"), Status::Skipped);
        assert_eq!(p.outcome.unwrap().status, Status::Failed);
    }

    #[test]
    fn summary_rows_override_and_reboot_is_noticed() {
        let mut p = Progress::new(INSTALL_SH);
        p.feed(&header("DKMS Setup"));
        p.feed(&warn("dkms not installed — skipping"));
        p.feed(&warn("REBOOT REQUIRED for iommu=pt to take effect"));
        p.feed(&header("Summary"));
        p.feed(&format!("  {YELLOW}[SKIP]{NC}  dkms"));
        p.feed(&format!("  {RED}[FAIL]{NC}  configs — deploy.sh not found"));
        p.finish(Some(0), true);
        assert_eq!(status_of(&p, "DKMS"), Status::Skipped);
        let configs = p.substeps.iter().find(|s| s.label.starts_with("Audio configs")).unwrap();
        assert_eq!((configs.status, configs.note.as_deref()), (Status::Failed, Some("deploy.sh not found")));
        assert!(p.reboot_needed);
        assert_eq!(p.outcome.unwrap().status, Status::Warning);
    }

    #[test]
    fn pkexec_exit_codes_explain_themselves() {
        let mut p = Progress::new(INSTALL_SH);
        p.finish(Some(126), true);
        let o = p.outcome.unwrap();
        assert_eq!(o.status, Status::Failed);
        assert!(o.message.contains("cancelled"));
        let mut p = Progress::new(INSTALL_UACONSOLE_SH);
        p.finish(Some(126), false);
        assert!(p.outcome.unwrap().message.contains("exit code 126"));
        let mut p = Progress::new(INSTALL_SH);
        p.finish(None, false);
        assert_eq!(p.outcome.unwrap().status, Status::Failed);
    }

    #[test]
    fn maps_an_uninstall_with_a_usb_install() {
        let mut p = Progress::new(UNINSTALL_SH);
        for l in [
            header("Earlier install"),
            "[INFO]  Found: ua_apollo 0.1.0 (DKMS)".into(),
            header("Mixer daemon"),
            ok("Removed /etc/systemd/user/ua-mixer-daemon.service"),
            header("USB install"),
            "Open Apollo USB — Uninstaller".into(),
            header("Stopping init processes"),
            header("Removing udev rules"),
            ok("Removed /etc/udev/rules.d/99-apollo-usb.rules"),
            header("Uninstall Complete"),
            header("Driver (DKMS)"),
            ok("Removed DKMS registration ua_apollo/0.1.0"),
            header("Check"),
            fail("Still here: /etc/udev/rules.d/91-ua-apollo.rules"),
        ] {
            p.feed(&l);
        }
        p.finish(Some(1), true);
        assert_eq!(p.substeps.len(), UNINSTALL_SH.len(), "uninstall-usb.sh's headers fold into one sub-step");
        assert_eq!(status_of(&p, "USB Apollo install"), Status::Done);
        assert_eq!(status_of(&p, "Unload the driver"), Status::Skipped);
        let check = p.substeps.iter().find(|s| s.label.starts_with("Check")).unwrap();
        assert_eq!(check.status, Status::Failed);
        assert_eq!(check.note.as_deref(), Some("Still here: /etc/udev/rules.d/91-ua-apollo.rules"));
    }

    #[test]
    fn unknown_headers_become_extra_substeps() {
        let mut p = Progress::new(INSTALL_UACONSOLE_SH);
        p.feed(&header("Copying Console resources"));
        p.feed(&ok("Copied (40M)."));
        p.feed(&header("Building uaconsole (release)"));
        p.feed("   Compiling houston v0.1.0");
        p.finish(Some(0), false);
        assert_eq!(p.substeps.last().unwrap().label, "Copying Console resources");
        assert_eq!(p.substeps.last().unwrap().status, Status::Done);
        assert_eq!(status_of(&p, "Build uaconsole"), Status::Done);
        assert_eq!(status_of(&p, "Fonts"), Status::Skipped);
        assert_eq!(p.outcome.unwrap().status, Status::Done);
    }
}
