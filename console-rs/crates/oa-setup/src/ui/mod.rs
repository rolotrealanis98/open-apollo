//! The egui window: sidebar of steps, the current page, Back/Next.

mod app;
mod pages;
mod theme;
mod widgets;

use eframe::egui::{self, Align, CornerRadius, Frame, Key, Layout, Margin, RichText, ScrollArea, Stroke, Ui};

use app::Action;
pub use app::App;
use theme::*;
use widgets::*;

use crate::options::Options;
use crate::steps::{Status, StepId};

pub fn run(opts: Options) -> Result<(), String> {
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("open-apollo setup")
            .with_app_id("open-apollo-setup")
            .with_inner_size(opts.window_size)
            .with_min_inner_size([860.0, 560.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native("open-apollo-setup", native, Box::new(move |cc| Ok(Box::new(App::new(cc, opts)))))
        .map_err(|e| e.to_string())
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let mut act = Vec::new();
        egui::Panel::left("steps")
            .exact_size(258.0)
            .resizable(false)
            .frame(Frame::new().fill(SIDEBAR).inner_margin(Margin { left: 14, right: 14, top: 20, bottom: 14 }))
            .show(ui, |ui| self.sidebar(ui, &mut act));
        egui::Panel::bottom("nav")
            .exact_size(66.0)
            .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(32, 14)).stroke(Stroke::new(1.0, STROKE)))
            .show(ui, |ui| self.nav(ui, &mut act));
        egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin { left: 36, right: 28, top: 26, bottom: 0 }))
            .show(ui, |ui| {
                let step = self.steps.current();
                let (title, sub) = self.heading(step);
                ui.label(
                    RichText::new(format!("STEP {} OF {}", step.index() + 1, StepId::ALL.len()))
                        .small()
                        .strong()
                        .color(FAINT),
                );
                ui.label(RichText::new(title).size(26.0).strong().color(STRONG));
                ui.add(egui::Label::new(RichText::new(sub).color(WEAK)).wrap());
                ui.add_space(10.0);
                ScrollArea::vertical().id_salt(("page", step.index())).auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(820.0) - 8.0);
                    self.page(ui, &mut act);
                    ui.add_space(18.0);
                });
            });
        if self.confirm_close {
            egui::Modal::new(egui::Id::new("confirm-close")).show(ui.ctx(), |ui| {
                ui.set_max_width(420.0);
                ui.label(RichText::new("The installer is still running").strong().size(17.0).color(STRONG));
                ui.label("It runs with administrator rights and keeps going if you close setup. You won't see how it ends here.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if primary(ui, "Keep setup open", true).clicked() {
                        self.confirm_close = false;
                    }
                    if secondary(ui, "Close anyway", true).clicked() {
                        act.push(Action::Close);
                    }
                });
            });
        }
        // Enter: Next, or the step's main action.
        let enter =
            ui.input(|i| i.key_pressed(Key::Enter)) && ui.memory(|m| m.focused().is_none()) && !self.confirm_close;
        if enter {
            if self.steps.can_advance() {
                act.push(Action::Next);
            } else {
                self.primary(self.steps.current());
            }
        }
        for a in act {
            self.apply(a, ui.ctx());
        }
    }
}

impl App {
    fn sidebar(&self, ui: &mut Ui, act: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            logo(ui, 34.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("open-apollo").size(18.0).strong().color(STRONG));
                ui.label(RichText::new("Setup").color(WEAK));
            });
        });
        ui.add_space(22.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        for step in StepId::ALL {
            let selected = self.steps.current() == step;
            let enabled = self.steps.can_visit(step);
            let status = self.steps.status(step);
            let resp = sidebar_row(ui, selected, enabled, |ui| {
                status_icon(ui, status, 18.0);
                ui.add_space(4.0);
                let color = if selected {
                    STRONG
                } else if enabled {
                    TEXT
                } else {
                    FAINT
                };
                let text = RichText::new(step.title()).color(color);
                ui.label(if selected { text.strong() } else { text });
            });
            if resp.clicked() && !selected {
                act.push(Action::Goto(step));
            }
        }
        // Footer: demo notice, data folder, version, pinned to the bottom.
        egui::Panel::bottom("sidebar-footer").frame(Frame::new()).resizable(false).show_separator_line(false).show(
            ui,
            |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                if self.opts.demo {
                    Frame::new()
                        .fill(tint(INFO, 30))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("DEMO MODE").small().strong().color(INFO));
                            ui.label(
                                RichText::new("Firmware, driver and Console steps are simulated.").small().color(TEXT),
                            );
                        });
                }
                if !self.opts.data_is_default {
                    let data = pages::tilde(&self.opts.data);
                    ui.add(
                        egui::Label::new(RichText::new(format!("Importing to {data}")).small().color(FAINT)).truncate(),
                    )
                    .on_hover_text(data);
                }
                ui.label(RichText::new(format!("oa-setup {}", env!("CARGO_PKG_VERSION"))).small().color(FAINT));
            },
        );
    }

    fn nav(&self, ui: &mut Ui, act: &mut Vec<Action>) {
        ui.horizontal_centered(|ui| {
            let cur = self.steps.current();
            if cur != StepId::Welcome && secondary(ui, "Back", true).clicked() {
                act.push(Action::Back);
            }
            let status = self.steps.status(cur);
            if status == Status::Running {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(RichText::new("Working…").color(WEAK));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = match cur {
                    StepId::Welcome => "Get started",
                    StepId::Find if self.selected_found().is_some() => "Use this install",
                    StepId::Find if !self.scanning => "Continue without one",
                    StepId::Console => "Finish",
                    StepId::Done => "Close",
                    _ if status == Status::Failed => "Continue anyway",
                    _ => "Next",
                };
                let enabled = cur == StepId::Welcome || cur == StepId::Done || self.steps.can_advance();
                let r = primary(ui, label, enabled);
                if r.clicked() {
                    act.push(Action::Next);
                }
                if !enabled && status != Status::Running {
                    ui.label(RichText::new(self.waiting_for(cur)).small().color(FAINT));
                }
            });
        });
    }

    /// Why Next is disabled.
    fn waiting_for(&self, step: StepId) -> &'static str {
        match step {
            StepId::Import => "Import, or skip, to continue",
            StepId::Firmware => "Extract the files to continue",
            StepId::Driver => "Install the driver, or skip, to continue",
            StepId::Console => "Install or launch the Console, or skip",
            _ => "",
        }
    }
}
