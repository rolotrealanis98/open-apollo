//! Wizard state, background work, and the actions pages ask for.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use oa_assets::discover::{self, Attached, Root};

use crate::firmware::{self, Extractor, FileLine};
use crate::importer::{self, Item, State as ImportState};
use crate::installlog::{INSTALL_SH, INSTALL_UACONSOLE_SH, Outcome, Phase, Progress};
use crate::jobs::{Job, JobMsg, Waker};
use crate::options::{self, Options};
use crate::scan::{self, Found, Scan};
use crate::steps::{Status, StepId, Steps};
use crate::syscheck;

pub(super) enum Msg {
    System(syscheck::Report),
    Scanned(Scan),
    Mounted { device: String, result: Result<PathBuf, String> },
    Attached { image: PathBuf, result: Result<Attached, String> },
    PickedImage(Option<PathBuf>),
    PickedFolder(Option<PathBuf>),
    Import(importer::Msg),
    Released { index: usize, result: Result<(), String> },
    ExtractorProbe(bool),
}

/// Something this session mounted or attached, which it may release again.
#[derive(Clone, Debug)]
pub(super) enum Mounted {
    Partition { device: String, at: PathBuf },
    Image { image: PathBuf, loop_device: String },
}

pub(super) struct MountedEntry {
    pub what: Mounted,
    /// `None` while still mounted; the release result once released.
    pub released: Option<Result<(), String>>,
}

/// A running or finished script (or its `--demo` replay).
pub(super) struct Run {
    pub progress: Progress,
    job: Job,
    pkexec: bool,
    pub demo: bool,
    /// What runs, for the log and for running it by hand.
    pub command: String,
    /// The firmware extractor's per-file lines.
    pub files: Vec<FileLine>,
}

impl Run {
    fn new(plan: &[Phase], job: Job, pkexec: bool, demo: bool, command: String) -> Self {
        Run { progress: Progress::new(plan), job, pkexec, demo, command, files: Vec::new() }
    }

    pub fn running(&self) -> bool {
        self.progress.is_running()
    }

    /// Feed new output; `Some(outcome)` once it ends.
    fn pump(&mut self) -> Option<Outcome> {
        let mut ended = None;
        for m in self.job.poll() {
            match m {
                JobMsg::Line(l) => {
                    if let Some(f) = firmware::parse_file_line(&l) {
                        self.files.push(f);
                    }
                    self.progress.feed(&l);
                }
                JobMsg::Exit(code) => {
                    self.progress.finish(code, self.pkexec);
                    ended = Some(code);
                }
                JobMsg::SpawnError(e) => {
                    self.progress.finish(None, false);
                    self.progress.outcome =
                        Some(Outcome { status: Status::Failed, message: format!("Couldn't start it: {e}") });
                    ended = Some(None);
                }
            }
        }
        ended.and(self.progress.outcome.clone())
    }

    pub fn exit_ok(&self) -> bool {
        self.progress.outcome.as_ref().is_some_and(|o| o.status != Status::Failed)
    }
}

/// What a page asked for; applied after drawing.
#[derive(Clone, Debug)]
pub(super) enum Action {
    Next,
    Back,
    Goto(StepId),
    RunSystem,
    Rescan,
    Select(PathBuf),
    Mount(String),
    AttachImage(PathBuf),
    PickImage,
    PickFolder,
    SkipFind,
    StartImport,
    UsePreviousImport,
    SkipImport,
    StartFirmware,
    KeepFirmware,
    SkipFirmware,
    StartDriver,
    SkipDriver,
    SendReport(bool),
    StartConsole,
    SkipConsole,
    Launch,
    Release(usize),
    Close,
}

pub struct App {
    pub(super) opts: Options,
    pub(super) steps: Steps,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    wake: Waker,
    entered: Option<StepId>,
    // System check
    pub(super) sys: Option<syscheck::Report>,
    // Find
    pub(super) scan: Option<Scan>,
    pub(super) scanning: bool,
    extra_roots: Vec<Root>,
    pub(super) selected: Option<PathBuf>,
    /// Select this root after the next scan (just opened or picked).
    prefer: Option<PathBuf>,
    pub(super) busy: Option<String>,
    pub(super) find_note: Option<(Status, String)>,
    pub(super) mounted: Vec<MountedEntry>,
    // Import
    pub(super) import: Vec<(Item, ImportState)>,
    pub(super) importing: bool,
    pub(super) used_previous_import: bool,
    // DSP programs & firmware
    pub(super) firmware: Option<Run>,
    pub(super) extractor: Option<Extractor>,
    pub(super) extractor_ok: Option<bool>,
    pub(super) kept_firmware: bool,
    // Driver
    pub(super) driver: Option<Run>,
    /// Pass --send-report: upload install.sh's report (opt-in, off by default).
    pub(super) send_report: bool,
    // Console
    pub(super) console: Option<Run>,
    pub(super) console_bin: Option<PathBuf>,
    pub(super) launched: Option<Result<String, String>>,
    // --auto
    auto_until: Option<StepId>,
    auto_since: Instant,
    auto_kicked: bool,
    pub(super) confirm_close: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        super::theme::apply(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let wake: Waker = Arc::new(move || ctx.request_repaint());
        let (tx, rx) = channel();
        let mut app = App {
            steps: Steps::new(opts.start),
            tx,
            rx,
            wake,
            entered: None,
            sys: None,
            scan: None,
            scanning: false,
            extra_roots: Vec::new(),
            selected: None,
            prefer: None,
            busy: None,
            find_note: None,
            mounted: Vec::new(),
            import: importer::plan(None),
            importing: false,
            used_previous_import: false,
            firmware: None,
            extractor: Extractor::locate(),
            extractor_ok: None,
            kept_firmware: false,
            driver: None,
            send_report: false,
            console: None,
            console_bin: options::installed_console(),
            launched: None,
            auto_until: opts.auto_until,
            auto_since: Instant::now(),
            auto_kicked: false,
            confirm_close: false,
            opts,
        };
        app.start_system();
        if app.opts.start >= StepId::Find {
            app.start_scan();
        }
        if let Some(x) = app.extractor.clone().filter(|_| !app.opts.demo) {
            app.background(move || Msg::ExtractorProbe(x.supports_firmware()));
        }
        app
    }

    fn background(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        thread::spawn(move || {
            let _ = tx.send(f());
            wake();
        });
    }

    pub(super) fn selected_found(&self) -> Option<&Found> {
        let sel = self.selected.as_ref()?;
        self.scan.as_ref()?.found.iter().find(|f| f.path() == sel)
    }

    /// The Console art is in place: imported now or earlier.
    pub(super) fn console_art(&self) -> Option<(Option<String>, u64)> {
        importer::previous(&self.opts.data)
    }

    /// An import or extraction is reading the selected install, so it can't change.
    pub(super) fn selection_locked(&self) -> bool {
        self.importing || self.firmware.as_ref().is_some_and(Run::running)
    }

    pub(super) fn privileged_job_running(&self) -> bool {
        [&self.firmware, &self.driver].into_iter().flatten().any(|r| r.running() && !r.demo)
    }

    // ---- per-frame ----

    pub(super) fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        if let Some(o) = self.firmware.as_mut().and_then(Run::pump) {
            // Without a USB Apollo (system check) nothing needs its boot image;
            // before the check has run, assume one might be there.
            let usb_apollo = self.sys.as_ref().is_none_or(|s| !s.apollo.usb.is_empty());
            let fw = self.firmware.as_mut().expect("pumped");
            let status = if o.status == Status::Failed {
                Status::Failed
            } else {
                firmware::outcome(Some(0), &fw.files, usb_apollo)
            };
            if status != o.status && status != Status::Failed {
                let skipped_images = fw.files.iter().any(|f| firmware::not_needed(f, usb_apollo));
                fw.progress.outcome = Some(Outcome {
                    status,
                    message: match status {
                        Status::Done if skipped_images => {
                            "Extracted everything this computer needs (the USB boot images are only for USB Apollos)."
                                .into()
                        }
                        Status::Done => "All files extracted.".into(),
                        _ => "Finished, but some files are missing: see the list.".into(),
                    },
                });
            }
            self.steps.set(StepId::Firmware, status);
        }
        if let Some(o) = self.driver.as_mut().and_then(Run::pump) {
            self.steps.set(StepId::Driver, o.status);
            self.sys_refresh_driver();
        }
        if let Some(o) = self.console.as_mut().and_then(Run::pump) {
            self.steps.set(StepId::Console, if o.status == Status::Failed { Status::Failed } else { o.status });
            if !self.opts.demo {
                self.console_bin = options::installed_console();
            }
        }
        let cur = self.steps.current();
        if self.entered != Some(cur) {
            self.entered = Some(cur);
            self.on_enter(cur);
        }
        self.auto_tick(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.privileged_job_running() && !self.confirm_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
    }

    fn sys_refresh_driver(&mut self) {
        if self.opts.demo {
            return;
        }
        let repo = self.opts.repo.clone();
        self.background(move || Msg::System(syscheck::run(repo.as_deref())));
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::System(r) => {
                if self.steps.status(StepId::System) == Status::Running || self.sys.is_none() {
                    self.steps.set(StepId::System, if r.warnings() == 0 { Status::Done } else { Status::Warning });
                }
                self.sys = Some(r);
            }
            Msg::Scanned(s) => {
                self.scanning = false;
                let usable = |p: &PathBuf| s.found.iter().any(|f| f.path() == p && f.install.found_anything());
                let candidates = match self.selection_locked() {
                    // An import or extraction is reading the current install: keep it.
                    true => [None, self.selected.clone()],
                    false => [self.prefer.take(), self.selected.clone()],
                };
                let pick = candidates
                    .into_iter()
                    .flatten()
                    .find(usable)
                    .or_else(|| s.best().map(|i| s.found[i].path().to_path_buf()));
                self.scan = Some(s);
                self.set_selection(pick);
            }
            Msg::Mounted { device, result } => {
                self.busy = None;
                match result {
                    Ok(at) => {
                        self.find_note =
                            Some((Status::Done, format!("Opened {device} read-only at {}.", at.display())));
                        self.mounted
                            .push(MountedEntry { what: Mounted::Partition { device, at: at.clone() }, released: None });
                        self.prefer = Some(at);
                        self.start_scan();
                    }
                    Err(e) => self.find_note = Some((Status::Failed, format!("Couldn't open {device}: {e}"))),
                }
            }
            Msg::Attached { image, result } => {
                self.busy = None;
                match result {
                    Ok(a) => {
                        let n = a.roots.len();
                        self.find_note = Some(if n == 0 {
                            (
                                Status::Warning,
                                format!(
                                    "Opened {} as {}, but it has no Windows partition.",
                                    image.display(),
                                    a.loop_device
                                ),
                            )
                        } else {
                            (Status::Done, format!("Opened {} read-only as {}.", image.display(), a.loop_device))
                        });
                        self.prefer = a.roots.first().map(|r| r.path.clone());
                        self.mounted.push(MountedEntry {
                            what: Mounted::Image { image, loop_device: a.loop_device },
                            released: None,
                        });
                        self.start_scan();
                    }
                    Err(e) => {
                        self.find_note = Some((Status::Failed, format!("Couldn't open {}: {e}", image.display())))
                    }
                }
            }
            Msg::PickedImage(p) => {
                self.busy = None;
                if let Some(p) = p {
                    self.attach(p);
                }
            }
            Msg::PickedFolder(p) => {
                self.busy = None;
                if let Some(p) = p {
                    let root = discover::given(&p);
                    self.prefer = Some(root.path.clone());
                    if !self.extra_roots.iter().any(|r| r.path == root.path) {
                        self.extra_roots.push(root);
                    }
                    self.start_scan();
                }
            }
            Msg::Import(importer::Msg::State(item, state)) => {
                if let Some(slot) = self.import.iter_mut().find(|(i, _)| *i == item) {
                    slot.1 = state;
                }
            }
            Msg::Import(importer::Msg::Finished) => {
                self.importing = false;
                let art = self.import.iter().find(|(i, _)| *i == Item::ConsoleArt).map(|(_, s)| s.clone());
                self.steps.set(
                    StepId::Import,
                    match art {
                        Some(ImportState::Done { .. }) => Status::Done,
                        Some(ImportState::Failed(_)) => Status::Failed,
                        _ => Status::Warning,
                    },
                );
            }
            Msg::Released { index, result } => {
                if let Some(e) = self.mounted.get_mut(index) {
                    e.released = Some(result);
                }
            }
            Msg::ExtractorProbe(ok) => self.extractor_ok = Some(ok),
        }
    }

    fn on_enter(&mut self, step: StepId) {
        self.auto_since = Instant::now();
        self.auto_kicked = false;
        match step {
            StepId::Find if self.scan.is_none() && !self.scanning => self.start_scan(),
            StepId::Done => self.steps.set(StepId::Done, Status::Done),
            _ => {}
        }
    }

    /// `--auto`: start each step's action and move on once it has ended.
    fn auto_tick(&mut self, ctx: &egui::Context) {
        let Some(until) = self.auto_until else { return };
        let cur = self.steps.current();
        if !self.auto_kicked && self.auto_since.elapsed() > Duration::from_millis(700) {
            let ready = match cur {
                StepId::Import => self.selected_found().is_some() && !self.scanning,
                _ => true,
            };
            if ready {
                self.auto_kicked = true;
                self.primary(cur);
                if cur == until {
                    // Started the last step's action; the rest is up to the user.
                    self.auto_until = None;
                    return;
                }
            }
        }
        if cur != until && self.auto_since.elapsed() > Duration::from_millis(1600) && self.steps.can_advance() {
            self.apply(Action::Next, ctx);
        }
        ctx.request_repaint_after(Duration::from_millis(150));
    }

    /// The main thing a step does, for Enter and `--auto`.
    pub(super) fn primary(&mut self, step: StepId) {
        let pending = self.steps.status(step) == Status::Pending;
        let unblocked = self.steps.blocker(step).is_none();
        match step {
            StepId::Welcome => self.steps.set(StepId::Welcome, Status::Done),
            StepId::Import if pending && unblocked && !self.importing => self.start_import(),
            StepId::Firmware if pending && unblocked => self.start_firmware(),
            StepId::Driver if pending && unblocked => self.start_driver(),
            StepId::Console if pending => {
                if self.console_bin.is_some() && !self.opts.demo {
                    self.launch()
                } else {
                    self.start_console()
                }
            }
            _ => {}
        }
    }

    pub(super) fn apply(&mut self, a: Action, ctx: &egui::Context) {
        match a {
            Action::Next => {
                if self.steps.current() == StepId::Welcome {
                    self.steps.set(StepId::Welcome, Status::Done);
                }
                if self.steps.current() == StepId::Done {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.steps.advance();
                }
            }
            Action::Back => {
                self.steps.back();
            }
            Action::Goto(s) => {
                self.steps.goto(s);
            }
            Action::RunSystem => self.start_system(),
            Action::Rescan => self.start_scan(),
            Action::Select(p) if !self.selection_locked() => self.set_selection(Some(p)),
            Action::Select(_) => {}
            Action::Mount(device) => {
                self.busy = Some(format!("Opening {device} read-only (your password may be asked)…"));
                self.find_note = None;
                self.background(move || {
                    let result = discover::mount_readonly(&device);
                    Msg::Mounted { device, result }
                });
            }
            Action::AttachImage(p) => self.attach(p),
            Action::PickImage => {
                self.busy = Some("Choose the disk image in the file dialog…".into());
                let start = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".windows")).filter(|p| p.is_dir());
                self.background(move || {
                    let mut d = rfd::FileDialog::new()
                        .set_title("Choose the Windows VM's disk image")
                        .add_filter("Disk images", &["img", "raw", "qcow2", "vhdx", "vhd", "vdi", "vmdk"])
                        .add_filter("All files", &["*"]);
                    if let Some(s) = start {
                        d = d.set_directory(s);
                    }
                    Msg::PickedImage(d.pick_file())
                });
            }
            Action::PickFolder => {
                self.busy = Some("Choose the folder in the file dialog…".into());
                self.background(|| {
                    Msg::PickedFolder(
                        rfd::FileDialog::new()
                            .set_title("Choose the folder that holds Windows' C:\\ drive, or a Wine prefix")
                            .pick_folder(),
                    )
                });
            }
            Action::SkipFind => {
                self.steps.set(StepId::Find, Status::Skipped);
                self.steps.invalidate_dependents(StepId::Find);
            }
            Action::StartImport => self.start_import(),
            Action::UsePreviousImport => {
                self.used_previous_import = true;
                self.steps.set(StepId::Import, Status::Done);
            }
            Action::SkipImport => self.steps.set(StepId::Import, Status::Skipped),
            Action::StartFirmware => self.start_firmware(),
            Action::KeepFirmware => {
                self.kept_firmware = true;
                self.steps.set(StepId::Firmware, Status::Done);
            }
            Action::SkipFirmware => self.steps.set(StepId::Firmware, Status::Skipped),
            Action::StartDriver => self.start_driver(),
            Action::SkipDriver => self.steps.set(StepId::Driver, Status::Skipped),
            Action::SendReport(on) => self.send_report = on,
            Action::StartConsole => self.start_console(),
            Action::SkipConsole => self.steps.set(StepId::Console, Status::Skipped),
            Action::Launch => self.launch(),
            Action::Release(i) => self.release(i),
            Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    // ---- actions ----

    fn start_system(&mut self) {
        self.steps.set(StepId::System, Status::Running);
        let repo = self.opts.repo.clone();
        self.background(move || Msg::System(syscheck::run(repo.as_deref())));
    }

    fn start_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.steps.set(StepId::Find, Status::Running);
        let extra = self.extra_roots.clone();
        self.background(move || Msg::Scanned(scan::scan(&extra)));
    }

    fn set_selection(&mut self, pick: Option<PathBuf>) {
        if pick != self.selected {
            self.steps.invalidate_dependents(StepId::Find);
            self.firmware = None;
            self.kept_firmware = false;
            self.used_previous_import = false;
        }
        self.selected = pick;
        self.import = importer::plan(self.selected_found().map(|f| &f.install));
        if self.scanning {
            return;
        }
        let status = match self.selected_found() {
            Some(f) if f.has_console() => Status::Done,
            // Nothing found (yet) is amber: later steps show what's missing.
            Some(_) | None => Status::Warning,
        };
        self.steps.set(StepId::Find, status);
    }

    fn attach(&mut self, image: PathBuf) {
        // Never take over an image something else attached: just show what's on it.
        if let Some(dev) = discover::existing_loop(&image) {
            self.find_note = Some((
                Status::Done,
                format!("{} is already open as {dev}; its Windows partition is listed here.", image.display()),
            ));
            self.start_scan();
            return;
        }
        self.busy = Some(format!("Opening {} read-only (your password may be asked)…", image.display()));
        self.find_note = None;
        self.background(move || {
            let result = discover::attach_image(&image);
            Msg::Attached { image, result }
        });
    }

    fn start_import(&mut self) {
        let Some(found) = self.selected_found() else { return };
        let install = found.install.clone();
        self.import = importer::plan(Some(&install));
        let items: Vec<Item> =
            self.import.iter().filter(|(_, s)| *s == ImportState::Waiting).map(|(i, _)| *i).collect();
        self.importing = true;
        self.used_previous_import = false;
        self.steps.set(StepId::Import, Status::Running);
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        let (itx, irx) = channel();
        importer::run(install, self.opts.data.clone(), items, itx, wake.clone());
        // Forward the importer's messages into the app's channel.
        thread::spawn(move || {
            for m in irx {
                if tx.send(Msg::Import(m)).is_err() {
                    return;
                }
                wake();
            }
        });
    }

    fn start_firmware(&mut self) {
        let Some(found) = self.selected_found() else { return };
        let root = found.path().to_path_buf();
        let out = Path::new(firmware::FIRMWARE_DIR);
        let run = if self.opts.demo {
            let i = &found.install;
            let script = crate::demo::firmware(&root, i.powered_plugins.len(), firmware::FIRMWARE_DIR);
            let command = format!(
                "pkexec oa-assets firmware --from {} --out {}",
                firmware::shell_quote(&root.to_string_lossy()),
                firmware::FIRMWARE_DIR
            );
            Run::new(&[], Job::replay(script, 0, self.wake.clone()), true, true, command)
        } else {
            let Some(x) = self.extractor.clone().filter(|_| self.extractor_ok == Some(true)) else { return };
            let mut cmd = Command::new("pkexec");
            cmd.arg(&x.program).args(x.args(&root, out));
            Run::new(&[], Job::spawn(cmd, self.wake.clone()), true, false, x.command_line(&root, out))
        };
        self.firmware = Some(run);
        self.kept_firmware = false;
        self.steps.set(StepId::Firmware, Status::Running);
    }

    pub(super) fn driver_command(&self) -> Option<(Command, String, String)> {
        let script = self.opts.repo.as_ref()?.join("scripts/install.sh");
        let bash = ["/usr/bin/bash", "/bin/bash"].into_iter().find(|b| Path::new(b).is_file()).unwrap_or("bash");
        let mut cmd = Command::new("pkexec");
        cmd.arg(bash).arg(&script).arg("--yes");
        let send = if self.send_report { " --send-report" } else { "" };
        if self.send_report {
            cmd.arg("--send-report");
        }
        let q = firmware::shell_quote(&script.to_string_lossy());
        Some((cmd, format!("pkexec bash {q} --yes{send}"), format!("sudo bash {q}{send}")))
    }

    fn start_driver(&mut self) {
        let run = if self.opts.demo {
            let (distro, kernel) = self.sys.as_ref().map(|s| (s.distro.clone(), s.kernel.clone())).unwrap_or_default();
            let script = crate::demo::driver(&distro, &kernel, self.send_report);
            let send = if self.send_report { " --send-report" } else { "" };
            Run::new(
                INSTALL_SH,
                Job::replay(script, 0, self.wake.clone()),
                true,
                true,
                format!("pkexec bash scripts/install.sh --yes{send}"),
            )
        } else {
            let Some((cmd, shown, _)) = self.driver_command() else { return };
            Run::new(INSTALL_SH, Job::spawn(cmd, self.wake.clone()), true, false, shown)
        };
        self.driver = Some(run);
        self.steps.set(StepId::Driver, Status::Running);
    }

    fn start_console(&mut self) {
        let run = if self.opts.demo {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let res = self.opts.data.join(oa_assets::import::CONSOLE_DIR);
            Run::new(
                INSTALL_UACONSOLE_SH,
                Job::replay(crate::demo::console(&home, &res), 0, self.wake.clone()),
                false,
                true,
                "bash scripts/install-uaconsole.sh".into(),
            )
        } else {
            let Some(repo) = self.opts.repo.clone() else { return };
            let script = repo.join("scripts/install-uaconsole.sh");
            let mut cmd = Command::new("bash");
            cmd.arg(&script);
            Run::new(
                INSTALL_UACONSOLE_SH,
                Job::spawn(cmd, self.wake.clone()),
                false,
                false,
                format!("bash {}", firmware::shell_quote(&script.to_string_lossy())),
            )
        };
        self.console = Some(run);
        self.launched = None;
        self.steps.set(StepId::Console, Status::Running);
    }

    fn launch(&mut self) {
        if self.opts.demo {
            self.launched = Some(Ok("Demo: the Console would start now (uaconsole).".into()));
            self.steps.set(StepId::Console, Status::Done);
            return;
        }
        let Some(bin) = self.console_bin.clone() else { return };
        let mut cmd = Command::new(&bin);
        if !self.opts.data_is_default {
            cmd.env("CONSOLE_RESOURCES", self.opts.data.join(oa_assets::import::CONSOLE_DIR));
        }
        // Its own process group, so it outlives the wizard and its terminal.
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
        self.launched = Some(match cmd.spawn() {
            Ok(mut child) => {
                thread::spawn(move || child.wait());
                self.steps.set(StepId::Console, Status::Done);
                Ok(format!("Started {}.", bin.display()))
            }
            Err(e) => Err(format!("Couldn't start {}: {e}", bin.display())),
        });
    }

    fn release(&mut self, index: usize) {
        let Some(entry) = self.mounted.get(index) else { return };
        let what = entry.what.clone();
        self.background(move || {
            let result = match what {
                Mounted::Partition { device, .. } => Command::new("udisksctl")
                    .args(["unmount", "--block-device", &device])
                    .output()
                    .map_err(|e| e.to_string())
                    .and_then(|o| {
                        if o.status.success() {
                            Ok(())
                        } else {
                            Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
                        }
                    }),
                Mounted::Image { loop_device, .. } => discover::detach(&loop_device),
            };
            Msg::Released { index, result }
        });
    }
}
