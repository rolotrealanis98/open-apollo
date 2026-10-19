//! The one privileged run: `scripts/setup-root.sh`, started once from "Review
//! and install" so the password is asked once.
//!
//! The script checks the UAD install, removes the earlier install, imports the
//! art, extracts the firmware, installs the driver and closes a partition it
//! opened, printing `::step <id> start|done|warn|failed|skipped [message]`
//! around each section. [`SetupRun`] routes the output between those markers
//! to one [`Section`] each (sub-steps, log lines, firmware file rows), which
//! the Remove / Import / Firmware / Driver pages show as live progress.
//!
//! Before the run, `scripts/uninstall.sh --list` (no root) says what an earlier
//! install left; [`parse_list`] reads it for the Review page.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use oa_assets::detect::Install;
use oa_assets::firmware::Pins;

use crate::firmware::{self, FileLine};
use crate::installlog::{INSTALL_SH, Line, Outcome, Phase, Progress, UNINSTALL_SH, classify};
use crate::steps::{Status, StepId};

/// setup-root.sh's sections, in the order it runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RootStep {
    Source,
    Remove,
    Import,
    Firmware,
    Driver,
    Unmount,
}

impl RootStep {
    pub const ALL: [RootStep; 6] =
        [RootStep::Source, RootStep::Remove, RootStep::Import, RootStep::Firmware, RootStep::Driver, RootStep::Unmount];

    pub fn key(self) -> &'static str {
        match self {
            RootStep::Source => "source",
            RootStep::Remove => "remove",
            RootStep::Import => "import",
            RootStep::Firmware => "firmware",
            RootStep::Driver => "driver",
            RootStep::Unmount => "unmount",
        }
    }

    pub fn parse(s: &str) -> Option<RootStep> {
        Self::ALL.into_iter().find(|r| r.key() == s)
    }

    /// The wizard page that shows this section (the unmount shows on Done).
    pub fn page(self) -> Option<StepId> {
        match self {
            RootStep::Source => Some(StepId::Review),
            RootStep::Remove => Some(StepId::Remove),
            RootStep::Import => Some(StepId::Import),
            RootStep::Firmware => Some(StepId::Firmware),
            RootStep::Driver => Some(StepId::Driver),
            RootStep::Unmount => None,
        }
    }

    /// The `── Header ──` setup-root.sh prints for the section itself.
    pub fn header(self) -> &'static str {
        match self {
            RootStep::Source => "Checking your UAD install",
            RootStep::Remove => "Removing the earlier install",
            RootStep::Import => "Importing the Console art",
            RootStep::Firmware => "Extracting DSP programs and firmware",
            RootStep::Driver => "Installing the driver",
            RootStep::Unmount => "Closing the Windows partition",
        }
    }

    /// For "Stopped at …".
    pub fn doing(self) -> &'static str {
        match self {
            RootStep::Source => "checking your UAD install",
            RootStep::Remove => "removing the earlier install",
            RootStep::Import => "importing the Console art",
            RootStep::Firmware => "extracting the DSP programs and firmware",
            RootStep::Driver => "installing the driver",
            RootStep::Unmount => "closing the Windows partition",
        }
    }

    /// The script the section runs, whose headers become its sub-steps.
    fn plan(self) -> &'static [Phase] {
        match self {
            RootStep::Remove => UNINSTALL_SH,
            RootStep::Driver => INSTALL_SH,
            _ => &[],
        }
    }
}

/// `::step remove done The earlier install is gone` -> (Remove, Done, Some(message)).
/// `start` is `Running`; an unknown step or state is no marker.
pub fn parse_marker(line: &str) -> Option<(RootStep, Status, Option<String>)> {
    let clean = crate::installlog::strip_ansi(line);
    let rest = clean.trim_end().strip_prefix("::step ")?;
    let mut parts = rest.splitn(3, ' ');
    let step = RootStep::parse(parts.next()?)?;
    let status = match parts.next()? {
        "start" => Status::Running,
        "done" => Status::Done,
        "warn" => Status::Warning,
        "failed" => Status::Failed,
        "skipped" => Status::Skipped,
        _ => return None,
    };
    let message = parts.next().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
    Some((step, status, message))
}

/// One section of the run.
#[derive(Clone, Debug)]
pub struct Section {
    pub id: RootStep,
    pub status: Status,
    /// The marker's message when it ended.
    pub message: Option<String>,
    pub progress: Progress,
    /// `oa-assets firmware`'s file rows (the firmware section).
    pub files: Vec<FileLine>,
}

/// The run's output, sorted into sections.
#[derive(Clone, Debug)]
pub struct SetupRun {
    pub sections: Vec<Section>,
    current: Option<usize>,
    /// Lines outside any section (the banner, pkexec's own errors).
    pub other: Vec<Line>,
    pub outcome: Option<Outcome>,
}

impl Default for SetupRun {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupRun {
    pub fn new() -> Self {
        SetupRun {
            sections: RootStep::ALL
                .iter()
                .map(|&id| Section {
                    id,
                    status: Status::Pending,
                    message: None,
                    progress: Progress::new(id.plan()),
                    files: Vec::new(),
                })
                .collect(),
            current: None,
            other: Vec::new(),
            outcome: None,
        }
    }

    pub fn section(&self, id: RootStep) -> &Section {
        &self.sections[RootStep::ALL.iter().position(|r| *r == id).expect("every step has a section")]
    }

    fn section_mut(&mut self, id: RootStep) -> &mut Section {
        &mut self.sections[RootStep::ALL.iter().position(|r| *r == id).expect("every step has a section")]
    }

    pub fn running(&self) -> bool {
        self.outcome.is_none()
    }

    /// The section running now.
    pub fn current(&self) -> Option<RootStep> {
        self.current.map(|i| self.sections[i].id)
    }

    pub fn status(&self, id: RootStep) -> Status {
        self.section(id).status
    }

    /// The section that failed, if one did.
    pub fn failed_at(&self) -> Option<RootStep> {
        self.sections.iter().find(|s| s.status == Status::Failed).map(|s| s.id)
    }

    /// Nothing on this computer was changed: no section past the source check started.
    pub fn nothing_changed(&self) -> bool {
        self.sections
            .iter()
            .filter(|s| !matches!(s.id, RootStep::Source | RootStep::Unmount))
            .all(|s| s.status == Status::Pending)
    }

    /// Feed one output line. Returns the section a `start` marker opened.
    pub fn feed(&mut self, raw: &str) -> Option<RootStep> {
        if let Some((id, status, message)) = parse_marker(raw) {
            let s = self.section_mut(id);
            if status == Status::Running {
                s.status = Status::Running;
                s.message = None;
                self.current = RootStep::ALL.iter().position(|r| *r == id);
                return Some(id);
            }
            s.status = status;
            s.message = message;
            s.progress.finish(Some(if status == Status::Failed { 1 } else { 0 }), false);
            if self.current() == Some(id) {
                self.current = None;
            }
            return None;
        }
        match self.current {
            Some(i) => {
                let s = &mut self.sections[i];
                // The section's own header is its title, not a sub-step.
                if classify(raw) == Line::Header(s.id.header().to_string()) {
                    return None;
                }
                if s.id == RootStep::Firmware
                    && let Some(f) = firmware::parse_file_line(raw)
                {
                    s.files.push(f);
                }
                s.progress.feed(raw);
            }
            None => self.other.push(classify(raw)),
        }
        None
    }

    /// The script ended with `exit` (`None`: killed). `pkexec` exit codes 126
    /// and 127 are its own: the password dialog was cancelled or refused.
    pub fn finish(&mut self, exit: Option<i32>, pkexec: bool) {
        if let Some(i) = self.current.take() {
            let s = &mut self.sections[i];
            s.status = Status::Failed;
            s.message.get_or_insert_with(|| "Stopped before this step finished.".into());
            s.progress.finish(exit, false);
        }
        let unchanged = self.nothing_changed();
        let started = self.sections.iter().any(|s| s.status != Status::Pending);
        let outcome = match exit {
            Some(126) if pkexec && !started => Outcome {
                status: Status::Failed,
                message: "The password dialog was cancelled. Nothing was changed.".into(),
            },
            Some(127) if pkexec && !started => Outcome {
                status: Status::Failed,
                message: "Administrator rights weren't granted (wrong password, or no password dialog is running). Nothing was changed.".into(),
            },
            Some(0) => {
                let worst = self
                    .sections
                    .iter()
                    .filter(|s| s.status != Status::Skipped)
                    .fold(Status::Done, |w, s| w.worst(s.status));
                match worst {
                    Status::Done | Status::Pending => Outcome { status: Status::Done, message: "Installed.".into() },
                    _ => Outcome {
                        status: Status::Warning,
                        message: "Installed, with warnings worth a look (marked in amber).".into(),
                    },
                }
            }
            code => {
                let at = self.failed_at();
                let what = match (at, code) {
                    (Some(at), _) => format!(
                        "Stopped while {}{}",
                        at.doing(),
                        self.section(at).message.as_deref().map(|m| format!(": {m}")).unwrap_or_default()
                    ),
                    (None, Some(c)) => format!("Stopped with an error (exit code {c})"),
                    (None, None) => "Stopped before finishing".into(),
                };
                let changed = match self.status(RootStep::Remove) {
                    _ if unchanged => " Nothing was changed on this computer.",
                    Status::Failed => " Part of the earlier install may be removed already: run the install again.",
                    Status::Done | Status::Warning => {
                        " The earlier install was already removed: run the install again to finish."
                    }
                    _ => "",
                };
                Outcome { status: Status::Failed, message: format!("{}.{changed}", what.trim_end_matches('.')) }
            }
        };
        self.outcome = Some(outcome);
    }

    /// The firmware section's status, judging its files: a missing DSP module
    /// (or a USB boot image when a USB Apollo is there) makes it a warning.
    pub fn firmware_status(&self, usb_apollo: bool) -> Status {
        let s = self.section(RootStep::Firmware);
        match s.status {
            Status::Done => firmware::outcome(Some(0), &s.files, usb_apollo),
            other => other,
        }
    }
}

/// The wizard's step statuses the run implies: each section's page gets the
/// section's status (the firmware judged by its files, see
/// [`SetupRun::firmware_status`]). Review shows the source check; before its
/// marker arrives it is running (the password dialog), or, once the run ended
/// without one, the run's outcome (pkexec cancelled or refused).
pub fn step_statuses(run: &SetupRun, usb_apollo: bool) -> Vec<(StepId, Status)> {
    run.sections
        .iter()
        .filter_map(|s| {
            let status = match s.id {
                RootStep::Firmware => run.firmware_status(usb_apollo),
                RootStep::Source if s.status == Status::Pending => {
                    run.outcome.as_ref().map(|o| o.status).unwrap_or(Status::Running)
                }
                _ => s.status,
            };
            Some((s.id.page()?, status))
        })
        .collect()
}

/// Where setup-root.sh reads the UAD install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A folder (a mounted partition, an attached VM image's partition, a Wine prefix).
    Folder(PathBuf),
    /// A Windows partition that isn't mounted: the run opens it read-only.
    Partition(String),
}

/// Where setup-root.sh installs the plug-in panel specs (scripts/install-panels.sh).
pub const PANEL_SPECS_DIR: &str = "/usr/share/open-apollo/plugin-specs";

/// What the Install button runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub user: String,
    pub source: Source,
    pub oa_assets: Option<PathBuf>,
    /// `--data`, when not the default folder.
    pub data: Option<PathBuf>,
    pub keep_current: bool,
    pub send_report: bool,
    /// `--plugin-hosting`: load the driver with its plug-in host on and map the
    /// user's plug-ins (scripts/setup-plugin-hosting.sh). Experimental, opt-in.
    pub plugin_hosting: bool,
    /// `--panels`: a plug-in panel specs tarball instead of the pinned download
    /// (`$OA_PANELS_TARBALL`, for testing a panels build), with its `--panels-sha256`.
    pub panels: Option<PathBuf>,
    pub panels_sha256: Option<String>,
}

impl Plan {
    /// setup-root.sh's arguments.
    pub fn args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--user".into(), self.user.clone().into()];
        match &self.source {
            Source::Folder(p) => a.extend(["--from".into(), p.into()]),
            Source::Partition(d) => a.extend(["--mount".into(), d.into()]),
        }
        if let Some(x) = &self.oa_assets {
            a.extend(["--oa-assets".into(), x.into()]);
        }
        if let Some(d) = &self.data {
            a.extend(["--data".into(), d.into()]);
        }
        if self.keep_current {
            a.push("--keep-current".into());
        }
        if self.send_report {
            a.push("--send-report".into());
        }
        if self.plugin_hosting {
            a.push("--plugin-hosting".into());
        }
        if let Some(p) = &self.panels {
            a.extend(["--panels".into(), p.into()]);
        }
        if let Some(h) = &self.panels_sha256 {
            a.extend(["--panels-sha256".into(), h.into()]);
        }
        a
    }

    /// `pkexec bash /repo/scripts/setup-root.sh --user …` as a user would type it
    /// (`sudo` instead of `pkexec` for a terminal).
    pub fn command_line(&self, elevate: &str, script: &Path) -> String {
        let mut parts = vec![elevate.to_string(), "bash".into(), firmware::shell_quote(&script.to_string_lossy())];
        parts.extend(self.args().iter().map(|a| firmware::shell_quote(&a.to_string_lossy())));
        parts.join(" ")
    }
}

/// What `install` lacks for the install to work, as the install's source check
/// (setup-root.sh) sees it: the UAD Console (its art), and the DSP programs'
/// sources (the Console strip plug-ins or UAD2SDK.dll). Empty when usable.
pub fn missing_parts(install: &Install) -> Vec<&'static str> {
    static PINS: OnceLock<Pins> = OnceLock::new();
    let pins = PINS.get_or_init(Pins::builtin);
    let mut out = Vec::new();
    if install.console.is_none() {
        out.push("UAD Console");
    }
    let strip = install.powered_plugins.iter().any(|p| {
        p.file_name().is_some_and(|n| pins.strip_headers.iter().any(|h| n.to_string_lossy().eq_ignore_ascii_case(h)))
    });
    if !strip && install.sdk_dll.is_none() {
        out.push("Console strip plug-ins or UAD2SDK.dll");
    }
    out
}

/// One thing an earlier install left, from `uninstall.sh --list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    pub kind: String,
    pub item: String,
}

/// `uninstall.sh --list` output: `KIND<TAB>ITEM` lines.
pub fn parse_list(text: &str) -> Vec<Leftover> {
    text.lines()
        .filter_map(|l| {
            let (kind, item) = l.split_once('\t')?;
            Some(Leftover { kind: kind.trim().to_string(), item: item.trim().to_string() })
        })
        .filter(|l| !l.kind.is_empty() && !l.item.is_empty())
        .collect()
}

/// The groups the Review page shows, in order, with their labels.
pub const KINDS: &[(&str, &str)] = &[
    ("dkms", "Driver (DKMS)"),
    ("module", "Loaded now"),
    ("process", "Running now"),
    ("driver", "Driver files"),
    ("daemon", "Mixer daemon units"),
    ("config", "Audio settings"),
    ("udev", "udev rules and helpers"),
    ("console", "Console (uaconsole)"),
    ("panels", "Plug-in panel specs"),
    ("legacy", "Old tray and launcher"),
    ("usb", "USB install"),
    ("firmware", "Extracted firmware"),
    ("data", "Imported art and caches"),
];

/// Leftovers grouped by kind, in [`KINDS`] order; unknown kinds last.
pub fn group(list: &[Leftover]) -> Vec<(&str, Vec<&str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for (kind, label) in KINDS {
        let items: Vec<&str> = list.iter().filter(|l| l.kind == *kind).map(|l| l.item.as_str()).collect();
        if !items.is_empty() {
            out.push((label, items));
        }
    }
    let other: Vec<&str> =
        list.iter().filter(|l| !KINDS.iter().any(|(k, _)| *k == l.kind)).map(|l| l.item.as_str()).collect();
    if !other.is_empty() {
        out.push(("Other", other));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREEN: &str = "\x1b[0;32m";
    const NC: &str = "\x1b[0m";

    fn feed_all(run: &mut SetupRun, lines: &[&str]) {
        for l in lines {
            run.feed(l);
        }
    }

    fn good_run() -> Vec<String> {
        vec![
            String::new(),
            "Open Apollo — Setup".into(),
            "::step source start".into(),
            "── Checking your UAD install ──".into(),
            format!("{GREEN}[ OK ]{NC}  UAD Console 1.3.1"),
            "::step source done UAD install found in /mnt/w".into(),
            "::step remove start".into(),
            "── Removing the earlier install ──".into(),
            "── Earlier install ──".into(),
            "[INFO]  Found: ua_apollo 0.1.0 (DKMS)".into(),
            "── Driver (DKMS) ──".into(),
            "[ OK ]  Removed DKMS registration ua_apollo/0.1.0".into(),
            "── Check ──".into(),
            "[ OK ]  Nothing from open-apollo is left".into(),
            "::step remove done The earlier install is gone".into(),
            "::step import start".into(),
            "Imported UAD Console 1.3.1 resources: 10 files".into(),
            "::step import done Console art imported".into(),
            "::step firmware start".into(),
            "── DSP modules ──".into(),
            "dsp/0c2.bill     ok       sha256 aa  (x)".into(),
            "::step firmware done Extracted to /lib/firmware/universal-audio".into(),
            "::step driver start".into(),
            "── Installing the driver ──".into(),
            "── Building Driver ──".into(),
            "[ OK ]  Build successful (0 warning(s))".into(),
            "::step driver done Installed".into(),
        ]
    }

    #[test]
    fn parses_markers() {
        assert_eq!(parse_marker("::step remove start"), Some((RootStep::Remove, Status::Running, None)));
        assert_eq!(
            parse_marker("::step driver warn Installed; restart the computer to finish"),
            Some((RootStep::Driver, Status::Warning, Some("Installed; restart the computer to finish".into())))
        );
        assert_eq!(parse_marker("::step remove skipped Kept").map(|m| m.1), Some(Status::Skipped));
        assert_eq!(parse_marker("::step unmount failed").map(|m| m.1), Some(Status::Failed));
        assert_eq!(parse_marker("::step bogus start"), None);
        assert_eq!(parse_marker("::step remove later"), None);
        assert_eq!(parse_marker("[ OK ]  ::step remove start"), None);
    }

    #[test]
    fn markers_drive_sections_and_route_lines() {
        let mut r = SetupRun::new();
        let lines = good_run();
        let mut opened = Vec::new();
        for l in &lines {
            opened.extend(r.feed(l));
        }
        assert_eq!(
            opened,
            [RootStep::Source, RootStep::Remove, RootStep::Import, RootStep::Firmware, RootStep::Driver]
        );
        assert_eq!(r.current(), None);
        r.finish(Some(0), true);
        for id in [RootStep::Source, RootStep::Remove, RootStep::Import, RootStep::Firmware, RootStep::Driver] {
            assert_eq!(r.status(id), Status::Done, "{id:?}");
        }
        assert_eq!(r.status(RootStep::Unmount), Status::Pending, "nothing was mounted");
        let remove = r.section(RootStep::Remove);
        assert_eq!(remove.message.as_deref(), Some("The earlier install is gone"));
        let label = |p: &Progress, l: &str| p.substeps.iter().find(|s| s.label.starts_with(l)).unwrap().status;
        assert_eq!(label(&remove.progress, "Look for the earlier install"), Status::Done);
        assert_eq!(label(&remove.progress, "Driver, DKMS"), Status::Done);
        assert_eq!(label(&remove.progress, "Unload the driver"), Status::Skipped, "never ran");
        assert!(
            !remove.progress.substeps.iter().any(|s| s.label == "Removing the earlier install"),
            "the section's own header isn't a sub-step"
        );
        assert_eq!(r.section(RootStep::Firmware).files.len(), 1);
        assert_eq!(label(&r.section(RootStep::Driver).progress, "Build the driver"), Status::Done);
        assert!(r.other.iter().any(|l| l.text() == "Open Apollo — Setup"), "the banner is outside any section");
        assert_eq!(r.outcome.unwrap().status, Status::Done);
    }

    #[test]
    fn a_cancelled_password_dialog_changed_nothing() {
        let mut r = SetupRun::new();
        r.feed("Error executing command as another user: Request dismissed");
        r.finish(Some(126), true);
        let o = r.outcome.clone().unwrap();
        assert_eq!(o.status, Status::Failed);
        assert!(o.message.contains("cancelled") && o.message.contains("Nothing was changed"), "{}", o.message);
        assert!(r.nothing_changed());
        let mut r = SetupRun::new();
        r.finish(Some(127), true);
        assert!(r.outcome.unwrap().message.contains("weren't granted"));
    }

    /// The good run cut off at `id`'s start, then that section fails.
    fn failing_at(id: RootStep) -> SetupRun {
        let mut r = SetupRun::new();
        let start = format!("::step {} start", id.key());
        let lines = good_run();
        let upto = lines.iter().position(|l| *l == start).unwrap();
        feed_all(&mut r, &lines[..=upto].iter().map(String::as_str).collect::<Vec<_>>());
        r.feed("[FAIL]  something broke");
        r.feed(&format!("::step {} failed it broke", id.key()));
        r.finish(Some(1), true);
        r
    }

    #[test]
    fn a_failure_in_each_step_stops_there() {
        for (i, id) in RootStep::ALL[..5].iter().enumerate() {
            let r = failing_at(*id);
            assert_eq!(r.status(*id), Status::Failed, "{id:?}");
            assert_eq!(r.failed_at(), Some(*id));
            for before in &RootStep::ALL[..i] {
                assert_eq!(r.status(*before), Status::Done, "{before:?} before {id:?}");
            }
            for after in &RootStep::ALL[i + 1..5] {
                assert_eq!(r.status(*after), Status::Pending, "{after:?} after {id:?} never ran");
            }
            let o = r.outcome.clone().unwrap();
            assert_eq!(o.status, Status::Failed);
            assert!(o.message.contains(id.doing()) && o.message.contains("it broke"), "{}", o.message);
            let nothing = o.message.contains("Nothing was changed");
            assert_eq!(nothing, *id == RootStep::Source, "{id:?}: {}", o.message);
            match id {
                RootStep::Source => {}
                RootStep::Remove => assert!(o.message.contains("may be removed already"), "{}", o.message),
                _ => assert!(o.message.contains("already removed"), "{id:?}: {}", o.message),
            }
        }
    }

    #[test]
    fn a_run_killed_mid_section_fails_that_section() {
        let mut r = SetupRun::new();
        feed_all(&mut r, &["::step source start", "::step source done ok", "::step remove start", "[ OK ]  Removed x"]);
        r.finish(None, true);
        assert_eq!(r.status(RootStep::Remove), Status::Failed);
        assert!(r.outcome.unwrap().message.contains("removing the earlier install"));
    }

    /// The wizard after `run`, as App::sync_install_steps leaves it.
    fn wizard(run: &SetupRun) -> crate::steps::Steps {
        let mut steps = crate::steps::Steps::new(StepId::Review);
        steps.set(StepId::Find, Status::Done);
        for (step, status) in step_statuses(run, false) {
            steps.set(step, status);
        }
        steps
    }

    #[test]
    fn the_wizard_follows_the_markers() {
        let mut r = SetupRun::new();
        let s = wizard(&r);
        assert_eq!(s.status(StepId::Review), Status::Running, "waiting for the password");
        assert_eq!(s.root_blocker(StepId::Import), Some(StepId::Review));
        let lines = good_run();
        let mut fed = 0;
        let mut feed_until = |r: &mut SetupRun, marker: &str| {
            while fed < lines.len() {
                r.feed(&lines[fed]);
                fed += 1;
                if lines[fed - 1] == marker {
                    break;
                }
            }
        };
        feed_until(&mut r, "::step remove start");
        let s = wizard(&r);
        assert_eq!(s.status(StepId::Review), Status::Done);
        assert_eq!(s.status(StepId::Remove), Status::Running);
        assert_eq!(s.blocker(StepId::Import), Some(StepId::Remove), "nothing goes in before the removal ends");
        assert_eq!(s.root_blocker(StepId::Console), Some(StepId::Remove));
        feed_until(&mut r, "::step firmware start");
        let s = wizard(&r);
        assert_eq!((s.status(StepId::Remove), s.status(StepId::Import)), (Status::Done, Status::Done));
        assert_eq!(s.status(StepId::Firmware), Status::Running);
        assert_eq!(s.root_blocker(StepId::Driver), Some(StepId::Firmware));
        feed_until(&mut r, "");
        r.finish(Some(0), true);
        let s = wizard(&r);
        for step in [StepId::Review, StepId::Remove, StepId::Import, StepId::Firmware, StepId::Driver] {
            assert_eq!(s.status(step), Status::Done, "{step:?}");
        }
        assert_eq!(s.blocker(StepId::Console), None, "the Console can be built now");
    }

    #[test]
    fn the_wizard_after_a_cancelled_password_dialog() {
        let mut r = SetupRun::new();
        r.finish(Some(126), true);
        let s = wizard(&r);
        assert_eq!(s.status(StepId::Review), Status::Failed);
        for step in [StepId::Remove, StepId::Import, StepId::Firmware, StepId::Driver] {
            assert_eq!(s.status(step), Status::Pending, "{step:?}");
        }
        assert_eq!(s.root_blocker(StepId::Console), Some(StepId::Review));
    }

    #[test]
    fn the_wizard_after_a_failure_in_each_step() {
        let pages = [StepId::Review, StepId::Remove, StepId::Import, StepId::Firmware, StepId::Driver];
        for (i, id) in RootStep::ALL[..5].iter().enumerate() {
            let s = wizard(&failing_at(*id));
            assert_eq!(s.status(pages[i]), Status::Failed, "{id:?}");
            for later in &pages[i + 1..] {
                assert_eq!(s.status(*later), Status::Pending, "{later:?} after {id:?}");
                assert_eq!(s.root_blocker(*later), Some(pages[i]), "{later:?} is blocked by the failure");
            }
            assert_eq!(s.root_blocker(StepId::Console), Some(pages[i]), "no Console after a failed {id:?}");
        }
    }

    #[test]
    fn the_wizard_with_the_current_install_kept() {
        let mut r = SetupRun::new();
        feed_all(
            &mut r,
            &["::step source start", "::step source done ok", "::step remove skipped Kept", "::step import start"],
        );
        let s = wizard(&r);
        assert_eq!(s.status(StepId::Remove), Status::Skipped);
        assert_eq!(s.blocker(StepId::Import), None, "keeping the install counts as done with it");
        assert_eq!(s.status(StepId::Import), Status::Running);
    }

    #[test]
    fn kept_install_and_warnings() {
        let mut r = SetupRun::new();
        feed_all(
            &mut r,
            &[
                "::step source start",
                "::step source done ok",
                "::step remove skipped Kept the current install (--keep-current)",
                "::step import start",
                "::step import warn Console art imported; plug-in art failed",
                "::step firmware start",
                "::step firmware done x",
                "::step driver start",
                "::step driver done x",
                "::step unmount start",
                "::step unmount done Closed /dev/sda3",
            ],
        );
        r.finish(Some(0), true);
        assert_eq!(r.status(RootStep::Remove), Status::Skipped);
        assert_eq!(r.status(RootStep::Import), Status::Warning);
        assert_eq!(r.status(RootStep::Unmount), Status::Done);
        assert!(!r.nothing_changed());
        assert_eq!(r.outcome.unwrap().status, Status::Warning);
    }

    #[test]
    fn firmware_files_judge_the_firmware_section() {
        let mut r = SetupRun::new();
        feed_all(
            &mut r,
            &[
                "::step firmware start",
                "dsp/0c2.bill     ok       sha256 aa  (x)",
                "ApolloTwin.bin   missing  not in this install",
                "::step firmware done x",
            ],
        );
        assert_eq!(r.firmware_status(false), Status::Done, "no USB Apollo needs the image");
        assert_eq!(r.firmware_status(true), Status::Warning);
    }

    #[test]
    fn builds_the_command() {
        let plan = Plan {
            user: "me".into(),
            source: Source::Folder("/run/media/me/OS Disk".into()),
            oa_assets: Some("/repo/console-rs/target/release/oa-assets".into()),
            data: None,
            keep_current: false,
            send_report: true,
            plugin_hosting: false,
            panels: None,
            panels_sha256: None,
        };
        assert_eq!(
            plan.command_line("pkexec", Path::new("/repo/scripts/setup-root.sh")),
            "pkexec bash /repo/scripts/setup-root.sh --user me --from '/run/media/me/OS Disk' \
             --oa-assets /repo/console-rs/target/release/oa-assets --send-report"
        );
        let plan = Plan {
            source: Source::Partition("/dev/nvme0n1p3".into()),
            oa_assets: None,
            data: Some("/tmp/d".into()),
            keep_current: true,
            send_report: false,
            ..plan
        };
        let args: Vec<String> = plan.args().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["--user", "me", "--mount", "/dev/nvme0n1p3", "--data", "/tmp/d", "--keep-current"]);
        let plan = Plan { panels: Some("/tmp/p.tar.gz".into()), panels_sha256: Some("ab12".into()), ..plan };
        let args: Vec<String> = plan.args().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args[args.len() - 4..], ["--panels", "/tmp/p.tar.gz", "--panels-sha256", "ab12"]);
        let plan = Plan { plugin_hosting: true, panels: None, panels_sha256: None, ..plan };
        let args: Vec<String> = plan.args().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args.last().map(String::as_str), Some("--plugin-hosting"));
    }

    #[test]
    fn an_install_needs_the_console_and_the_dsp_sources() {
        use oa_assets::detect::Console;
        use oa_assets::discover::{Root, Source as RootSource};
        let mut i = Install {
            root: Root { path: "/w".into(), source: RootSource::Given },
            console: None,
            uad_version: None,
            mixer_engine: None,
            powered_plugins: vec!["/w/Program Files/Universal Audio/Powered Plugins/UAD 1176.dll".into()],
            vst3: Vec::new(),
            drivers: Vec::new(),
            sdk_dll: None,
            firmware: Vec::new(),
            plugin_icons: None,
        };
        assert_eq!(missing_parts(&i), ["UAD Console", "Console strip plug-ins or UAD2SDK.dll"]);
        i.console = Some(Console { dir: "/w/c".into(), resources: "/w/c/Resources".into(), version: None });
        i.powered_plugins.push("/w/Program Files/Universal Audio/Powered Plugins/uad input strip header.dll".into());
        assert!(missing_parts(&i).is_empty(), "the strip plug-in's name matches regardless of case");
        i.powered_plugins.truncate(1);
        i.sdk_dll = Some("/w/Windows/System32/UAD2SDK.dll".into());
        assert!(missing_parts(&i).is_empty(), "UAD2SDK.dll is enough");
    }

    #[test]
    fn reads_and_groups_the_earlier_install() {
        let list = parse_list(
            "dkms\tua_apollo 0.1.0 (DKMS)\ndkms\tua_apollo 0.2.0 (DKMS)\nmodule\tthe ua_apollo driver is loaded\n\
             garbage line\nconsole\t/home/me/.local/bin/uaconsole\nnew-kind\tx\n\n",
        );
        assert_eq!(list.len(), 5);
        let groups = group(&list);
        let labels: Vec<&str> = groups.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Driver (DKMS)", "Loaded now", "Console (uaconsole)", "Other"]);
        assert_eq!(groups[0].1, ["ua_apollo 0.1.0 (DKMS)", "ua_apollo 0.2.0 (DKMS)"]);
        assert!(parse_list("").is_empty());
    }
}
