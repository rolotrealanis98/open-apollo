//! Drawing each step. Pages only read state and push [`Action`]s.

use eframe::egui::{self, CollapsingHeader, ProgressBar, RichText, Ui};

use super::app::{Action, App, Mounted, Run};
use super::theme::*;
use super::widgets::*;
use crate::firmware;
use crate::importer::{Item, State as ImportState};
use crate::scan::{Found, Origin, human_size};
use crate::steps::{Status, StepId};
use crate::syscheck::Level;

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
            StepId::Import => {
                ("Import assets", format!("Copy the Console art from your UAD install to {}.", tilde(&self.opts.data)))
            }
            StepId::Firmware => (
                "DSP programs & firmware",
                "Extract what the driver loads onto your Apollo, from your own UAD install.".into(),
            ),
            StepId::Driver => {
                ("Install the driver", "Build the open-apollo driver and set up audio for your Apollo.".into())
            }
            StepId::Console => {
                ("Console", "Build and start the open-apollo Console, the mixer for your Apollo.".into())
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
                (StepId::Import, "Copy the Console art to this computer."),
                (StepId::Firmware, "Extract the DSP programs and firmware the driver loads. Asks for your password."),
                (StepId::Driver, "Build and install the driver and audio settings. Asks for your password."),
                (StepId::Console, "Build and start the open-apollo Console."),
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
                "The firmware, driver and Console steps only simulate their work: no password prompts, no builds, nothing installed. Scanning and the art import are real.",
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
                        ui.horizontal(|ui| {
                            ui.add_sized([170.0, 18.0], egui::Label::new(RichText::new(c.title).strong().color(TEXT)));
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
        if with.is_empty() {
            callout(
                ui,
                Status::Warning,
                "No UAD install found yet",
                "If Windows is on another partition, open it below. No Windows on this computer? See the help at the bottom.",
            );
        } else {
            section(ui, "UAD installs found");
            for f in with {
                self.found_card(ui, f, act);
            }
        }
        let parts: Vec<_> = scan.likely_windows().collect();
        if !parts.is_empty() {
            section(ui, "Windows partitions not opened yet");
            for u in parts {
                unmounted_card(ui, u, self.busy.is_none(), act);
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
                        if !u.is_bitlocker() && ui.small_button("Open read-only").clicked() {
                            act.push(Action::Mount(u.device.clone()));
                        }
                    });
                }
            });
        }
        ui.add_space(6.0);
        CollapsingHeader::new(RichText::new("No Windows on this computer?").strong().color(TEXT))
            .id_salt("vmhelp")
            .show(ui, vm_help);
        ui.add_space(4.0);
        if !self.scanning && self.selected_found().is_none() {
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
                if self.mounted.iter().any(|m| matches!(&m.what, Mounted::Partition { at, .. } if at == f.path())) {
                    badge(ui, "opened by setup", WEAK);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.radio(selected, if selected { "Using this one" } else { "Use this one" }).clicked()
                        && !selected
                    {
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
            facts(
                ui,
                &format!("facts-{}", f.path().display()),
                &[
                    (
                        "UAD Console",
                        i.console
                            .as_ref()
                            .map(|c| c.version.clone().unwrap_or_else(|| "installed".into()))
                            .unwrap_or_else(|| "not installed".into()),
                    ),
                    ("Powered plug-ins", count(i.powered_plugins.len(), "plug-in file")),
                    ("UADx plug-ins", count(i.vst3.len(), "VST3 bundle")),
                    ("UA drivers", if i.drivers.is_empty() { "none".into() } else { count(i.drivers.len(), "file") }),
                ],
            );
            if i.console.is_none() {
                ui.label(RichText::new("No UAD Console here, so there's no Console art to import. The plug-in files still give the driver its DSP programs.").small().color(WARN));
            }
        });
    }

    // ---- 4. Import ----

    fn import_page(&self, ui: &mut Ui, act: &mut Acts) {
        let Some(found) = self.selected_found().filter(|_| self.steps.blocker(StepId::Import).is_none()) else {
            callout(
                ui,
                Status::Failed,
                "No UAD install picked",
                "Go back to Find your UAD install and pick one, or skip importing (the Console needs the art to draw its mixer).",
            );
            ui.horizontal(|ui| {
                if secondary(ui, "Back to Find", true).clicked() {
                    act.push(Action::Goto(StepId::Find));
                }
                if secondary(ui, "Skip importing", self.steps.status(StepId::Import) != Status::Skipped).clicked() {
                    act.push(Action::SkipImport);
                }
            });
            return;
        };
        card(ui, false, |ui| {
            let from = match &found.install.console {
                Some(c) => format!(
                    "UAD Console {} on {}",
                    c.version.as_deref().unwrap_or(""),
                    found.origin.label().to_lowercase()
                ),
                None => format!("{} (no UAD Console)", found.origin.label()),
            };
            facts(
                ui,
                "import-facts",
                &[
                    ("From", from),
                    ("", tilde(found.path())),
                    ("To", tilde(&self.opts.data.join(oa_assets::import::CONSOLE_DIR))),
                ],
            );
        });
        let status = self.steps.status(StepId::Import);
        if let Some((ver, files)) = self.console_art().filter(|_| status == Status::Pending && !self.importing) {
            callout(
                ui,
                Status::Done,
                &format!(
                    "Console art is already imported (Console {}, {files} files)",
                    ver.as_deref().unwrap_or("version unknown")
                ),
                "Use it as it is, or import again to refresh it from this install.",
            );
        }
        for (item, state) in &self.import {
            card(ui, false, |ui| {
                ui.horizontal(|ui| {
                    let st = match state {
                        ImportState::Waiting => Status::Pending,
                        ImportState::ComingSoon | ImportState::NotInInstall(_) => Status::Skipped,
                        ImportState::Copying { .. } => Status::Running,
                        ImportState::Done { .. } => Status::Done,
                        ImportState::Failed(_) => Status::Failed,
                    };
                    status_icon(ui, st, 18.0);
                    ui.label(RichText::new(item.title()).strong().color(STRONG));
                    if *state == ImportState::ComingSoon {
                        badge(ui, "coming soon", INFO);
                    }
                });
                ui.label(RichText::new(item.description()).color(WEAK));
                match state {
                    ImportState::Copying { files, bytes, total_files, total_bytes } => {
                        ui.add(
                            ProgressBar::new(state.fraction().unwrap_or(0.0))
                                .fill(ACCENT)
                                .desired_height(10.0)
                                .corner_radius(5),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{files} of {total_files} files, {} of {}",
                                human_size(*bytes),
                                human_size(*total_bytes)
                            ))
                            .small()
                            .color(WEAK),
                        );
                    }
                    ImportState::Done { files, bytes, dest } => {
                        ui.label(
                            RichText::new(format!("Copied {files} files ({}) to {}", human_size(*bytes), tilde(dest)))
                                .color(ACCENT),
                        );
                    }
                    ImportState::Failed(e) => {
                        ui.label(RichText::new(e).color(FAIL));
                    }
                    ImportState::NotInInstall(why) => {
                        ui.label(RichText::new(why).small().color(WARN));
                    }
                    ImportState::ComingSoon if *item == Item::PluginPanels => {
                        ui.label(RichText::new("Not in this version yet. The Console works without them; plug-in windows show a plain panel.").small().color(FAINT));
                    }
                    _ => {}
                }
            });
        }
        if self.used_previous_import {
            callout(ui, Status::Done, "Using the Console art imported earlier.", "");
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let has_art = found.has_console();
            match status {
                Status::Running => {}
                Status::Pending => {
                    if primary(ui, "Import", has_art).clicked() {
                        act.push(Action::StartImport);
                    }
                    if self.console_art().is_some() && secondary(ui, "Use the earlier import", true).clicked() {
                        act.push(Action::UsePreviousImport);
                    }
                    if secondary(ui, "Skip", true).clicked() {
                        act.push(Action::SkipImport);
                    }
                }
                _ => {
                    if secondary(ui, "Import again", has_art).clicked() {
                        act.push(Action::StartImport);
                    }
                }
            }
        });
    }

    // ---- 5. DSP programs & firmware ----

    fn firmware_page(&self, ui: &mut Ui, act: &mut Acts) {
        let Some(found) = self.selected_found().filter(|_| self.steps.blocker(StepId::Firmware).is_none()) else {
            no_install_blocker(
                ui,
                act,
                "The driver loads DSP programs and firmware that come from your own UAD install, and no install is picked.",
            );
            return;
        };
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "Your Apollo runs DSP programs and firmware written by Universal Audio. open-apollo doesn't ship them, so this step extracts them from your install into {}, with a manifest listing each file's SHA-256. Writing there needs administrator rights, so your password is asked.",
                    firmware::FIRMWARE_DIR
                ))
                .color(TEXT),
            )
            .wrap(),
        );
        card(ui, false, |ui| {
            let i = &found.install;
            facts(
                ui,
                "fw-facts",
                &[
                    ("From", format!("{} ({})", tilde(found.path()), found.origin.label().to_lowercase())),
                    ("Plug-in files", count(i.powered_plugins.len(), "Powered plug-in DLL")),
                    ("UA drivers", count(i.drivers.len(), "file")),
                    ("To", format!("{}  (read by the driver)", firmware::FIRMWARE_DIR)),
                ],
            );
        });
        if found.install.powered_plugins.is_empty() && found.install.drivers.is_empty() {
            callout(
                ui,
                Status::Warning,
                "This install has no plug-in or driver files",
                "The DSP programs come from the Powered plug-ins. Install UAD's plug-ins in Windows (UA Connect), or pick another install.",
            );
        }
        let status = self.steps.status(StepId::Firmware);
        let existing = firmware::existing(std::path::Path::new(firmware::FIRMWARE_DIR));
        if let Some(e) = existing.as_ref().filter(|_| status == Status::Pending && self.firmware.is_none()) {
            let from = e.source.as_deref().map(|s| format!(" from {s}")).unwrap_or_default();
            callout(
                ui,
                Status::Done,
                &format!("Already extracted earlier: {} files{from}", e.files),
                "Keep them, or extract again from this install.",
            );
        }
        let unavailable = !self.opts.demo && self.extractor_ok != Some(true);
        if unavailable && self.firmware.is_none() {
            let body = match (&self.extractor, self.extractor_ok) {
                (None, _) => "The oa-assets program wasn't found next to setup or on your PATH. Build it with: cargo build -p oa-assets".to_string(),
                (Some(_), None) => "Checking the extractor…".to_string(),
                (Some(x), Some(_)) => format!("{} has no firmware command yet. It's being finished separately; until then the driver can't be installed from here.", x.program.display()),
            };
            callout(ui, Status::Warning, "The firmware extractor isn't available in this build", &body);
        }
        if let Some(run) = &self.firmware {
            if run.demo {
                badge(ui, "DEMO: simulated, nothing written", INFO);
            }
            if !run.files.is_empty() {
                card(ui, false, |ui| {
                    egui::Grid::new("fw-files").num_columns(3).spacing([14.0, 6.0]).show(ui, |ui| {
                        for f in &run.files {
                            ui.horizontal(|ui| {
                                status_icon(ui, f.status, 16.0);
                                ui.label(RichText::new(&f.name).monospace().color(STRONG));
                            });
                            let sha = f
                                .sha256
                                .as_deref()
                                .map(|h| {
                                    format!("sha256 {}…{}", &h[..h.len().min(12)], &h[h.len().saturating_sub(6)..])
                                })
                                .unwrap_or_default();
                            ui.label(RichText::new(sha).monospace().small().color(WEAK));
                            ui.label(RichText::new(f.note.as_deref().unwrap_or("")).small().color(WEAK));
                            ui.end_row();
                        }
                    });
                });
            }
            run_status(ui, run, "firmware-log", None);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let running = self.firmware.as_ref().is_some_and(Run::running);
            if !running {
                let label = if self.firmware.is_some() { "Extract again" } else { "Extract (asks for your password)" };
                let enabled = !unavailable;
                let r = if self.firmware.is_some() || existing.is_some() {
                    secondary(ui, label, enabled)
                } else {
                    primary(ui, label, enabled)
                };
                if r.clicked() {
                    act.push(Action::StartFirmware);
                }
                if existing.is_some()
                    && status == Status::Pending
                    && secondary(ui, "Keep the earlier files", true).clicked()
                {
                    act.push(Action::KeepFirmware);
                }
            }
        });
    }

    // ---- 6. Driver ----

    fn driver_page(&self, ui: &mut Ui, act: &mut Acts) {
        if let Some(b) = self.steps.blocker(StepId::Driver) {
            if self.selected_found().is_none() || self.steps.blocker(b).is_some() {
                no_install_blocker(
                    ui,
                    act,
                    "The driver loads DSP programs and firmware extracted from your own UAD install, and setup hasn't found one. Without them the Apollo can't run.",
                );
            } else {
                callout(
                    ui,
                    Status::Failed,
                    "DSP programs & firmware come first",
                    "The driver loads the DSP programs and firmware extracted from your UAD install. Run that step, then come back.",
                );
                if secondary(ui, "Go to DSP programs & firmware", true).clicked() {
                    act.push(Action::Goto(StepId::Firmware));
                }
            }
            if self.steps.status(StepId::Driver) == Status::Pending && ui.link("Skip the driver for now").clicked() {
                act.push(Action::SkipDriver);
            }
            return;
        }
        ui.add(
            egui::Label::new(
                RichText::new("Runs open-apollo's installer (scripts/install.sh) with administrator rights. It installs missing packages, builds the ua_apollo driver, registers it with DKMS so kernel updates rebuild it, installs PipeWire, WirePlumber and UCM2 settings, and, if your Apollo is on, loads the driver and starts the mixer daemon.")
                    .color(TEXT),
            )
            .wrap(),
        );
        ui.add(
            egui::Label::new(
                RichText::new("Its questions get their default answers: missing packages are installed, DKMS is set up, on Intel VT-d systems iommu=pt is added to GRUB (a restart is needed after), and the install report stays on this computer.")
                    .small()
                    .color(WEAK),
            )
            .wrap(),
        );
        if let Some(sys) = &self.sys {
            if sys.apollo.pci.is_empty() && !sys.apollo.usb.is_empty() {
                callout(
                    ui,
                    Status::Warning,
                    "Your Apollo is a USB model",
                    "USB Apollos use scripts/install-usb.sh. This step installs the Thunderbolt driver.",
                );
            } else if sys.apollo.pci.is_empty() && self.driver.is_none() {
                callout(
                    ui,
                    Status::Skipped,
                    "No Apollo connected",
                    "That's fine: it's set up when you connect it. To have the installer verify the driver now, power on the Apollo, connect it, and wait ~20 s before starting.",
                );
            }
            if sys.driver.is_some() && self.driver.is_none() {
                callout(
                    ui,
                    Status::Done,
                    "The driver is already installed",
                    "Running the installer again updates it to this version.",
                );
            }
        }
        if !self.opts.demo && self.opts.repo.is_none() {
            callout(
                ui,
                Status::Failed,
                "Can't find the open-apollo folder",
                "Setup runs scripts/install.sh from your open-apollo checkout. Start setup from inside it, or pass --repo DIR.",
            );
        }
        if let Some(run) = &self.driver {
            if run.demo {
                badge(ui, "DEMO: simulated output, nothing installed", INFO);
            }
            ui.columns(2, |cols| {
                card(&mut cols[0], false, |ui| substeps(ui, &run.progress.substeps));
                cols[1].vertical(|ui| {
                    if let Some(cur) = run.progress.current().filter(|_| run.running()) {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new(&cur.label).strong().color(STRONG));
                        });
                    }
                    log_view(ui, "driver-log", &run.progress.lines, 300.0);
                });
            });
            let fallback = self.driver_command().map(|(_, _, t)| t);
            run_status(ui, run, "", fallback.as_deref());
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let running = self.driver.as_ref().is_some_and(Run::running);
            let possible = self.opts.demo || self.opts.repo.is_some();
            if !running {
                let r = if self.driver.is_some() { secondary(ui, "Run again", possible) } else { primary(ui, "Install driver (asks for your password)", possible) };
                if r.clicked() {
                    act.push(Action::StartDriver);
                }
                if self.steps.status(StepId::Driver) == Status::Pending && secondary(ui, "Skip", true).clicked() {
                    act.push(Action::SkipDriver);
                }
            } else if !self.opts.demo {
                ui.label(RichText::new("It runs with administrator rights, so it can't be stopped from here; closing setup leaves it running.").small().color(WEAK));
            }
        });
    }

    // ---- 7. Console ----

    fn console_page(&self, ui: &mut Ui, act: &mut Acts) {
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
                    if ui.link("Import assets").clicked() {
                        act.push(Action::Goto(StepId::Import));
                    }
                }
            });
            let installed = if self.opts.demo {
                self.console.as_ref().is_some_and(Run::exit_ok)
            } else {
                self.console_bin.is_some()
            };
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
        });
        let needs_build =
            if self.opts.demo { !self.console.as_ref().is_some_and(Run::exit_ok) } else { self.console_bin.is_none() };
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
            ui.columns(2, |cols| {
                card(&mut cols[0], false, |ui| substeps(ui, &run.progress.substeps));
                log_view(&mut cols[1], "console-log", &run.progress.lines, 240.0);
            });
            run_status(ui, run, "", None);
        }
        match &self.launched {
            Some(Ok(m)) => callout(
                ui,
                Status::Done,
                m,
                "It connects to the mixer daemon the driver step started (127.0.0.1:4710).",
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

    // ---- 8. Done ----

    fn done(&self, ui: &mut Ui, act: &mut Acts) {
        card(ui, false, |ui| {
            for step in &StepId::ALL[1..StepId::ALL.len() - 1] {
                ui.horizontal(|ui| {
                    status_icon(ui, self.steps.status(*step), 18.0);
                    ui.add_sized([190.0, 18.0], egui::Label::new(RichText::new(step.title()).strong().color(TEXT)));
                    ui.add(egui::Label::new(RichText::new(self.result_of(*step)).color(WEAK)).wrap());
                });
            }
        });
        if self.driver.as_ref().is_some_and(|d| d.progress.reboot_needed) {
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
                rows.push(("", "Install report: /tmp/open-apollo-install-report.json (not uploaded)".into()));
            }
            if self.steps.status(StepId::Console).is_success() {
                rows.push(("Console", "~/.local/bin/uaconsole, and \"Open Apollo Console\" in your app menu".into()));
            }
            if rows.is_empty() {
                ui.label(RichText::new("Nothing was installed or copied.").color(WEAK));
            } else {
                facts(ui, "where", &rows);
            }
        });
        section(ui, "Your Windows disk");
        card(ui, false, |ui| {
            if self.mounted.is_empty() {
                ui.label(
                    RichText::new(
                        "Setup didn't open any disk itself; disks that were already open were left as they were.",
                    )
                    .color(WEAK),
                );
            }
            for (i, m) in self.mounted.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (what, button) = match &m.what {
                        Mounted::Partition { device, at } => {
                            (format!("{device} opened read-only at {}", tilde(at)), "Unmount")
                        }
                        Mounted::Image { image, loop_device } => {
                            (format!("{} attached as {loop_device}", tilde(image)), "Detach image")
                        }
                    };
                    match &m.released {
                        None => {
                            status_icon(ui, Status::Done, 16.0);
                            ui.label(RichText::new(what).color(TEXT));
                            if secondary(ui, button, true).clicked() {
                                act.push(Action::Release(i));
                            }
                        }
                        Some(Ok(())) => {
                            status_icon(ui, Status::Skipped, 16.0);
                            ui.label(RichText::new(format!("{what}: released")).color(WEAK));
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
        let installed =
            if self.opts.demo { self.console.as_ref().is_some_and(Run::exit_ok) } else { self.console_bin.is_some() };
        if installed && primary(ui, "Launch Console", true).clicked() {
            act.push(Action::Launch);
        }
        if let Some(Ok(m)) = &self.launched {
            ui.label(RichText::new(m).color(ACCENT));
        }
    }

    pub(super) fn result_of(&self, step: StepId) -> String {
        let st = self.steps.status(step);
        if st == Status::Skipped {
            return "skipped".into();
        }
        if st == Status::Pending {
            return "not run".into();
        }
        let outcome = |r: &Option<Run>| r.as_ref().and_then(|r| r.progress.outcome.as_ref()).map(|o| o.message.clone());
        match step {
            StepId::System => match &self.sys {
                Some(r) if r.warnings() > 0 => format!("{} warning(s) with fixes", r.warnings()),
                Some(_) => "all good".into(),
                None => "not run".into(),
            },
            StepId::Find => match self.selected_found() {
                Some(f) => format!(
                    "{} on {}",
                    f.install
                        .console
                        .as_ref()
                        .and_then(|c| c.version.clone())
                        .map(|v| format!("UAD Console {v}"))
                        .unwrap_or_else(|| "UAD plug-ins".into()),
                    tilde(f.path())
                ),
                None => "no UAD install found".into(),
            },
            StepId::Import => match self.import.iter().find(|(i, _)| *i == Item::ConsoleArt) {
                Some((_, ImportState::Done { files, bytes, .. })) => {
                    format!("Console art: {files} files, {}", human_size(*bytes))
                }
                Some((_, ImportState::Failed(e))) => e.clone(),
                _ if self.used_previous_import => "using the earlier import".into(),
                _ => "nothing imported".into(),
            },
            StepId::Firmware if self.kept_firmware => "kept the files extracted earlier".into(),
            StepId::Firmware => match &self.firmware {
                Some(r) if !r.running() => {
                    let ok = r.files.iter().filter(|f| f.status == Status::Done).count();
                    format!("{ok} file(s) in {}{}", firmware::FIRMWARE_DIR, if r.demo { " (demo)" } else { "" })
                }
                _ => "not run".into(),
            },
            StepId::Driver => outcome(&self.driver)
                .map(|m| if self.opts.demo { format!("{m} (demo)") } else { m })
                .unwrap_or_else(|| "not run".into()),
            StepId::Console => match (&self.launched, outcome(&self.console)) {
                (Some(Ok(_)), _) => "installed and started".into(),
                (_, Some(m)) => m,
                _ => "installed".into(),
            },
            _ => String::new(),
        }
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title.to_uppercase()).small().strong().color(WEAK));
}

fn count(n: usize, what: &str) -> String {
    match n {
        0 => "none".into(),
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
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

fn unmounted_card(ui: &mut Ui, u: &oa_assets::discover::Unmounted, idle: bool, act: &mut Acts) {
    card(ui, false, |ui| {
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
                    if secondary(ui, "Open read-only", idle).clicked() {
                        act.push(Action::Mount(u.device.clone()));
                    }
                });
            }
        });
        if u.is_bitlocker() {
            ui.add(egui::Label::new(RichText::new(
                "This partition is encrypted with BitLocker, so Linux can't read it as it is. Either unlock it in your file manager with its 48-digit recovery key (find it at account.microsoft.com/devices/recoverykey), then press Rescan; or start Windows, copy \"C:\\Program Files\\Universal Audio\" and \"C:\\Program Files (x86)\\Universal Audio\" to a USB stick keeping those folder names, and use Choose a folder.",
            ).small().color(TEXT)).wrap());
        } else {
            ui.label(RichText::new("Opens it read-only to look for UAD software. Your password may be asked; nothing on it is changed.").small().color(WEAK));
        }
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
        "vm-paths",
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
        "No UAD install found",
        &format!(
            "{why} Go back to Find your UAD install: open your Windows partition, pick a VM disk image, or choose a folder."
        ),
    );
    if secondary(ui, "Back to Find your UAD install", true).clicked() {
        act.push(Action::Goto(StepId::Find));
    }
}

/// The end-of-run banner, with a terminal fallback command when it failed.
fn run_status(ui: &mut Ui, run: &Run, log_id: &str, fallback: Option<&str>) {
    if !log_id.is_empty() {
        CollapsingHeader::new(RichText::new("Output").color(WEAK)).id_salt(log_id).default_open(run.running()).show(
            ui,
            |ui| {
                log_view(ui, log_id, &run.progress.lines, 200.0);
            },
        );
    }
    if let Some(o) = &run.progress.outcome {
        callout(ui, o.status, &o.message, "");
        if o.status == Status::Failed {
            ui.label(RichText::new("You can also run it in a terminal to see everything:").small().color(WEAK));
            command(ui, fallback.unwrap_or(&run.command));
        }
        if run.progress.reboot_needed {
            callout(
                ui,
                Status::Warning,
                "Restart needed",
                "The installer added iommu=pt to the kernel command line. Restart before using the Apollo.",
            );
        }
    }
}
