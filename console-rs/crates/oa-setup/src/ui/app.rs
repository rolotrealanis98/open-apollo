//! Wizard state, background work, and the actions pages ask for.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use oa_assets::discover::{self, Attached, Root};

use crate::firmware::{self, Extractor};
use crate::importer;
use crate::installlog::{INSTALL_UACONSOLE_SH, Line, Outcome, Progress};
use crate::jobs::{Job, JobMsg, Waker};
use crate::options::{self, Options};
use crate::runlog::{self, Redactor, RunLog};
use crate::scan::{self, Found, Origin, Scan};
use crate::setuprun::{self, Leftover, Plan, RootStep, SetupRun, Source};
use crate::steps::{Status, StepId, Steps};
use crate::syscheck;

pub(super) enum Msg {
    System(syscheck::Report),
    Scanned(Scan),
    Attached { image: PathBuf, result: Result<Attached, String> },
    PickedImage(Option<PathBuf>),
    PickedFolder(Option<PathBuf>),
    Released { index: usize, result: Result<(), String> },
    ExtractorProbe(bool),
    Earlier(Result<Vec<Leftover>, String>),
    DaemonRestarted(Result<(), String>),
}

/// A VM disk image this session attached (read-only, through udisks), which it may detach again.
pub(super) struct AttachedImage {
    pub image: PathBuf,
    pub loop_device: String,
    /// `None` while attached; the detach result once detached.
    pub released: Option<Result<(), String>>,
}

/// A running or finished user-level script (the Console build) or its `--demo` replay.
pub(super) struct Run {
    pub progress: Progress,
    job: Job,
    pub demo: bool,
    /// What runs, for the log and for running it by hand.
    pub command: String,
    /// The setup log it appends to.
    pub log: Option<PathBuf>,
}

impl Run {
    pub fn running(&self) -> bool {
        self.progress.is_running()
    }

    /// Feed new output; `Some(outcome)` once it ends. A `--demo` replay's
    /// lines go to `log` (a real script writes its own).
    fn pump(&mut self, mut log: Option<&mut RunLog>) -> Option<Outcome> {
        let mut ended = false;
        for m in self.job.poll() {
            match m {
                JobMsg::Line(l) => {
                    if self.demo
                        && let Some(log) = log.as_deref_mut()
                    {
                        log.line(&l);
                    }
                    self.progress.feed(&l)
                }
                JobMsg::Exit(code) => {
                    self.progress.finish(code, false);
                    ended = true;
                }
                JobMsg::SpawnError(e) => {
                    self.progress.finish(None, false);
                    self.progress.outcome =
                        Some(Outcome { status: Status::Failed, message: format!("Couldn't start it: {e}") });
                    ended = true;
                }
            }
        }
        if ended { self.progress.outcome.clone() } else { None }
    }

    pub fn exit_ok(&self) -> bool {
        self.progress.outcome.as_ref().is_some_and(|o| o.status != Status::Failed)
    }
}

/// The install: scripts/setup-root.sh through pkexec (or its `--demo` replay).
pub(super) struct Install {
    pub run: SetupRun,
    job: Job,
    pub demo: bool,
    pub plan: Plan,
    /// The command with sudo, for a terminal.
    pub terminal: String,
    /// The setup log (setup-root.sh --log appends to it).
    pub log: Option<PathBuf>,
}

impl Install {
    /// Feed new output: the sections that started, and whether it ended. A
    /// `--demo` replay's lines go to `log` (setup-root.sh writes its own).
    fn pump(&mut self, mut log: Option<&mut RunLog>) -> (Vec<RootStep>, bool) {
        let mut opened = Vec::new();
        let mut ended = false;
        for m in self.job.poll() {
            match m {
                JobMsg::Line(l) => {
                    if self.demo
                        && let Some(log) = log.as_deref_mut()
                    {
                        log.line(&l);
                    }
                    opened.extend(self.run.feed(&l))
                }
                JobMsg::Exit(code) => {
                    self.run.finish(code, !self.demo);
                    ended = true;
                }
                JobMsg::SpawnError(e) => {
                    self.run.other.push(Line::Fail(format!("Couldn't start pkexec: {e}")));
                    self.run.finish(None, false);
                    if let Some(o) = self.run.outcome.as_mut() {
                        o.message = format!("Couldn't start the install ({e}). Nothing was changed.");
                    }
                    ended = true;
                }
            }
        }
        (opened, ended)
    }

    pub fn running(&self) -> bool {
        self.run.running()
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
    SelectDevice(String),
    AttachImage(PathBuf),
    PickImage,
    PickFolder,
    SkipFind,
    CheckEarlier,
    KeepCurrent(bool),
    SendReport(bool),
    PluginReport(bool),
    PluginHosting(bool),
    StartInstall,
    SkipInstall,
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
    /// An unmounted Windows partition picked instead; the install opens it read-only.
    pub(super) selected_device: Option<String>,
    /// Select this root after the next scan (just opened or picked).
    prefer: Option<PathBuf>,
    pub(super) busy: Option<String>,
    pub(super) find_note: Option<(Status, String)>,
    pub(super) attached: Vec<AttachedImage>,
    // Review and install
    /// What an earlier install left (`uninstall.sh --list`), once checked.
    pub(super) earlier: Option<Result<Vec<Leftover>, String>>,
    pub(super) earlier_checking: bool,
    /// "Keep my current install": --keep-current (off by default).
    pub(super) keep_current: bool,
    /// --send-report: upload install.sh's report (opt-in, off by default).
    pub(super) send_report: bool,
    /// With --send-report: include the authorized plug-ins (on by default; off: --no-plugin-report).
    pub(super) plugin_report: bool,
    /// --plugin-hosting: host UA plug-ins on the Apollo's DSP (experimental, off by default).
    pub(super) plugin_hosting: bool,
    pub(super) extractor: Option<Extractor>,
    pub(super) extractor_ok: Option<bool>,
    pub(super) install: Option<Install>,
    /// This run's setup log (~/.local/state/open-apollo/setup-*.log), while it is written.
    run_log: Option<RunLog>,
    /// Move to each section's page as the install reaches it, until the user navigates.
    follow: bool,
    // Console
    pub(super) console: Option<Run>,
    pub(super) console_bin: Option<PathBuf>,
    pub(super) launched: Option<Result<String, String>>,
    /// Restart the mixer daemon once the Console build after an install ends.
    restart_daemon: bool,
    pub(super) daemon: Option<Result<(), String>>,
    // --auto
    auto_until: Option<StepId>,
    /// --auto: the install's progress doesn't move the wizard past this step.
    auto_stop: Option<StepId>,
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
            selected_device: None,
            prefer: None,
            busy: None,
            find_note: None,
            attached: Vec::new(),
            earlier: None,
            earlier_checking: false,
            keep_current: false,
            send_report: false,
            plugin_report: true,
            plugin_hosting: false,
            extractor: Extractor::locate(),
            extractor_ok: None,
            install: None,
            run_log: None,
            follow: false,
            console: None,
            console_bin: options::installed_console(),
            launched: None,
            restart_daemon: false,
            daemon: None,
            auto_until: opts.auto_until,
            auto_stop: opts.auto_until,
            auto_since: Instant::now(),
            auto_kicked: false,
            confirm_close: false,
            opts,
        };
        app.start_system();
        if app.opts.start >= StepId::Find {
            app.start_scan();
        }
        app.start_earlier_check();
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

    pub(super) fn install_running(&self) -> bool {
        self.install.as_ref().is_some_and(Install::running)
    }

    /// The install reads the selected install, so it can't change meanwhile.
    pub(super) fn selection_locked(&self) -> bool {
        self.install_running()
    }

    pub(super) fn privileged_job_running(&self) -> bool {
        self.install.as_ref().is_some_and(|i| i.running() && !i.demo)
    }

    /// The Console may be built: after the install, or when the user skipped it.
    pub(super) fn console_allowed(&self) -> bool {
        self.steps.blocker(StepId::Console).is_none()
    }

    /// What the Install button would run, or why it can't.
    pub(super) fn install_plan(&self) -> Result<Plan, String> {
        if self.install_running() {
            return Err("The install is running.".into());
        }
        let source = if let Some(f) = self.selected_found() {
            let missing = setuprun::missing_parts(&f.install);
            if !missing.is_empty() {
                return Err(format!("This UAD install has no {}.", missing.join(", and no ")));
            }
            Source::Folder(f.path().to_path_buf())
        } else if let Some(d) = &self.selected_device {
            Source::Partition(d.clone())
        } else {
            return Err("No UAD install is picked: go back to Find your UAD install.".into());
        };
        if !self.opts.demo {
            if self.opts.repo.is_none() {
                return Err("Can't find the open-apollo folder: start setup from inside it, or pass --repo DIR.".into());
            }
            match (&self.extractor, self.extractor_ok) {
                (None, _) => {
                    return Err("The oa-assets program wasn't found next to setup or on your PATH. Build it with: cargo build --release -p oa-assets".into());
                }
                (Some(_), None) => return Err("Checking the oa-assets program…".into()),
                (Some(x), Some(false)) => {
                    return Err(format!(
                        "{} has no firmware command; build a newer oa-assets (cargo build --release -p oa-assets).",
                        x.program.display()
                    ));
                }
                _ => {}
            }
        }
        Ok(Plan {
            user: current_user(),
            source,
            oa_assets: self.extractor.as_ref().filter(|_| !self.opts.demo).map(|x| absolute(&x.program)),
            data: (!self.opts.data_is_default).then(|| self.opts.data.clone()),
            keep_current: self.keep_current,
            send_report: self.send_report,
            plugin_report: self.plugin_report,
            plugin_hosting: self.plugin_hosting,
            panels: std::env::var_os("OA_PANELS_TARBALL").filter(|v| !v.is_empty()).map(|p| absolute(Path::new(&p))),
            panels_sha256: std::env::var("OA_PANELS_SHA256").ok().filter(|v| !v.is_empty()),
        })
    }

    pub(super) fn setup_script(&self) -> PathBuf {
        match &self.opts.repo {
            Some(r) => r.join("scripts/setup-root.sh"),
            None => PathBuf::from("scripts/setup-root.sh"),
        }
    }

    // ---- per-frame ----

    pub(super) fn tick(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        if let Some(i) = self.install.as_mut() {
            let running = i.running();
            let (opened, ended) = i.pump(self.run_log.as_mut());
            // Once it has ended, its result stays put (a new pick resets the steps).
            if running {
                self.sync_install_steps();
            }
            if self.follow {
                for page in opened.iter().filter_map(|id| id.page()) {
                    self.follow_to(page);
                }
            }
            if ended {
                self.on_install_end();
            }
        }
        if let Some(o) = self.console.as_mut().and_then(|r| r.pump(self.run_log.as_mut())) {
            self.log_line(&format!("Console build ended: {}", o.message));
            self.steps.set(StepId::Console, o.status);
            if !self.opts.demo {
                self.console_bin = options::installed_console();
            }
            if std::mem::take(&mut self.restart_daemon) {
                self.restart_mixer_daemon();
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

    /// Show `page`, unless --auto stops earlier.
    fn follow_to(&mut self, page: StepId) {
        if self.auto_stop.is_some_and(|stop| page > stop) {
            return;
        }
        self.steps.reach(page);
    }

    /// The wizard's steps follow the install's sections.
    fn sync_install_steps(&mut self) {
        let Some(i) = &self.install else { return };
        // Without a USB Apollo nothing needs its boot image; before the check has run, assume one might be there.
        let usb_apollo = self.sys.as_ref().is_none_or(|s| !s.apollo.usb.is_empty());
        for (step, status) in setuprun::step_statuses(&i.run, usb_apollo) {
            self.steps.set(step, status);
        }
    }

    fn on_install_end(&mut self) {
        let Some(i) = &self.install else { return };
        let ok = i.run.outcome.as_ref().is_some_and(|o| o.status != Status::Failed);
        let ended = i.run.outcome.as_ref().map(|o| o.message.clone()).unwrap_or_default();
        self.log_line(&format!("Setup ended: {ended}"));
        self.sys_refresh();
        if !self.opts.demo {
            self.console_bin = options::installed_console();
        }
        if ok {
            // Then, as the user: build the Console, restart the daemon, offer Launch.
            self.restart_daemon = true;
            self.start_console();
            if self.follow {
                self.follow_to(StepId::Console);
            }
        }
    }

    fn sys_refresh(&mut self) {
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
                // A picked partition that got mounted meanwhile: use its folder.
                let device_now = self.selected_device.as_ref().and_then(|d| {
                    s.found
                        .iter()
                        .find(|f| matches!(&f.origin, Origin::Partition { device } if device == d))
                        .map(|f| f.path().to_path_buf())
                });
                let device_still = self
                    .selected_device
                    .as_ref()
                    .is_some_and(|d| device_now.is_none() && s.unmounted.iter().any(|u| &u.device == d));
                let candidates = match self.selection_locked() {
                    // The install is reading the current install: keep it.
                    true => [None, self.selected.clone()],
                    false => [self.prefer.take().or(device_now), self.selected.clone()],
                };
                let pick = candidates
                    .into_iter()
                    .flatten()
                    .find(usable)
                    .or_else(|| s.best().map(|i| s.found[i].path().to_path_buf()));
                self.scan = Some(s);
                if device_still && !self.selection_locked() {
                    self.update_find_status();
                } else {
                    self.set_selection(pick);
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
                        self.attached.push(AttachedImage { image, loop_device: a.loop_device, released: None });
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
            Msg::Released { index, result } => {
                if let Some(e) = self.attached.get_mut(index) {
                    e.released = Some(result);
                }
            }
            Msg::ExtractorProbe(ok) => self.extractor_ok = Some(ok),
            Msg::Earlier(r) => {
                self.earlier_checking = false;
                self.earlier = Some(r);
            }
            Msg::DaemonRestarted(r) => {
                self.log_line(&match &r {
                    Ok(()) => "Mixer daemon restarted".to_string(),
                    Err(e) => format!("Mixer daemon restart failed: {e}"),
                });
                self.daemon = Some(r)
            }
        }
    }

    fn on_enter(&mut self, step: StepId) {
        self.auto_since = Instant::now();
        self.auto_kicked = false;
        match step {
            StepId::Find if self.scan.is_none() && !self.scanning => self.start_scan(),
            // What's installed may have changed since setup started (not after the install ran).
            StepId::Review if self.install.is_none() && !self.earlier_checking => self.start_earlier_check(),
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
                StepId::Review => self.install_plan().is_ok() && !self.earlier_checking && !self.scanning,
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
        // The install moves the wizard along by itself.
        let busy = self.install_running() || self.console.as_ref().is_some_and(Run::running);
        if cur != until && !busy && self.auto_since.elapsed() > Duration::from_millis(1600) && self.steps.can_advance()
        {
            if cur == StepId::Welcome {
                self.steps.set(StepId::Welcome, Status::Done);
            }
            self.steps.advance();
        }
        ctx.request_repaint_after(Duration::from_millis(150));
    }

    /// The main thing a step does, for Enter and `--auto`.
    pub(super) fn primary(&mut self, step: StepId) {
        let pending = self.steps.status(step) == Status::Pending;
        match step {
            StepId::Welcome => self.steps.set(StepId::Welcome, Status::Done),
            StepId::Review if self.install.is_none() && self.install_plan().is_ok() => self.start_install(),
            StepId::Console
                if pending && self.console_allowed() && !self.console.as_ref().is_some_and(Run::running) =>
            {
                if self.console_bin.is_some() && !self.opts.demo { self.launch() } else { self.start_console() }
            }
            _ => {}
        }
    }

    pub(super) fn apply(&mut self, a: Action, ctx: &egui::Context) {
        match a {
            Action::Next => {
                self.follow = false;
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
                self.follow = false;
                self.steps.back();
            }
            Action::Goto(s) => {
                self.follow = false;
                self.steps.goto(s);
            }
            Action::RunSystem => self.start_system(),
            Action::Rescan => self.start_scan(),
            Action::Select(p) if !self.selection_locked() => self.set_selection(Some(p)),
            Action::SelectDevice(d) if !self.selection_locked() => self.select_device(d),
            Action::Select(_) | Action::SelectDevice(_) => {}
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
            Action::CheckEarlier => self.start_earlier_check(),
            Action::KeepCurrent(on) => {
                if !self.install_running() {
                    self.keep_current = on;
                }
            }
            Action::SendReport(on) => {
                if !self.install_running() {
                    self.send_report = on;
                }
            }
            Action::PluginReport(on) => {
                if !self.install_running() {
                    self.plugin_report = on;
                }
            }
            Action::PluginHosting(on) => {
                if !self.install_running() {
                    self.plugin_hosting = on;
                }
            }
            Action::StartInstall => self.start_install(),
            Action::SkipInstall => {
                if !self.install_running() {
                    for s in [StepId::Review, StepId::Remove, StepId::Import, StepId::Firmware, StepId::Driver] {
                        self.steps.set(s, Status::Skipped);
                    }
                    self.follow = false;
                    self.steps.reach(StepId::Console);
                }
            }
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

    /// Ask `uninstall.sh --list` (no root) what an earlier install left.
    fn start_earlier_check(&mut self) {
        if self.earlier_checking {
            return;
        }
        self.earlier_checking = true;
        if self.opts.demo {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            self.background(move || {
                thread::sleep(Duration::from_millis(300));
                Msg::Earlier(Ok(crate::demo::earlier(&home)))
            });
            return;
        }
        let Some(repo) = self.opts.repo.clone() else {
            self.earlier_checking = false;
            self.earlier = Some(Err("Can't find the open-apollo folder, so setup can't check.".into()));
            return;
        };
        self.background(move || {
            let out = Command::new("bash").arg(repo.join("scripts/uninstall.sh")).arg("--list").output();
            Msg::Earlier(match out {
                Ok(o) if o.status.success() => Ok(setuprun::parse_list(&String::from_utf8_lossy(&o.stdout))),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                Err(e) => Err(format!("bash: {e}")),
            })
        });
    }

    fn set_selection(&mut self, pick: Option<PathBuf>) {
        if pick != self.selected || (pick.is_some() && self.selected_device.is_some()) {
            self.steps.invalidate_dependents(StepId::Find);
        }
        if pick.is_some() {
            self.selected_device = None;
        }
        self.selected = pick;
        self.update_find_status();
    }

    fn select_device(&mut self, device: String) {
        if self.selected_device.as_deref() != Some(device.as_str()) {
            self.steps.invalidate_dependents(StepId::Find);
        }
        self.selected = None;
        self.selected_device = Some(device);
        self.update_find_status();
    }

    fn update_find_status(&mut self) {
        if self.scanning {
            return;
        }
        let status = match (self.selected_found(), &self.selected_device) {
            (Some(f), _) if f.has_console() => Status::Done,
            // The install opens the partition and checks it before changing anything.
            (None, Some(_)) => Status::Done,
            // Nothing found (yet) is amber: Review and install says what's missing.
            _ => Status::Warning,
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
        self.busy = Some(format!("Opening {} read-only…", image.display()));
        self.find_note = None;
        self.background(move || {
            let result = discover::attach_image(&image);
            Msg::Attached { image, result }
        });
    }

    fn start_install(&mut self) {
        let Ok(plan) = self.install_plan() else { return };
        let script = self.setup_script();
        let terminal = plan.command_line("sudo", &script);
        let log = self.open_run_log("Install", Some(&plan), &terminal);
        let job = if self.opts.demo {
            let (distro, kernel) = self.sys.as_ref().map(|s| (s.distro.clone(), s.kernel.clone())).unwrap_or_default();
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let (root, powered) = match self.selected_found() {
                Some(f) => (f.path().to_path_buf(), f.install.powered_plugins.len()),
                None => (PathBuf::new(), 223),
            };
            let script = crate::demo::setup(&crate::demo::SetupDemo {
                root: &root,
                home: &home,
                powered,
                partition: self.selected_device.as_deref(),
                keep_current: plan.keep_current,
                distro: &distro,
                kernel: &kernel,
                send_report: plan.send_report,
            });
            Job::replay(script, 0, self.wake.clone())
        } else {
            let bash = ["/usr/bin/bash", "/bin/bash"].into_iter().find(|b| Path::new(b).is_file()).unwrap_or("bash");
            let mut cmd = Command::new("pkexec");
            cmd.arg(bash).arg(&script).args(plan.args());
            if let Some(p) = &log {
                cmd.arg("--log").arg(p);
            }
            Job::spawn(cmd, self.wake.clone())
        };
        for s in [StepId::Remove, StepId::Import, StepId::Firmware, StepId::Driver, StepId::Console] {
            self.steps.set(s, Status::Pending);
        }
        self.steps.set(StepId::Review, Status::Running);
        self.console = None;
        self.launched = None;
        self.daemon = None;
        self.restart_daemon = false;
        self.follow = true;
        self.install = Some(Install { run: SetupRun::new(), job, demo: self.opts.demo, plan, terminal, log });
    }

    fn start_console(&mut self) {
        if self.run_log.is_none() {
            self.open_run_log("Console build", None, "");
        }
        let log = self.run_log.as_ref().map(|l| l.path().to_path_buf());
        let run = if self.opts.demo {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let res = self.opts.data.join(oa_assets::import::CONSOLE_DIR);
            Run {
                progress: Progress::new(INSTALL_UACONSOLE_SH),
                job: Job::replay(crate::demo::console(&home, &res), 0, self.wake.clone()),
                demo: true,
                command: "bash scripts/install-uaconsole.sh".into(),
                log: log.clone(),
            }
        } else {
            let Some(repo) = self.opts.repo.clone() else { return };
            let script = repo.join("scripts/install-uaconsole.sh");
            let mut cmd = Command::new("bash");
            cmd.arg(&script);
            // install-uaconsole.sh appends its output to the setup log.
            cmd.env_remove("OA_LOG_CAPTURED");
            match &log {
                Some(p) => cmd.env("OA_INSTALL_LOG", p),
                None => cmd.env_remove("OA_INSTALL_LOG"),
            };
            Run {
                progress: Progress::new(INSTALL_UACONSOLE_SH),
                job: Job::spawn(cmd, self.wake.clone()),
                demo: false,
                command: format!("bash {}", firmware::shell_quote(&script.to_string_lossy())),
                log: log.clone(),
            }
        };
        self.log_line(&format!("Building the Console: {}", run.command));
        self.console = Some(run);
        self.launched = None;
        self.steps.set(StepId::Console, Status::Running);
    }

    /// A new setup log for this run, opened with what was chosen. `None` (and
    /// no log) when it can't be written; the install goes on without one.
    fn open_run_log(&mut self, what: &str, plan: Option<&Plan>, command: &str) -> Option<PathBuf> {
        let mut log = RunLog::create(&runlog::log_dir(), SystemTime::now(), Redactor::from_env()).ok();
        if let Some(l) = log.as_mut() {
            let yes = |b: bool| if b { "yes" } else { "no" };
            l.line(&format!(
                "open-apollo setup log: {}, open-apollo-setup {}{}",
                runlog::iso_utc(SystemTime::now()),
                env!("CARGO_PKG_VERSION"),
                if self.opts.demo { " (--demo: nothing is installed)" } else { "" }
            ));
            if let Some(s) = &self.sys {
                l.line(&format!("System: {}, kernel {}", s.distro, s.kernel));
            }
            match plan {
                Some(plan) => l.line(&format!(
                    "{what}. From: {}. Keep the current install: {}. Send the install report: {}{}. Plug-ins on the Apollo: {}.",
                    match plan.source {
                        Source::Folder(_) => "a folder",
                        Source::Partition(_) => "a Windows partition, opened read-only",
                    },
                    yes(plan.keep_current),
                    yes(plan.send_report),
                    if plan.send_report {
                        format!(" (authorized plug-ins: {})", yes(plan.plugin_report))
                    } else {
                        String::new()
                    },
                    yes(plan.plugin_hosting),
                )),
                None => l.line(&format!("{what}.")),
            }
            if !command.is_empty() {
                l.line(&format!("Command: {command}"));
            }
        }
        let path = log.as_ref().map(|l| l.path().to_path_buf());
        self.run_log = log;
        path
    }

    fn log_line(&mut self, text: &str) {
        if let Some(l) = self.run_log.as_mut() {
            l.line(text);
        }
    }

    /// The daemon reads the imported catalogue when it starts.
    fn restart_mixer_daemon(&mut self) {
        if self.opts.demo {
            self.daemon = Some(Ok(()));
            return;
        }
        self.background(|| {
            let out = Command::new("systemctl").args(["--user", "restart", "ua-mixer-daemon.service"]).output();
            Msg::DaemonRestarted(match out {
                Ok(o) if o.status.success() => Ok(()),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                Err(e) => Err(format!("systemctl: {e}")),
            })
        });
    }

    fn launch(&mut self) {
        if self.opts.demo {
            self.launched = Some(Ok("Demo: the Console would start now (uaconsole).".into()));
            self.steps.set(StepId::Console, Status::Done);
            return;
        }
        let Some(bin) = self.console_bin.clone() else { return };
        let mut cmd = Command::new(&bin);
        // The user's mixer: it controls the Apollo (plain uaconsole starts read-only).
        cmd.arg("--writes");
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
        let Some(entry) = self.attached.get(index) else { return };
        let loop_device = entry.loop_device.clone();
        self.background(move || Msg::Released { index, result: discover::detach(&loop_device) });
    }
}

/// The user running setup (the install's desktop user).
fn current_user() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|u| !u.is_empty()))
        .or_else(|| {
            let out = Command::new("id").arg("-un").output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|u| !u.is_empty())
        })
        .unwrap_or_else(|| "nobody".into())
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|d| d.join(p)).unwrap_or(p.into()) }
}
