//! Drawing each step. Pages only read state and push [`Action`]s.

use eframe::egui::{self, CollapsingHeader, RichText, Ui};

use super::app::{Action, App, Install, Run};
use super::theme::*;
use super::widgets::*;
use crate::firmware;
use crate::scan::{Found, Origin, human_size};
use crate::setuprun::{self, RootStep, Section, Source};
use crate::steps::{Status, StepId};
use crate::syscheck::Level;

/// What the install report holds (scripts/install.sh generate_report) and where it goes
/// (install.sh TELEMETRY_URL), word for word on Review.
const REPORT_CONTENTS: &str = "It holds your distro, kernel, CPU, RAM, PCI and sound devices, PipeWire's Apollo devices, \
each install step's result and the driver's kernel log lines, with your user name, host name, machine id and serial \
numbers taken out. It goes to open-apollo-api.rolotrealanis.workers.dev; a copy stays in \
/tmp/open-apollo-install-report.json. Nothing is sent without this box.";

/// What the plug-in part adds (mixer-engine/plugin_report.py), word for word on Review.
const PLUGIN_REPORT_CONTENTS: &str = "Adds the render ids (UA's plug-in product numbers, such as 2276) of the plug-ins \
your Apollo itself says are authorized, and which of those built for the Apollo on this computer, so open-apollo can \
test the plug-ins people own first. Nothing else: no demo or licence details, no serial number, account, names or \
files. Your Apollo answers this only with Plug-ins on the Apollo on; otherwise the report says it couldn't ask.";

type Acts = Vec<Action>;

impl App {
    pub(super) fn heading(&self, step: StepId) -> (&'static str, String) {
        let any_failed = StepId::ALL.iter().any(|s| self.steps.status(*s) == Status::Failed);
        match step {
            StepId::Welcome => (
                "Set up open-apollo",
                "Get your Apollo working on Linux with the Universal Audio software you already own.".into(),
            ),
            StepId::System => (
                "System check",
                "A quick, read-only look at this computer. Nothing here stops setup; amber items come with a fix."
                    .into(),
            ),
            StepId::Find => (
                "Find your UAD install",
                "open-apollo uses the UAD software installed on your Windows side, opened read-only.".into(),
            ),
            StepId::Review => (
                "Review and install",
                "Setup always installs fresh: it removes any earlier open-apollo install first. Your password is asked once."
                    .into(),
            ),
            StepId::Remove => (
                "Remove previous install",
                "Everything an earlier open-apollo install left on this computer is removed, so what follows is fresh."
                    .into(),
            ),
            StepId::Import => {
                ("Import assets", format!(
                    "The Console art is copied from your UAD install to {}. The plug-in panel specs (about 1 MB) are downloaded from GitHub, and the plug-in panels' art is made from your UAD install.",
                    tilde(&self.opts.data)
                ))
            }
            StepId::Firmware => (
                "DSP programs & firmware",
                "What the driver loads onto your Apollo is extracted from your own UAD install.".into(),
            ),
            StepId::Driver => {
                ("Install the driver", "The open-apollo driver is built and the audio settings for your Apollo installed.".into())
            }
            StepId::Console => {
                ("Console", "The open-apollo Console, the mixer for your Apollo, is built and started.".into())
            }
            StepId::Done if any_failed => {
                ("Setup finished", "Some steps need another look; the details are below.".into())
            }
            StepId::Done => ("You're set", "Here's what setup did and where everything went.".into()),
        }
    }

    pub(super) fn page(&self, ui: &mut Ui, act: &mut Acts) {
        match self.steps.current() {
            StepId::Welcome => self.welcome(ui),
            StepId::System => self.system(ui, act),
            StepId::Find => self.find(ui, act),
            StepId::Review => self.review(ui, act),
            StepId::Remove => self.remove_page(ui, act),
            StepId::Import => self.import_page(ui, act),
            StepId::Firmware => self.firmware_page(ui, act),
            StepId::Driver => self.driver_page(ui, act),
            StepId::Console => self.console_page(ui, act),
            StepId::Done => self.done(ui, act),
        }
    }

    // ---- 1. Welcome ----

    fn welcome(&self, ui: &mut Ui) {
        card(ui, false, |ui| {
            ui.label(RichText::new("What happens").strong().color(STRONG));
            ui.add_space(4.0);
            let rows = [
                (StepId::System, "A read-only look at your kernel, Secure Boot, Thunderbolt and Apollo."),
                (StepId::Find, "Your UAD install on a Windows partition, a Windows VM's disk, or a Wine prefix."),
                (StepId::Review, "See what will happen, then install. Asks for your password once."),
                (StepId::Remove, "Remove everything an earlier open-apollo install left, so the rest is fresh."),
                (StepId::Import, "Copy the Console art, fetch the plug-in panel specs and make their art."),
                (StepId::Firmware, "Extract the DSP programs and firmware the driver loads."),
                (StepId::Driver, "Build and install the driver and audio settings."),
                (StepId::Console, "Build and start the open-apollo Console. No password needed."),
            ];
            for (i, (step, text)) in rows.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    ui.label(RichText::new(format!("{}", i + 1)).strong().color(ACCENT));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new(step.title()).strong().color(TEXT));
                        ui.label(RichText::new(*text).color(WEAK));
                    });
                });
            }
        });
        ui.add_space(6.0);
        card(ui, false, |ui| {
            ui.horizontal(|ui| {
                status_icon(ui, Status::Done, 18.0);
                ui.label(RichText::new("Your UA software stays yours").strong().color(STRONG));
            });
            ui.add_space(2.0);
            let data = tilde(&self.opts.data);
            for line in [
                "open-apollo includes no Universal Audio software or artwork. It uses the copy you installed yourself.".to_string(),
                "Your Windows disk or VM disk image is opened read-only. Nothing on it is changed.".to_string(),
                format!("Files are copied only within this computer, to {data} and {}. Nothing is uploaded; the install report stays here.", firmware::FIRMWARE_DIR),
                "Universal Audio's license terms apply to your copy of their software. Whether this use fits them is your call.".to_string(),
            ] {
                ui.horizontal_top(|ui| {
                    ui.label(RichText::new("•").color(ACCENT));
                    ui.add(egui::Label::new(RichText::new(line).color(TEXT)).wrap());
                });
            }
        });
        if self.opts.demo {
            ui.add_space(6.0);
            callout(
                ui,
                Status::Skipped,
                "Demo mode",
                "The install and the Console build only simulate their work: no password prompt, no builds, nothing removed or installed. Scanning is real.",
            );
        }
        ui.add_space(8.0);
        ui.label(RichText::new("open-apollo is an independent open-source project, not affiliated with or endorsed by Universal Audio.").small().color(FAINT));
    }

    // ---- 2. System check ----

    fn system(&self, ui: &mut Ui, act: &mut Acts) {
        let Some(r) = &self.sys else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking this computer…");
            });
            return;
        };
        let warns = r.warnings();
        ui.horizontal(|ui| {
            if warns == 0 {
                status_icon(ui, Status::Done, 20.0);
                ui.label(RichText::new("Everything looks ready.").strong().color(STRONG));
            } else {
                status_icon(ui, Status::Warning, 20.0);
                let s = if warns == 1 { "1 thing to look at" } else { "things to look at" };
                let text = if warns == 1 { s.to_string() } else { format!("{warns} {s}") };
                ui.label(RichText::new(format!("{text}. None of them stop setup.")).strong().color(STRONG));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let running = self.steps.status(StepId::System) == Status::Running;
                if secondary(ui, if running { "Checking…" } else { "Check again" }, !running).clicked() {
                    act.push(Action::RunSystem);
                }
            });
        });
        ui.add_space(4.0);
        let mut checks: Vec<_> = r.checks.iter().collect();
        checks.sort_by_key(|c| std::cmp::Reverse(c.level == Level::Warn));
        card(ui, false, |ui| {
            for (i, c) in checks.iter().enumerate() {
                if i > 0 {
                    rule(ui);
                }
                ui.horizontal_top(|ui| {
                    status_icon(ui, level_status(c.level), 18.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal_top(|ui| {
                            fixed_label(ui, 170.0, RichText::new(c.title).strong().color(TEXT));
                            ui.add(
                                egui::Label::new(RichText::new(&c.value).color(if c.level == Level::Warn {
                                    WARN
                                } else {
                                    WEAK
                                }))
                                .wrap(),
                            );
                        });
                        if let Some(h) = &c.hint {
                            ui.add(egui::Label::new(RichText::new(h).small().color(WEAK)).wrap());
                        }
                        for cmd in &c.commands {
                            command(ui, cmd);
                        }
                    });
                });
            }
        });
    }

    // ---- 3. Find ----

    fn find(&self, ui: &mut Ui, act: &mut Acts) {
        ui.horizontal(|ui| {
            let idle = self.busy.is_none() && !self.scanning;
            if secondary(ui, "Rescan", idle).clicked() {
                act.push(Action::Rescan);
            }
            if secondary(ui, "Use a VM disk image…", idle).clicked() {
                act.push(Action::PickImage);
            }
            if secondary(ui, "Choose a folder…", idle).clicked() {
                act.push(Action::PickFolder);
            }
            if let Some(b) = &self.busy {
                ui.spinner();
                ui.label(RichText::new(b).color(WEAK));
            } else if self.scanning {
                ui.spinner();
                ui.label(RichText::new("Looking at disks, partitions and Wine prefixes…").color(WEAK));
            }
        });
        if let Some((st, note)) = &self.find_note {
            callout(ui, *st, note, "");
        }
        let Some(scan) = &self.scan else { return };
        ui.add_space(4.0);

        let (with, without): (Vec<&Found>, Vec<&Found>) = scan.found.iter().partition(|f| f.install.found_anything());
        let parts: Vec<_> = scan.likely_windows().collect();
        if with.is_empty() && parts.is_empty() {
            callout(
                ui,
                Status::Warning,
                "No UAD install found yet",
                "If Windows is on another disk, connect it and press Rescan. No Windows on this computer? See the help at the bottom.",
            );
        } else if !with.is_empty() {
            section(ui, "UAD installs found");
            for f in with {
                self.found_card(ui, f, act);
            }
        }
        if !parts.is_empty() {
            section(ui, "Windows partitions");
            for u in parts {
                self.partition_card(ui, u, act);
            }
        }
        if !scan.images.is_empty() {
            section(ui, "Windows VM disk images");
            for img in &scan.images {
                card(ui, false, |ui| {
                    ui.horizontal(|ui| {
                        badge(ui, "VM disk", INFO);
                        ui.label(RichText::new(tilde(&img.path)).strong().color(TEXT));
                        ui.label(RichText::new(human_size(img.size)).color(WEAK));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if secondary(ui, "Open read-only", self.busy.is_none()).clicked() {
                                act.push(Action::AttachImage(img.path.clone()));
                            }
                        });
                    });
                    ui.label(
                        RichText::new("Shut the VM's Windows down fully first (not sleep or hibernate).")
                            .small()
                            .color(WEAK),
                    );
                });
            }
        }
        let small: Vec<_> = scan.other_partitions().collect();
        if !small.is_empty() || !without.is_empty() {
            ui.add_space(4.0);
            CollapsingHeader::new(RichText::new("Also checked").color(WEAK)).id_salt("checked").show(ui, |ui| {
                for f in &without {
                    ui.label(
                        RichText::new(format!("{} ({}): no UAD software", tilde(f.path()), f.origin.label()))
                            .small()
                            .color(WEAK),
                    );
                }
                for u in small {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} {} ({}): small NTFS partition, usually recovery or data",
                                u.device,
                                u.label.as_deref().unwrap_or(""),
                                u.size.as_deref().unwrap_or("?")
                            ))
                            .small()
                            .color(WEAK),
                        );
                        let picked = self.selected_device.as_deref() == Some(u.device.as_str());
                        if !u.is_bitlocker()
                            && !picked
                            && !self.selection_locked()
                            && ui.small_button("Use this one").clicked()
                        {
                            act.push(Action::SelectDevice(u.device.clone()));
                        }
                    });
                }
            });
        }
        ui.add_space(6.0);
        CollapsingHeader::new(RichText::new("No Windows on this computer?").strong().color(TEXT))
            .id_salt("vmhelp")
            .default_open(scan.best().is_none() && scan.unmounted.is_empty())
            .show(ui, vm_help);
        ui.add_space(4.0);
        if !self.scanning && self.selected_found().is_none() && self.selected_device.is_none() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Already set up, or doing this later?").color(WEAK));
                if ui.link("Skip finding an install").clicked() {
                    act.push(Action::SkipFind);
                }
            });
        }
    }

    fn found_card(&self, ui: &mut Ui, f: &Found, act: &mut Acts) {
        let selected = self.selected.as_deref() == Some(f.path());
        card(ui, selected, |ui| {
            ui.horizontal(|ui| {
                let color = match f.origin {
                    Origin::VmImage { .. } => INFO,
                    Origin::Partition { .. } => ACCENT,
                    _ => WEAK,
                };
                badge(ui, f.origin.label(), color);
                ui.label(RichText::new(tilde(f.path())).strong().color(STRONG));
                if matches!(&f.origin, Origin::VmImage { image, .. } if self.attached.iter().any(|a| &a.image == image))
                {
                    badge(ui, "opened by setup", WEAK);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(
                        selected || !self.selection_locked(),
                        egui::RadioButton::new(selected, if selected { "Using this one" } else { "Use this one" }),
                    );
                    if r.clicked() && !selected {
                        act.push(Action::Select(f.path().to_path_buf()));
                    }
                });
            });
            match &f.origin {
                Origin::VmImage { image, device } => {
                    ui.label(RichText::new(format!("{device}, from {}", tilde(image))).small().color(WEAK));
                }
                Origin::Partition { device } => {
                    ui.label(RichText::new(device).small().color(WEAK));
                }
                _ => {}
            }
            ui.add_space(2.0);
            let i = &f.install;
            let n = |v: usize| if v == 0 { "none".to_string() } else { v.to_string() };
            stats(
                ui,
                &[
                    (
                        "UAD Console",
                        i.console
                            .as_ref()
                            .map(|c| c.version.clone().unwrap_or_else(|| "installed".into()))
                            .unwrap_or_else(|| "not installed".into()),
                    ),
                    ("Powered plug-ins", n(i.powered_plugins.len())),
                    ("VST3 plug-ins", n(i.vst3.len())),
                    ("UA driver files", n(i.drivers.len())),
                ],
            );
            let missing = setuprun::missing_parts(i);
            if !missing.is_empty() {
                ui.label(
                    RichText::new(format!("Setup can't use this one: it has no {}.", missing.join(", and no ")))
                        .small()
                        .color(WARN),
                );
            }
        });
    }

    fn partition_card(&self, ui: &mut Ui, u: &oa_assets::discover::Unmounted, act: &mut Acts) {
        let picked = self.selected_device.as_deref() == Some(u.device.as_str());
        card(ui, picked, |ui| {
            ui.horizontal(|ui| {
                badge(
                    ui,
                    if u.is_bitlocker() { "BitLocker" } else { "not open" },
                    if u.is_bitlocker() { WARN } else { WEAK },
                );
                let label = u.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default();
                ui.label(RichText::new(format!("Windows partition{label}")).strong().color(STRONG));
                ui.label(RichText::new(format!("{}, {}", u.device, u.size.as_deref().unwrap_or("?"))).color(WEAK));
                if !u.is_bitlocker() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui.add_enabled(
                            picked || !self.selection_locked(),
                            egui::RadioButton::new(picked, if picked { "Using this one" } else { "Use this one" }),
                        );
                        if r.clicked() && !picked {
                            act.push(Action::SelectDevice(u.device.clone()));
                        }
                    });
                }
            });
            if u.is_bitlocker() {
                ui.add(egui::Label::new(RichText::new(
                    "This partition is encrypted with BitLocker, so Linux can't read it as it is. Either unlock it in your file manager with its 48-digit recovery key (find it at account.microsoft.com/devices/recoverykey), then press Rescan; or start Windows, copy \"C:\\Program Files\\Universal Audio\" and \"C:\\Program Files (x86)\\Universal Audio\" to a USB stick keeping those folder names, and use Choose a folder.",
                ).small().color(TEXT)).wrap());
            } else {
                ui.label(RichText::new("The install opens it read-only (no extra password) and checks it holds a UAD install before changing anything. Nothing on it is changed.").small().color(WEAK));
            }
        });
    }

    // ---- 4. Review and install ----

    fn review(&self, ui: &mut Ui, act: &mut Acts) {
        let plan = self.install_plan();
        let picked = self.selected_found().is_some() || self.selected_device.is_some();
        if !picked && self.install.is_none() {
            no_install_blocker(
                ui,
                act,
                "The install reads the DSP programs, firmware and Console art from your own UAD install, and none is picked.",
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Driver already installed?").color(WEAK));
                if ui.link("Skip the install and go to the Console").clicked() {
                    act.push(Action::SkipInstall);
                }
            });
            return;
        }
        if let Some(i) = &self.install {
            self.install_banner(ui, act, i);
        }
        card(ui, false, |ui| {
            let source = match (self.selected_found(), &self.selected_device) {
                (Some(f), _) => {
                    let console = f
                        .install
                        .console
                        .as_ref()
                        .map(|c| format!("UAD Console {}", c.version.as_deref().unwrap_or("(version unknown)")))
                        .unwrap_or_else(|| "no UAD Console".into());
                    format!("{}: {} ({console})", f.origin.label(), tilde(f.path()))
                }
                (None, Some(d)) => format!("Windows partition {d}: opened read-only during the install"),
                (None, None) => "none".into(),
            };
            facts(
                ui,
                &[
                    ("From", source),
                    ("Console art to", tilde(&self.opts.data.join(oa_assets::import::CONSOLE_DIR))),
                    ("Plug-in panel specs to", setuprun::PANEL_SPECS_DIR.to_string()),
                    ("Firmware to", firmware::FIRMWARE_DIR.to_string()),
                ],
            );
        });
        section(ui, "When you press Install");
        card(ui, false, |ui| {
            let remove = if self.keep_current {
                "Skipped: your current install stays, and the rest is installed over it."
            } else {
                "Everything an earlier open-apollo install left is removed: driver, DKMS builds, settings, extracted firmware, imported art, plug-in panel specs, the Console."
            };
            let rows: Vec<(&str, String)> = vec![
                ("1", "Your UAD install is checked. If it can't be used, setup stops here and nothing is changed.".into()),
                ("2", remove.into()),
                ("3", "The Console art is copied fresh from your UAD install. The plug-in panel specs (about 1 MB) are downloaded from GitHub and checked against a pinned SHA-256, and the panels' art is made from your UAD install.".into()),
                ("4", "The DSP programs and firmware are extracted fresh.".into()),
                ("5", "The driver is built fresh and installed with DKMS, PipeWire, WirePlumber and UCM2 settings and the mixer daemon.".into()),
                ("6", "Then, without a password: the Console is built, the mixer daemon restarted, and you can launch it.".into()),
            ];
            facts(ui, &rows);
            ui.add_space(2.0);
            ui.label(RichText::new("Steps 1 to 5 run in one go with administrator rights, so your password is asked once. The installer's questions get their default answers; on Intel VT-d systems iommu=pt is added to GRUB (a restart is needed after).").small().color(WEAK));
        });
        self.earlier_card(ui, act);
        ui.add_space(4.0);
        let idle = !self.install_running();
        let mut keep = self.keep_current;
        if ui
            .add_enabled(idle, egui::Checkbox::new(&mut keep, "Keep my current install (don't remove it first)"))
            .changed()
        {
            act.push(Action::KeepCurrent(keep));
        }
        ui.label(
            RichText::new(
                "Only if a clean reinstall isn't possible: leftovers of an older version can then get in the way.",
            )
            .small()
            .color(WEAK),
        );
        let mut hosting = self.plugin_hosting;
        if ui.add_enabled(idle, egui::Checkbox::new(&mut hosting, "Plug-ins on the Apollo (experimental)")).changed() {
            act.push(Action::PluginHosting(hosting));
        }
        ui.add(
            egui::Label::new(
                RichText::new(
                    "Runs your Powered plug-ins on the Apollo's DSP. Setup builds their control mappings by running your own plug-ins offline (about 10 to 40 minutes after the driver; nothing leaves this computer) and loads the driver with its plug-in host on. The Apollo's licence check still decides each load.",
                )
                .small()
                .color(WEAK),
            )
            .wrap(),
        );
        let mut send = self.send_report;
        if ui
            .add_enabled(idle, egui::Checkbox::new(&mut send, "Send an anonymous install report to help open-apollo"))
            .changed()
        {
            act.push(Action::SendReport(send));
        }
        ui.add(egui::Label::new(RichText::new(REPORT_CONTENTS).small().color(WEAK)).wrap());
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            let mut plugins = self.plugin_report;
            if ui
                .add_enabled(
                    idle && self.send_report,
                    egui::Checkbox::new(&mut plugins, "Include the plug-ins my Apollo is authorized for"),
                )
                .changed()
            {
                act.push(Action::PluginReport(plugins));
            }
        });
        ui.add(egui::Label::new(RichText::new(PLUGIN_REPORT_CONTENTS).small().color(WEAK)).wrap());
        if let Some(sys) = &self.sys
            && sys.apollo.pci.is_empty()
            && !sys.apollo.usb.is_empty()
        {
            callout(
                ui,
                Status::Warning,
                "Your Apollo is a USB model",
                "USB Apollos use scripts/install-usb.sh. This installs the Thunderbolt driver.",
            );
        }
        if self.opts.demo {
            badge(ui, "DEMO: the install is simulated, nothing changes", INFO);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if self.install_running() {
                ui.spinner();
                ui.label(RichText::new("Installing… the next pages follow along.").color(WEAK));
                return;
            }
            let label = if self.install.is_some() { "Install again" } else { "Install (asks for your password once)" };
            let r = if self.install.as_ref().is_some_and(|i| !i.running()) {
                secondary(ui, label, plan.is_ok())
            } else {
                primary(ui, label, plan.is_ok())
            };
            if r.clicked() {
                act.push(Action::StartInstall);
            }
        });
        if let Err(why) = &plan
            && !self.install_running()
        {
            ui.label(RichText::new(why).small().color(WARN));
        }
        if let Ok(p) = &plan {
            CollapsingHeader::new(RichText::new("The command it runs").small().color(WEAK)).id_salt("cmd").show(
                ui,
                |ui| {
                    command(ui, &p.command_line("pkexec", &self.setup_script()));
                },
            );
        }
    }

    /// What an earlier install left, from `uninstall.sh --list`.
    fn earlier_card(&self, ui: &mut Ui, act: &mut Acts) {
        section(ui, "Earlier open-apollo install");
        card(ui, false, |ui| {
            if self.install.is_some() {
                ui.label(RichText::new("See Remove previous install for what the install removed.").color(WEAK));
                return;
            }
            match &self.earlier {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Looking for an earlier install…").color(WEAK));
                    });
                }
                Some(Err(e)) => {
                    ui.horizontal(|ui| {
                        status_icon(ui, Status::Warning, 18.0);
                        ui.label(RichText::new(format!("Couldn't check: {e}")).color(WARN));
                    });
                    ui.label(RichText::new("The install removes whatever is there anyway.").small().color(WEAK));
                }
                Some(Ok(list)) if list.is_empty() => {
                    ui.horizontal(|ui| {
                        status_icon(ui, Status::Done, 18.0);
                        ui.label(RichText::new("Nothing from an earlier install is on this computer.").color(TEXT));
                    });
                }
                Some(Ok(list)) => {
                    ui.horizontal(|ui| {
                        status_icon(ui, if self.keep_current { Status::Skipped } else { Status::Warning }, 18.0);
                        let what = if self.keep_current { "kept" } else { "removed first" };
                        ui.label(
                            RichText::new(format!("Found, and {what} ({} items):", list.len())).strong().color(TEXT),
                        );
                    });
                    let rows: Vec<(&str, String)> =
                        setuprun::group(list).into_iter().map(|(label, items)| (label, summarize(&items))).collect();
                    facts(ui, &rows);
                }
            }
            if !self.earlier_checking && ui.link("Check again").clicked() {
                act.push(Action::CheckEarlier);
            }
        });
    }

    /// How the install went, at the top of Review and install.
    fn install_banner(&self, ui: &mut Ui, act: &mut Acts, i: &Install) {
        match &i.run.outcome {
            None => {
                let at = i.run.current().map(|c| c.doing()).unwrap_or("waiting for your password");
                callout(ui, Status::Running, &format!("Installing: {at}…"), "");
            }
            Some(o) => {
                callout(ui, o.status, &o.message, "");
                if o.status == Status::Failed {
                    failed_help(ui, act, i, false);
                }
            }
        }
    }

    // ---- 5-8. The install's sections ----

    /// The top of a section's page: where the install is, and how this section
    /// went. Returns the section once it has started.
    fn section_head<'a>(&'a self, ui: &mut Ui, act: &mut Acts, id: RootStep) -> Option<&'a Section> {
        let Some(i) = &self.install else {
            let skipped = self.steps.status(id.page().unwrap_or(StepId::Review)) == Status::Skipped;
            if skipped {
                callout(ui, Status::Skipped, "Skipped", "You skipped the install.");
            } else {
                callout(
                    ui,
                    Status::Pending,
                    "Not started yet",
                    "This runs as part of the install. See Review and install, then press Install.",
                );
                if secondary(ui, "Go to Review and install", true).clicked() {
                    act.push(Action::Goto(StepId::Review));
                }
            }
            return None;
        };
        if i.demo {
            badge(ui, "DEMO: simulated, nothing changes", INFO);
        }
        let s = i.run.section(id);
        match s.status {
            Status::Pending if i.running() => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Waiting for the steps before it…").color(WEAK));
                });
                None
            }
            Status::Pending => {
                let why = match i.run.failed_at() {
                    Some(at) => format!("The install stopped while {} before getting here.", at.doing()),
                    None => "The install ended before getting here.".into(),
                };
                callout(ui, Status::Skipped, "Not run", &why);
                failed_help(ui, act, i, true);
                None
            }
            Status::Running => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let now = s.progress.current().map(|c| c.label.as_str()).unwrap_or("Working");
                    ui.label(RichText::new(format!("{now}…")).strong().color(STRONG));
                });
                Some(s)
            }
            st => {
                let status = if id == RootStep::Firmware {
                    let usb = self.sys.as_ref().is_none_or(|s| !s.apollo.usb.is_empty());
                    i.run.firmware_status(usb)
                } else {
                    st
                };
                let fallback = match status {
                    Status::Done => "Done.",
                    Status::Warning => "Done, with warnings.",
                    Status::Skipped => "Skipped.",
                    _ => "Failed.",
                };
                callout(ui, status, s.message.as_deref().unwrap_or(fallback), "");
                if st == Status::Failed {
                    failed_help(ui, act, i, true);
                }
                Some(s)
            }
        }
    }

    fn remove_page(&self, ui: &mut Ui, act: &mut Acts) {
        ui.add(egui::Label::new(RichText::new("Runs open-apollo's uninstaller (scripts/uninstall.sh --yes --purge): every DKMS build of the driver, the mixer daemon's units and drop-ins, PipeWire and WirePlumber rules (yours and the system's), udev rules, the extracted firmware, the imported art and the Console go, and it checks nothing is left. Then everything is installed fresh.").color(TEXT)).wrap());
        ui.label(RichText::new("By hand: sudo bash scripts/uninstall.sh --purge").small().color(WEAK));
        let Some(s) = self.section_head(ui, act, RootStep::Remove) else { return };
        if s.status == Status::Skipped {
            return;
        }
        two_columns(ui, s, "remove-log");
    }

    fn import_page(&self, ui: &mut Ui, act: &mut Acts) {
        ui.add(egui::Label::new(RichText::new(format!("Copies the UAD Console's art from your install to {}, as you (oa-assets import). Plug-in panels follow once oa-assets can generate them.", tilde(&self.opts.data.join(oa_assets::import::CONSOLE_DIR)))).color(TEXT)).wrap());
        let Some(s) = self.section_head(ui, act, RootStep::Import) else { return };
        if s.status.is_success()
            && let Some((ver, files)) = self.console_art()
        {
            card(ui, false, |ui| {
                facts(
                    ui,
                    &[
                        (
                            "Console art",
                            format!("Console {}, {files} files", ver.as_deref().unwrap_or("(version unknown)")),
                        ),
                        ("In", tilde(&self.opts.data.join(oa_assets::import::CONSOLE_DIR))),
                        ("Manifest", tilde(&self.opts.data.join(oa_assets::import::MANIFEST))),
                    ],
                );
            });
        }
        log_view(ui, "import-log", &s.progress.lines, 160.0);
    }

    fn firmware_page(&self, ui: &mut Ui, act: &mut Acts) {
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "Your Apollo runs DSP programs and firmware written by Universal Audio. open-apollo doesn't ship them, so they're extracted from your install into {}, with a manifest listing each file's SHA-256.",
                    firmware::FIRMWARE_DIR
                ))
                .color(TEXT),
            )
            .wrap(),
        );
        let Some(s) = self.section_head(ui, act, RootStep::Firmware) else { return };
        if !s.files.is_empty() {
            card(ui, false, |ui| {
                egui::Grid::new("fw-files").num_columns(3).spacing([14.0, 6.0]).show(ui, |ui| {
                    for f in &s.files {
                        ui.horizontal(|ui| {
                            status_icon(ui, f.status, 16.0);
                            ui.label(RichText::new(&f.name).monospace().color(STRONG));
                        });
                        let sha = f.sha256.as_deref().map(|h| format!("sha256 {}", short_hash(h))).unwrap_or_default();
                        ui.label(RichText::new(sha).monospace().small().color(WEAK));
                        ui.label(RichText::new(f.note.as_deref().unwrap_or("")).small().color(WEAK));
                        ui.end_row();
                    }
                });
            });
        }
        CollapsingHeader::new(RichText::new("Output").color(WEAK))
            .id_salt("firmware-log")
            .default_open(s.status == Status::Running || s.status == Status::Failed)
            .show(ui, |ui| log_view(ui, "firmware-log", &s.progress.lines, 200.0));
    }

    fn driver_page(&self, ui: &mut Ui, act: &mut Acts) {
        ui.add(
            egui::Label::new(
                RichText::new("Runs open-apollo's installer (scripts/install.sh --yes): it installs missing packages, builds the ua_apollo driver, registers it with DKMS so kernel updates rebuild it, installs PipeWire, WirePlumber and UCM2 settings, and, if your Apollo is on, loads the driver and starts the mixer daemon.")
                    .color(TEXT),
            )
            .wrap(),
        );
        if let Some(sys) = &self.sys
            && sys.apollo.pci.is_empty()
            && self.install.is_none()
        {
            callout(
                ui,
                Status::Skipped,
                "No Apollo connected",
                "That's fine: it's set up when you connect it. To have the installer verify the driver too, power on the Apollo, connect it, and wait ~20 s before installing.",
            );
        }
        let Some(s) = self.section_head(ui, act, RootStep::Driver) else { return };
        if s.progress.reboot_needed {
            callout(
                ui,
                Status::Warning,
                "Restart needed",
                "The installer added iommu=pt to the kernel command line. Restart before using the Apollo.",
            );
        }
        two_columns(ui, s, "driver-log");
    }

    // ---- 9. Console ----

    fn console_page(&self, ui: &mut Ui, act: &mut Acts) {
        if !self.console_allowed() {
            let installing = self.install_running();
            callout(
                ui,
                if installing { Status::Running } else { Status::Pending },
                if installing {
                    "The Console is built once the install is done"
                } else {
                    "The Console is built after the install"
                },
                "Building it before would be wasted: the install removes an earlier Console build first.",
            );
            if !installing && secondary(ui, "Go to Review and install", true).clicked() {
                act.push(Action::Goto(StepId::Review));
            }
            if self.steps.status(StepId::Console) == Status::Pending && ui.link("Skip the Console").clicked() {
                act.push(Action::SkipConsole);
            }
            return;
        }
        card(ui, false, |ui| {
            ui.horizontal(|ui| match self.console_art() {
                Some((ver, files)) => {
                    status_icon(ui, Status::Done, 18.0);
                    ui.label(
                        RichText::new(format!(
                            "Console art imported (Console {}, {files} files)",
                            ver.as_deref().unwrap_or("version unknown")
                        ))
                        .color(TEXT),
                    );
                }
                None => {
                    status_icon(ui, Status::Warning, 18.0);
                    ui.label(RichText::new("No Console art yet: the Console needs it to draw its mixer.").color(WARN));
                }
            });
            let installed = self.console_installed();
            ui.horizontal(|ui| {
                if installed {
                    status_icon(ui, Status::Done, 18.0);
                    let at = self.console_bin.as_deref().map(tilde).unwrap_or_else(|| "~/.local/bin/uaconsole".into());
                    ui.label(RichText::new(format!("uaconsole is installed: {at}")).color(TEXT));
                } else {
                    status_icon(ui, Status::Pending, 18.0);
                    ui.label(RichText::new("uaconsole isn't installed yet").color(TEXT));
                }
            });
            match &self.daemon {
                Some(Ok(())) => {
                    ui.horizontal(|ui| {
                        status_icon(ui, Status::Done, 18.0);
                        ui.label(
                            RichText::new("Mixer daemon restarted: it sees the imported art and plug-ins").color(TEXT),
                        );
                    });
                }
                Some(Err(e)) => {
                    ui.horizontal(|ui| {
                        status_icon(ui, Status::Warning, 18.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "Couldn't restart the mixer daemon ({e}); it reads them when it next starts."
                                ))
                                .color(WARN),
                            )
                            .wrap(),
                        );
                    });
                }
                None => {}
            }
        });
        let needs_build = !self.console_installed();
        if needs_build && self.console.is_none() {
            ui.add(
                egui::Label::new(
                    RichText::new("Builds uaconsole from this open-apollo folder with Rust (a few minutes the first time) and installs it for you in ~/.local/bin, with an \"Open Apollo Console\" menu entry. No administrator rights needed.")
                        .color(TEXT),
                )
                .wrap(),
            );
            let cargo = self.sys.as_ref().and_then(|s| s.checks.iter().find(|c| c.title == "Rust toolchain"));
            if cargo.is_some_and(|c| c.level != Level::Ok) && !self.opts.demo {
                callout(ui, Status::Warning, "Rust isn't installed", "Install it, then press Build and install:");
                command(ui, "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh");
            }
        }
        if let Some(run) = &self.console {
            if run.demo {
                badge(ui, "DEMO: simulated build, nothing installed", INFO);
            }
            run_outcome(ui, run);
            ui.columns(2, |cols| {
                card(&mut cols[0], false, |ui| substeps(ui, &run.progress.substeps));
                log_view(&mut cols[1], "console-log", &run.progress.lines, 240.0);
            });
        }
        match &self.launched {
            Some(Ok(m)) => callout(
                ui,
                Status::Done,
                m,
                "It connects to the mixer daemon the driver install started (127.0.0.1:4710).",
            ),
            Some(Err(e)) => callout(ui, Status::Failed, e, ""),
            None => {}
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let running = self.console.as_ref().is_some_and(Run::running);
            if running {
                return;
            }
            if needs_build {
                let can = self.opts.demo || self.opts.repo.is_some();
                if primary(ui, if self.console.is_some() { "Try again" } else { "Build and install" }, can).clicked() {
                    act.push(Action::StartConsole);
                }
            } else {
                if primary(ui, "Launch Console", true).clicked() {
                    act.push(Action::Launch);
                }
                if secondary(ui, "Rebuild and reinstall", self.opts.demo || self.opts.repo.is_some()).clicked() {
                    act.push(Action::StartConsole);
                }
            }
            if self.steps.status(StepId::Console) == Status::Pending && secondary(ui, "Skip", true).clicked() {
                act.push(Action::SkipConsole);
            }
        });
    }

    fn console_installed(&self) -> bool {
        if self.opts.demo { self.console.as_ref().is_some_and(Run::exit_ok) } else { self.console_bin.is_some() }
    }

    // ---- 10. Done ----

    fn done(&self, ui: &mut Ui, act: &mut Acts) {
        card(ui, false, |ui| {
            for step in &StepId::ALL[1..StepId::ALL.len() - 1] {
                ui.horizontal(|ui| {
                    status_icon(ui, self.steps.status(*step), 18.0);
                    fixed_label(ui, 190.0, RichText::new(step.title()).strong().color(TEXT));
                    ui.add(egui::Label::new(RichText::new(self.result_of(*step)).color(WEAK)).wrap());
                });
            }
        });
        let reboot = self.install.as_ref().is_some_and(|i| i.run.section(RootStep::Driver).progress.reboot_needed);
        if reboot {
            callout(
                ui,
                Status::Warning,
                "Restart the computer",
                "The installer changed the kernel command line (iommu=pt). The Apollo works after a restart.",
            );
        }
        section(ui, "Where things went");
        card(ui, false, |ui| {
            let mut rows: Vec<(&str, String)> = Vec::new();
            if self.console_art().is_some() {
                rows.push(("Console art", tilde(&self.opts.data.join(oa_assets::import::CONSOLE_DIR))));
                rows.push((
                    "",
                    format!(
                        "{} (what was imported, from where)",
                        tilde(&self.opts.data.join(oa_assets::import::MANIFEST))
                    ),
                ));
                if !self.opts.data_is_default {
                    rows.push((
                        "",
                        format!(
                            "Custom folder: start the Console with CONSOLE_RESOURCES={}",
                            tilde(&self.opts.data.join(oa_assets::import::CONSOLE_DIR))
                        ),
                    ));
                }
            }
            if self.steps.status(StepId::Firmware).is_success() {
                rows.push(("DSP programs", format!("{}  (+ {})", firmware::FIRMWARE_DIR, firmware::MANIFEST)));
            }
            if self.steps.status(StepId::Driver).is_success() {
                rows.push(("Driver", "ua_apollo kernel module, kept up to date by DKMS".into()));
                rows.push(("", "PipeWire, WirePlumber and UCM2 settings; mixer daemon on 127.0.0.1:4710".into()));
                let plan = self.install.as_ref().map(|i| &i.plan);
                let sent = match plan {
                    Some(p) if p.send_report && p.plugin_report => "also sent, with the authorized plug-ins",
                    Some(p) if p.send_report => "also sent, without plug-ins",
                    _ => "not uploaded",
                };
                rows.push(("", format!("Install report: /tmp/open-apollo-install-report.json ({sent})")));
            }
            if let Some(log) = self.install.as_ref().and_then(|i| i.log.as_ref()) {
                rows.push(("Setup log", format!("{}  (attach it to a bug report)", tilde(log))));
            }
            if self.steps.status(StepId::Console).is_success() {
                rows.push(("Console", "~/.local/bin/uaconsole, and \"Open Apollo Console\" in your app menu".into()));
            }
            rows.push(("Remove it all", "sudo bash scripts/uninstall.sh --purge".into()));
            facts(ui, &rows);
        });
        section(ui, "Your Windows disk");
        card(ui, false, |ui| {
            let opened = self.install.as_ref().filter(|i| matches!(i.plan.source, Source::Partition(_)));
            if self.attached.is_empty() && opened.is_none() {
                ui.label(
                    RichText::new(
                        "Setup didn't open any disk itself; disks that were already open were left as they were.",
                    )
                    .color(WEAK),
                );
            }
            if let Some(i) = opened {
                let Source::Partition(dev) = &i.plan.source else { unreachable!() };
                let u = i.run.section(RootStep::Unmount);
                ui.horizontal(|ui| {
                    let (st, text) = match u.status {
                        Status::Done => {
                            (Status::Skipped, format!("{dev}: opened read-only during the install, closed again"))
                        }
                        Status::Pending if i.running() => {
                            (Status::Running, format!("{dev}: open read-only while installing"))
                        }
                        Status::Pending => (Status::Skipped, format!("{dev}: opened read-only during the install")),
                        st => (st, format!("{dev}: {}", u.message.as_deref().unwrap_or("couldn't be closed"))),
                    };
                    status_icon(ui, st, 16.0);
                    ui.label(RichText::new(text).color(if st == Status::Warning { WARN } else { WEAK }));
                });
            }
            for (i, m) in self.attached.iter().enumerate() {
                ui.horizontal(|ui| {
                    let what = format!("{} attached as {}", tilde(&m.image), m.loop_device);
                    match &m.released {
                        None => {
                            status_icon(ui, Status::Done, 16.0);
                            ui.label(RichText::new(what).color(TEXT));
                            if secondary(ui, "Detach image", !self.install_running()).clicked() {
                                act.push(Action::Release(i));
                            }
                        }
                        Some(Ok(())) => {
                            status_icon(ui, Status::Skipped, 16.0);
                            ui.label(RichText::new(format!("{what}: detached")).color(WEAK));
                        }
                        Some(Err(e)) => {
                            status_icon(ui, Status::Failed, 16.0);
                            ui.label(RichText::new(format!("{what}: {e}")).color(FAIL));
                        }
                    }
                });
            }
        });
        ui.add_space(6.0);
        if self.console_installed() && primary(ui, "Launch Console", true).clicked() {
            act.push(Action::Launch);
        }
        if let Some(Ok(m)) = &self.launched {
            ui.label(RichText::new(m).color(ACCENT));
        }
    }

    pub(super) fn result_of(&self, step: StepId) -> String {
        let st = self.steps.status(step);
        if st == Status::Pending {
            return "not run".into();
        }
        let section_msg = |id: RootStep| {
            let i = self.install.as_ref()?;
            let s = i.run.section(id);
            let m = s.message.clone()?;
            Some(if i.demo { format!("{m} (demo)") } else { m })
        };
        match step {
            StepId::System => match &self.sys {
                Some(r) if r.warnings() > 0 => format!("{} warning(s) with fixes", r.warnings()),
                Some(_) => "all good".into(),
                None => "not run".into(),
            },
            _ if st == Status::Skipped && step != StepId::Remove => "skipped".into(),
            StepId::Find => match (self.selected_found(), &self.selected_device) {
                (Some(f), _) => format!(
                    "{} on {}",
                    f.install
                        .console
                        .as_ref()
                        .and_then(|c| c.version.clone())
                        .map(|v| format!("UAD Console {v}"))
                        .unwrap_or_else(|| "UAD plug-ins".into()),
                    tilde(f.path())
                ),
                (None, Some(d)) => format!("Windows partition {d}"),
                (None, None) => "no UAD install found".into(),
            },
            StepId::Review => match &self.install {
                Some(i) => i.run.outcome.as_ref().map(|o| o.message.clone()).unwrap_or_else(|| "installing".into()),
                None => "not run".into(),
            },
            StepId::Remove if st == Status::Skipped => "kept the current install".into(),
            StepId::Remove => section_msg(RootStep::Remove).unwrap_or_else(|| "removed".into()),
            StepId::Import => match (section_msg(RootStep::Import), self.console_art()) {
                (Some(m), Some((_, files))) => format!("{m}: {files} files"),
                (Some(m), None) => m,
                (None, _) => "not run".into(),
            },
            StepId::Firmware => match &self.install {
                Some(i) if i.run.status(RootStep::Firmware).is_finished() => {
                    let s = i.run.section(RootStep::Firmware);
                    let ok = s.files.iter().filter(|f| f.status == Status::Done).count();
                    format!("{ok} file(s) in {}{}", firmware::FIRMWARE_DIR, if i.demo { " (demo)" } else { "" })
                }
                _ => "not run".into(),
            },
            StepId::Driver => section_msg(RootStep::Driver).unwrap_or_else(|| "not run".into()),
            StepId::Console => {
                match (&self.launched, self.console.as_ref().and_then(|r| r.progress.outcome.as_ref())) {
                    (Some(Ok(_)), _) => "installed and started".into(),
                    (_, Some(o)) => o.message.clone(),
                    _ => "installed".into(),
                }
            }
            _ => String::new(),
        }
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).small().strong().color(WEAK));
}

/// A few items of a group, then how many more.
fn summarize(items: &[&str]) -> String {
    const SHOWN: usize = 3;
    let home = std::env::var("HOME").unwrap_or_default();
    let short: Vec<String> = items
        .iter()
        .take(SHOWN)
        .map(|i| match i.strip_prefix(&home) {
            Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
            _ => i.to_string(),
        })
        .collect();
    let more = items.len().saturating_sub(SHOWN);
    if more > 0 { format!("{}, and {more} more", short.join(", ")) } else { short.join(", ") }
}

/// `/home/me/x` as `~/x`.
pub(super) fn tilde(p: &std::path::Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let s = p.display().to_string();
    match s.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => s,
    }
}

/// Sub-steps on the left, the section's log on the right.
fn two_columns(ui: &mut Ui, s: &Section, id: &str) {
    ui.columns(2, |cols| {
        card(&mut cols[0], false, |ui| substeps(ui, &s.progress.substeps));
        log_view(&mut cols[1], id, &s.progress.lines, 300.0);
    });
}

fn vm_help(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.label(RichText::new("Install UAD in a Windows virtual machine, then let setup read the VM's disk:").color(TEXT));
    let steps = [
        "Create a Windows VM: on Omarchy run omarchy-windows-vm; elsewhere GNOME Boxes or virt-manager work.",
        "In Windows, install UA Connect from uaudio.com, sign in, and install UAD (Console and plug-ins). No Apollo needs to be connected.",
        "Shut Windows down fully: Start, Power, Shut down (not sleep or hibernate). Hold Shift while clicking Shut down if Fast Startup is on.",
        "Here, choose \"Use a VM disk image…\" and pick its disk:",
    ];
    for (i, s) in steps.iter().enumerate() {
        ui.horizontal_top(|ui| {
            ui.label(RichText::new(format!("{}.", i + 1)).strong().color(ACCENT));
            ui.add(egui::Label::new(RichText::new(*s).color(TEXT)).wrap());
        });
    }
    facts(
        ui,
        &[
            ("Omarchy", "~/.windows/data.img".into()),
            ("GNOME Boxes", "~/.local/share/gnome-boxes/images/".into()),
            ("virt-manager", "/var/lib/libvirt/images/ (may need read access)".into()),
        ],
    );
    ui.label(RichText::new("qcow2, VDI, VMDK and VHDX disks must be converted to a raw image first (needs room for the full disk size):").small().color(WEAK));
    command(ui, "qemu-img convert -O raw windows.qcow2 windows.img");
}

fn no_install_blocker(ui: &mut Ui, act: &mut Acts, why: &str) {
    callout(
        ui,
        Status::Failed,
        "No UAD install picked",
        &format!(
            "{why} Go back to Find your UAD install: pick a Windows partition, a VM disk image, or choose a folder."
        ),
    );
    if secondary(ui, "Back to Find your UAD install", true).clicked() {
        act.push(Action::Goto(StepId::Find));
    }
}

/// After a failed install: how it ended, the command for a terminal, and the way back.
fn failed_help(ui: &mut Ui, act: &mut Acts, i: &Install, with_outcome: bool) {
    if i.running() {
        return;
    }
    if with_outcome && let Some(o) = &i.run.outcome {
        ui.label(RichText::new(&o.message).color(TEXT));
    }
    if !i.run.other.is_empty() && i.run.nothing_changed() {
        log_view(ui, "install-other", &i.run.other, 80.0);
    }
    log_hint(ui, i.log.as_deref());
    ui.label(RichText::new("You can also run it in a terminal to see everything:").small().color(WEAK));
    command(ui, &i.terminal);
    if secondary(ui, "Back to Review and install", true).clicked() {
        act.push(Action::Goto(StepId::Review));
    }
}

/// The end-of-run banner of the Console build, with its command when it failed.
fn run_outcome(ui: &mut Ui, run: &Run) {
    if let Some(o) = &run.progress.outcome {
        callout(ui, o.status, &o.message, "");
        if o.status == Status::Failed {
            log_hint(ui, run.log.as_deref());
            ui.label(RichText::new("You can also run it in a terminal to see everything:").small().color(WEAK));
            command(ui, &run.command);
        }
    }
}

/// Where the whole run's output is, for a bug report.
fn log_hint(ui: &mut Ui, log: Option<&std::path::Path>) {
    if let Some(log) = log {
        ui.label(
            RichText::new("Everything it printed is in this log (attach it to a bug report):").small().color(WEAK),
        );
        command(ui, &tilde(log));
    }
}

/// `3f1a9c0d2e4b…07c1fe` for a long ASCII hash.
fn short_hash(h: &str) -> String {
    if h.len() > 20 && h.is_ascii() { format!("{}…{}", &h[..12], &h[h.len() - 6..]) } else { h.to_string() }
}
