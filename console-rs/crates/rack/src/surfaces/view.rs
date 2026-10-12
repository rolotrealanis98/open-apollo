//! UI-only state the app shell owns (`ViewState`) and the routing of pointer actions between
//! it and the [`MixerModel`].
//!
//! Shell loop:
//! ```ignore
//! let mut ui = Ui::new(&assets);
//! let layout = build_window_with(&mut ui, &model, &view);
//! // pointer down/move/up:
//! for a in interaction.pointer_down(&ui.hits, x, y, double) { view.dispatch(a, &mut model) }
//! // right click / long press:     view.context_click(&ui.hits, x, y)
//! // wheel:                         view.wheel(&ui.scrolls, x, y, dy)
//! // keys while view.editing():     view.key_text(s) / key_backspace() / key_enter(&mut model) / key_escape()
//! ```

use std::sync::Arc;

use crate::input::{Action, Control, ControlId, Hit, ScrollRegion, ScrollTarget};
use crate::model::{MixerModel, StripInfo};
use crate::settings::{Panel, SettingOp, SettingsTab};

/// Cell rows toggled by the navigator (and FADER, the main strip's fader area).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Inputs,
    Inserts,
    Sends,
    Fader,
}

/// Insert slot pitch: LARGE 50 pt, SMALL 23 pt (`Rack_Inserts` vslot15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InsertSize {
    #[default]
    Large,
    Small,
}

impl InsertSize {
    pub fn pitch(self) -> f32 {
        match self {
            InsertSize::Large => 50.0,
            InsertSize::Small => 23.0,
        }
    }
}

/// Navigator MODIFIERS: what a tap on an insert slot (or name plate) does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Power,
    Remove,
    Copy,
    Isolate,
    SetDefault,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BrowserTab {
    #[default]
    Plugins,
    Presets,
}

/// Monitor column SHOW: the bus area shows the aux returns or the control room.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MonitorPage {
    #[default]
    Aux,
    ControlRoom,
}

/// Which cell rows are expanded (collapsed = header only) and whether faders are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sections {
    pub inputs: bool,
    pub inserts: bool,
    pub sends: bool,
    pub fader: bool,
}

impl Default for Sections {
    fn default() -> Self {
        // Console 1.3.1 opens with the input SENDS row collapsed (both reference pairs).
        Self { inputs: true, inserts: true, sends: false, fader: true }
    }
}

impl Sections {
    pub fn get(&self, s: Section) -> bool {
        match s {
            Section::Inputs => self.inputs,
            Section::Inserts => self.inserts,
            Section::Sends => self.sends,
            Section::Fader => self.fader,
        }
    }
    pub fn set(&mut self, s: Section, v: bool) {
        match s {
            Section::Inputs => self.inputs = v,
            Section::Inserts => self.inserts = v,
            Section::Sends => self.sends = v,
            Section::Fader => self.fader = v,
        }
    }
}

/// Plug-in browser UI state.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserView {
    pub tab: BrowserTab,
    /// Live search filter (case-insensitive substring of the name).
    pub search: String,
    pub categories_open: bool,
    /// Index into `plugin_categories()`.
    pub category: Option<u8>,
    pub favorites_only: bool,
    /// Icon rows (48 pt) vs. compact text rows.
    pub large_rows: bool,
    /// "DRAG TO AN INSERT TO ASSIGN" hint visible.
    pub hint: bool,
    /// "Universal Audio" group expanded.
    pub group_open: bool,
    /// "Universal Audio (Inactive)" group expanded.
    pub inactive_open: bool,
    /// List scroll, 0…1 of the overflow.
    pub scroll: f32,
    /// Selected catalog index; while set, the next insert tap assigns it (assign mode).
    pub selected: Option<u16>,
}

impl Default for BrowserView {
    fn default() -> Self {
        Self { tab: BrowserTab::Plugins, search: String::new(), categories_open: true, category: None, favorites_only: false, large_rows: true, hint: true, group_open: true, inactive_open: true, scroll: 0.0, selected: None }
    }
}

/// What a popover shows. Drawn by `surfaces::popover::draw_popover`, anchored to the hit
/// rect of `PopoverState::anchor` in the same frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Popover {
    /// MIC / LINE / HI-Z picker for a preamp strip.
    InputSource { strip: Arc<str> },
    /// Open / Power / Remove / Copy for an insert slot (255 = Unison).
    InsertMenu { strip: Arc<str>, slot: u8 },
    /// Channel name editor (uses `ViewState::text_edit`).
    Rename { strip: Arc<str> },
    CueOutputs,
    Headphone { output: u8 },
    SampleRate,
    ClockSource,
    BufferSize,
    Scenes,
    /// Tempo editor (uses `ViewState::text_edit`).
    Tempo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PopoverState {
    pub kind: Popover,
    /// The control whose hit rect the popover points at.
    pub anchor: ControlId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextTarget {
    Rename(Arc<str>),
    Search,
    Tempo,
    /// A settings text field (DEVICE NAME), by field key.
    Setting(Arc<str>),
}

/// A text field with keyboard focus.
#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub target: TextTarget,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    Settings,
    /// A secondary panel (CUE OUTPUTS, MONITOR CONTROLLER, …).
    Panel(Panel),
}

/// Settings window and secondary-panel UI state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsView {
    pub tab: SettingsTab,
    /// HARDWARE › DEVICES selection (engine device index).
    pub device: u8,
    /// The settings combo whose dropdown is open (field key).
    pub menu: Option<Arc<str>>,
    /// The open panel's tab (MONITOR CONTROLLER: 0 BASS MANAGEMENT, 1 SPEAKER UTILITIES;
    /// HEADPHONE CORRECTION: HP 1 / HP 2).
    pub panel_tab: u8,
    /// Page / list scroll, 0…1 of the overflow.
    pub scroll: f32,
    /// PLUG-IN SCENES: the selected (primed) scene.
    pub scene: Option<u16>,
}

/// UI-only state: owned by the app shell, read by `build_window_with`, changed by `route`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    /// No browser / navigator; aux and monitor packed after the last strip.
    pub compact: bool,
    pub sections: Sections,
    pub insert_size: InsertSize,
    pub fixed_slots: bool,
    pub sends_overview: bool,
    pub modifier: Option<Modifier>,
    pub show_browser: bool,
    pub browser: BrowserView,
    pub monitor_page: MonitorPage,
    /// Cells column scroll, 0…1 of the overflow.
    pub cells_scroll: f32,
    /// First visible input strip (session navigator); clamped when drawn.
    pub first_strip: usize,
    pub popover: Option<PopoverState>,
    pub text_edit: Option<TextEdit>,
    pub sheet: Option<Sheet>,
    pub settings: SettingsView,
    /// Window size in points; the full layout follows it (None: Console's 1920 × 1050).
    pub window: Option<(f32, f32)>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            compact: false,
            sections: Sections::default(),
            insert_size: InsertSize::Large,
            fixed_slots: true,
            sends_overview: false,
            modifier: None,
            show_browser: true,
            browser: BrowserView::default(),
            monitor_page: MonitorPage::Aux,
            cells_scroll: 0.0,
            first_strip: 0,
            popover: None,
            text_edit: None,
            sheet: None,
            settings: SettingsView::default(),
            window: None,
        }
    }
}

/// What `route` asks of the model.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Model(Action),
    SetText(ControlId, String),
}

/// Controls consumed by `ViewState` (never reach the model).
pub fn is_view_control(c: &Control) -> bool {
    use Control as C;
    matches!(
        c,
        C::Section(_)
            | C::SectionsOpenAll
            | C::SectionsCloseAll
            | C::InsertSize(_)
            | C::FixedSlots
            | C::SendsOverview
            | C::Modifier(_)
            | C::ShowBrowser
            | C::ShowPluginScenes
            | C::ShowSettings
            | C::Name
            | C::BrowserTab(_)
            | C::BrowserSearch
            | C::BrowserCategoriesReveal
            | C::BrowserCategory(_)
            | C::BrowserFavorites
            | C::BrowserLargeRows
            | C::BrowserHintClose
            | C::BrowserGroupReveal
            | C::BrowserInactiveReveal
            | C::BrowserScroll
            | C::BrowserItem(_)
            | C::SessionNavigate(_)
            | C::CellsScroll
            | C::StatusTempo
            | C::StatusRate
            | C::StatusClock
            | C::StatusScene
            | C::MonitorPage(_)
            | C::CueOutputs
            | C::HeadphoneMenu(_)
            | C::PopoverDismiss
            | C::TextCommit
            | C::TextCancel
            | C::SettingsClose
            | C::SettingsRate
            | C::SettingsClock
            | C::SettingsBuffer
            | C::SettingsTab(_)
            | C::SettingsDevice(_)
            | C::SettingMenu(_)
            | C::SettingEdit(_)
            | C::ShowPanel(_)
            | C::PanelClose
            | C::PanelTab(_)
            | C::SceneSelect(_)
            | C::SceneStep(_)
            | C::SettingsScroll
    ) || matches!(c, C::Preamp(p) if &**p == "input_source")
}

fn find_strip(model: &dyn MixerModel, id: &str) -> Option<StripInfo> {
    model.inputs().into_iter().chain(model.auxes()).find(|s| &*s.id == id)
}

impl ViewState {
    pub fn compact() -> Self {
        Self { compact: true, ..Self::default() }
    }

    /// A text field has keyboard focus.
    pub fn editing(&self) -> bool {
        self.text_edit.is_some()
    }

    fn open(&mut self, kind: Popover, anchor: ControlId) {
        self.popover = Some(PopoverState { kind, anchor });
    }

    /// Routes one pointer action: UI-only controls change `self`; the rest come back as model
    /// effects (possibly translated by the active modifier or browser assign mode).
    pub fn route(&mut self, action: Action, model: &dyn MixerModel) -> Vec<Effect> {
        use Control as C;
        let id = action.id().clone();
        let press = matches!(action, Action::Tap(_) | Action::Toggle(_));
        if is_view_control(&id.control) {
            if let Action::Set(_, v) = action {
                let frac = (1.0 - v as f32).clamp(0.0, 1.0);
                match id.control {
                    C::BrowserScroll => self.browser.scroll = frac,
                    C::CellsScroll => self.cells_scroll = frac,
                    C::SettingsScroll => self.settings.scroll = frac,
                    _ => {}
                }
            }
            return if press { self.press_view(id, model) } else { Vec::new() };
        }
        if !press {
            return vec![Effect::Model(action)];
        }
        // Picking a settings option closes its dropdown.
        if matches!(id.control, C::SettingChoose(..)) {
            self.settings.menu = None;
        }
        // A tap on a menu item closes its popover; toggles inside panel popovers keep it open.
        let from_menu = matches!(action, Action::Tap(_)) && self.popover.is_some();
        let insert_menu = matches!(self.popover, Some(PopoverState { kind: Popover::InsertMenu { .. }, .. }));
        if from_menu {
            self.popover = None;
        }
        if let C::Insert(k) = id.control {
            if insert_menu {
                return vec![Effect::Model(action)]; // "Open" from the context menu
            }
            if let Some(sel) = self.browser.selected.take() {
                if let Some(p) = model.plugin_catalog().get(sel as usize) {
                    return vec![Effect::SetText(id, p.name.clone())];
                }
            }
            let to = |control| ControlId { strip: id.strip.clone(), control };
            return vec![Effect::Model(match self.modifier {
                Some(Modifier::Power) => Action::Tap(to(C::InsertPower(k))),
                Some(Modifier::Remove) => Action::Tap(to(C::InsertRemove(k))),
                Some(Modifier::Copy) => Action::Tap(to(C::InsertCopy(k))),
                Some(Modifier::SetDefault) => Action::Tap(to(C::InsertSetDefault(k))),
                _ => action,
            })];
        }
        vec![Effect::Model(action)]
    }

    fn press_view(&mut self, id: ControlId, model: &dyn MixerModel) -> Vec<Effect> {
        use Control as C;
        let mut out = Vec::new();
        let keep_popover = matches!(id.control, C::PopoverDismiss | C::TextCommit | C::TextCancel);
        if !matches!(id.control, C::SettingMenu(_) | C::PopoverDismiss) {
            self.settings.menu = None;
        }
        if !keep_popover && self.popover.is_some() {
            // Opening something else (or any view press) replaces the current popover.
            self.popover = None;
            self.text_edit = self.text_edit.take().filter(|t| t.target == TextTarget::Search);
        }
        match id.control.clone() {
            C::Section(s) => self.sections.set(s, !self.sections.get(s)),
            C::SectionsOpenAll => {
                self.sections = Sections { inputs: true, inserts: true, sends: true, fader: self.sections.fader };
            }
            C::SectionsCloseAll => {
                self.sections = Sections { inputs: false, inserts: false, sends: false, fader: self.sections.fader };
                self.cells_scroll = 0.0;
            }
            C::InsertSize(s) => self.insert_size = s,
            C::FixedSlots => self.fixed_slots ^= true,
            C::SendsOverview => self.sends_overview ^= true,
            C::Modifier(m) => self.modifier = if self.modifier == Some(m) { None } else { Some(m) },
            C::ShowBrowser => self.show_browser ^= true,
            C::ShowPluginScenes => self.toggle_panel(Panel::PluginScenes, model),
            C::ShowSettings => {
                self.sheet = if self.sheet == Some(Sheet::Settings) { None } else { Some(Sheet::Settings) };
                self.settings.scroll = 0.0;
            }
            C::SettingsClose | C::PanelClose => {
                self.sheet = None;
                self.text_edit = self.text_edit.take().filter(|t| !matches!(t.target, TextTarget::Setting(_)));
            }
            C::SettingsTab(t) => {
                self.settings.tab = t;
                self.settings.scroll = 0.0;
            }
            C::SettingsDevice(d) => self.settings.device = d,
            C::SettingMenu(k) => self.settings.menu = if self.settings.menu.as_ref() == Some(&k) { None } else { Some(k) },
            C::SettingEdit(k) => {
                let text = model.properties().and_then(|p| crate::settings::find_field(p, &k)).map(|f| f.value_text()).unwrap_or_default();
                self.text_edit = Some(TextEdit { target: TextTarget::Setting(k), text });
            }
            C::ShowPanel(p) => self.toggle_panel(p, model),
            C::PanelTab(i) => {
                self.settings.panel_tab = i;
                self.settings.scroll = 0.0;
            }
            C::SceneSelect(i) => self.settings.scene = Some(i),
            C::SceneStep(up) => {
                let n = model.plugin_scenes().len() as u16;
                if n > 0 {
                    let cur = self.settings.scene.unwrap_or(0);
                    self.settings.scene = Some(if up { cur.saturating_sub(1) } else { (cur + 1).min(n - 1) });
                }
            }
            C::Name => {
                if self.modifier == Some(Modifier::Isolate) {
                    out.push(Effect::Model(Action::Toggle(ControlId { strip: id.strip.clone(), control: C::Isolate })));
                } else {
                    let name = find_strip(model, &id.strip).map(|s| model.main(&s).name).unwrap_or_default();
                    self.text_edit = Some(TextEdit { target: TextTarget::Rename(id.strip.clone()), text: name });
                    self.open(Popover::Rename { strip: id.strip.clone() }, id);
                }
            }
            C::Preamp(_) => {
                // input_source: the MIC / LINE / HI-Z picker.
                self.open(Popover::InputSource { strip: id.strip.clone() }, id);
            }
            C::BrowserTab(t) => self.browser.tab = t,
            C::BrowserSearch => self.text_edit = Some(TextEdit { target: TextTarget::Search, text: self.browser.search.clone() }),
            C::BrowserCategoriesReveal => self.browser.categories_open ^= true,
            C::BrowserCategory(i) => {
                self.browser.category = if self.browser.category == Some(i) { None } else { Some(i) };
                self.browser.scroll = 0.0;
            }
            C::BrowserFavorites => {
                self.browser.favorites_only ^= true;
                self.browser.scroll = 0.0;
            }
            C::BrowserLargeRows => self.browser.large_rows ^= true,
            C::BrowserHintClose => self.browser.hint = false,
            C::BrowserGroupReveal => self.browser.group_open ^= true,
            C::BrowserInactiveReveal => self.browser.inactive_open ^= true,
            C::BrowserItem(i) => self.browser.selected = if self.browser.selected == Some(i) { None } else { Some(i) },
            C::SessionNavigate(first) => self.first_strip = first as usize,
            C::StatusTempo => {
                self.text_edit = Some(TextEdit { target: TextTarget::Tempo, text: format!("{:.2}", model.status().tempo) });
                self.open(Popover::Tempo, id);
            }
            C::StatusRate | C::SettingsRate => self.open(Popover::SampleRate, id),
            C::StatusClock | C::SettingsClock => self.open(Popover::ClockSource, id),
            C::SettingsBuffer => self.open(Popover::BufferSize, id),
            C::StatusScene => self.open(Popover::Scenes, id),
            C::MonitorPage(p) => self.monitor_page = p,
            C::CueOutputs => self.toggle_panel(Panel::CueOutputs, model),
            C::HeadphoneMenu(o) => self.open(Popover::Headphone { output: o }, id),
            C::PopoverDismiss => {
                self.popover = None;
                self.text_edit = self.text_edit.take().filter(|t| t.target == TextTarget::Search);
                // A tap outside closes an open settings dropdown first, then an anchored panel.
                if self.settings.menu.take().is_none() && self.sheet == Some(Sheet::Panel(Panel::CueOutputs)) {
                    self.sheet = None;
                }
            }
            C::TextCommit => out.extend(self.commit_text()),
            C::TextCancel => {
                self.popover = None;
                self.text_edit = None;
            }
            _ => {}
        }
        out
    }

    fn commit_text(&mut self) -> Option<Effect> {
        self.popover = None;
        let edit = self.text_edit.take()?;
        match edit.target {
            TextTarget::Rename(strip) => Some(Effect::SetText(ControlId { strip, control: Control::Name }, edit.text)),
            TextTarget::Tempo => Some(Effect::SetText(ControlId { strip: "global".into(), control: Control::Tempo }, edit.text)),
            TextTarget::Setting(k) => Some(Effect::SetText(ControlId { strip: "settings".into(), control: Control::SettingEdit(k) }, edit.text)),
            TextTarget::Search => None,
        }
    }

    /// Opens a sheet by name (`settings[:tab]`, `cue-outputs`, `monitor-controller`,
    /// `headphone-correction`, `plugin-scenes`, `about`) for `--open` and tests. Tabs:
    /// `hardware`, `io-matrix`, `plug-ins`, `options`, `midi`, `dante`. False if unknown.
    pub fn open_named(&mut self, name: &str) -> bool {
        let (what, tab) = name.split_once(':').unwrap_or((name, ""));
        let panel = match what {
            "settings" => {
                self.sheet = Some(Sheet::Settings);
                self.settings.tab = match tab {
                    "" | "hardware" => SettingsTab::Hardware,
                    "io-matrix" => SettingsTab::IoMatrix,
                    "plug-ins" | "plugins" => SettingsTab::PlugIns,
                    "options" => SettingsTab::Options,
                    "midi" => SettingsTab::Midi,
                    "dante" => SettingsTab::DanteDevices,
                    _ => return false,
                };
                return true;
            }
            "cue-outputs" => Panel::CueOutputs,
            "monitor-controller" => Panel::MonitorController,
            "headphone-correction" => Panel::HeadphoneCorrection,
            "plugin-scenes" => Panel::PluginScenes,
            "about" => Panel::About,
            _ => return false,
        };
        self.sheet = Some(Sheet::Panel(panel));
        self.settings.panel_tab = tab.parse().unwrap_or(0);
        true
    }

    /// Opens panel `p` (or closes it when it is already open).
    fn toggle_panel(&mut self, p: Panel, model: &dyn MixerModel) {
        if self.sheet == Some(Sheet::Panel(p)) {
            self.sheet = None;
            return;
        }
        self.sheet = Some(Sheet::Panel(p));
        self.settings.panel_tab = 0;
        self.settings.scroll = 0.0;
        if p == Panel::PluginScenes {
            let current = model.status().scene;
            self.settings.scene = current.and_then(|c| model.plugin_scenes().iter().position(|n| *n == c)).map(|i| i as u16);
        }
    }

    /// `route` + apply the effects to `model`.
    pub fn dispatch(&mut self, action: Action, model: &mut dyn MixerModel) {
        let effects = self.route(action, model);
        apply_effects(effects, model);
    }

    /// Secondary click / long press: insert context menu, rename, or browser assign mode.
    pub fn context_click(&mut self, hits: &[Hit], x: f32, y: f32) -> bool {
        let Some(hit) = hits.iter().rev().find(|h| h.rect.contains(x, y)) else { return false };
        let id = hit.id.clone();
        match id.control {
            Control::Insert(slot) => {
                self.open(Popover::InsertMenu { strip: id.strip.clone(), slot }, id);
                true
            }
            Control::BrowserItem(i) => {
                self.browser.selected = Some(i);
                true
            }
            _ => false,
        }
    }

    /// Wheel / trackpad scroll by `dy` points over the topmost scroll region at (x, y).
    pub fn wheel(&mut self, regions: &[ScrollRegion], x: f32, y: f32, dy: f32) -> bool {
        let Some(r) = regions.iter().rev().find(|r| r.rect.contains(x, y)) else { return false };
        if r.max <= 0.0 {
            return false;
        }
        let slot = match r.target {
            ScrollTarget::Browser => &mut self.browser.scroll,
            ScrollTarget::Cells => &mut self.cells_scroll,
            ScrollTarget::Settings => &mut self.settings.scroll,
        };
        *slot = (*slot + dy / r.max).clamp(0.0, 1.0);
        true
    }

    pub fn key_text(&mut self, s: &str) {
        if let Some(e) = &mut self.text_edit {
            e.text.push_str(s);
            self.sync_search();
        }
    }

    pub fn key_backspace(&mut self) {
        if let Some(e) = &mut self.text_edit {
            e.text.pop();
            self.sync_search();
        }
    }

    pub fn key_enter(&mut self, model: &mut dyn MixerModel) {
        if self.text_edit.is_some() {
            apply_effects(self.commit_text().into_iter().collect(), model);
        }
    }

    /// Closes the innermost thing: text focus / popover, then the sheet, then the modifier.
    pub fn key_escape(&mut self) {
        if self.popover.is_some() || self.text_edit.is_some() {
            self.popover = None;
            self.text_edit = None;
        } else if self.settings.menu.is_some() {
            self.settings.menu = None;
        } else if self.sheet.is_some() {
            self.sheet = None;
        } else {
            self.modifier = None;
        }
    }

    fn sync_search(&mut self) {
        if let Some(TextEdit { target: TextTarget::Search, text }) = &self.text_edit {
            self.browser.search = text.clone();
            self.browser.scroll = 0.0;
        }
    }
}

fn apply_effects(effects: Vec<Effect>, model: &mut dyn MixerModel) {
    for e in effects {
        match e {
            Effect::Model(Action::Tap(ControlId { control: Control::SettingChoose(k, i), .. }) | Action::Toggle(ControlId { control: Control::SettingChoose(k, i), .. })) => {
                model.apply_setting(&k, &SettingOp::Choose(i as usize))
            }
            Effect::Model(Action::Tap(ControlId { control: Control::SettingPress(k), .. }) | Action::Toggle(ControlId { control: Control::SettingPress(k), .. })) => {
                model.apply_setting(&k, &SettingOp::Press)
            }
            Effect::SetText(ControlId { control: Control::SettingEdit(k), .. }, text) => model.apply_setting(&k, &SettingOp::Text(text)),
            Effect::Model(a) => model.apply(&a),
            Effect::SetText(id, text) => model.set_text(&id, &text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InsertState, PreviewMixer};

    fn cid(strip: &str, control: Control) -> ControlId {
        ControlId { strip: strip.into(), control }
    }

    #[test]
    fn sections_and_navigator_are_view_only() {
        let model = PreviewMixer::default();
        let mut v = ViewState::default();
        assert!(v.route(Action::Toggle(cid("nav", Control::Section(Section::Inputs))), &model).is_empty());
        assert!(!v.sections.inputs);
        v.route(Action::Tap(cid("nav", Control::SectionsCloseAll)), &model);
        assert_eq!(v.sections, Sections { inputs: false, inserts: false, sends: false, fader: true });
        v.route(Action::Tap(cid("nav", Control::SectionsOpenAll)), &model);
        assert!(v.sections.inputs && v.sections.inserts && v.sections.sends);
        let fwd = v.route(Action::Toggle(cid("in0", Control::Mute)), &model);
        assert_eq!(fwd, vec![Effect::Model(Action::Toggle(cid("in0", Control::Mute)))]);
    }

    #[test]
    fn modifiers_translate_insert_taps() {
        let mut model = PreviewMixer::default();
        let mut v = ViewState::default();
        v.dispatch(Action::Toggle(cid("nav", Control::Modifier(Modifier::Power))), &mut model);
        assert_eq!(v.modifier, Some(Modifier::Power));
        v.dispatch(Action::Tap(cid("in0", Control::Insert(0))), &mut model);
        assert!(matches!(model.inserts["in0"][0], InsertState::Loaded { power: false, .. }));
        v.dispatch(Action::Toggle(cid("nav", Control::Modifier(Modifier::Remove))), &mut model);
        v.dispatch(Action::Tap(cid("in0", Control::Insert(0))), &mut model);
        assert_eq!(model.inserts["in0"][0], InsertState::Empty);
        // Tapping the active modifier clears it; plain taps pass through.
        v.dispatch(Action::Toggle(cid("nav", Control::Modifier(Modifier::Remove))), &mut model);
        assert_eq!(v.modifier, None);
        let e = v.route(Action::Tap(cid("in0", Control::Insert(1))), &model);
        assert_eq!(e, vec![Effect::Model(Action::Tap(cid("in0", Control::Insert(1))))]);
    }

    #[test]
    fn input_source_popover_selects_and_closes() {
        let mut model = PreviewMixer::default();
        let mut v = ViewState::default();
        v.dispatch(Action::Tap(cid("in1", Control::Preamp("input_source".into()))), &mut model);
        assert!(matches!(v.popover, Some(PopoverState { kind: Popover::InputSource { .. }, .. })));
        v.dispatch(Action::Tap(cid("in1", Control::InputSource(1))), &mut model);
        assert!(v.popover.is_none());
        assert_eq!(model.preamps["in1"].source, "LINE");
    }

    #[test]
    fn rename_and_assign_go_through_set_text() {
        let mut model = PreviewMixer::default();
        let mut v = ViewState::default();
        v.dispatch(Action::Tap(cid("in0", Control::Name)), &mut model);
        assert_eq!(v.text_edit.as_ref().map(|t| t.text.as_str()), Some("SM7B"));
        v.key_backspace();
        v.key_text("8");
        v.key_enter(&mut model);
        assert_eq!(model.mains["in0"].name, "SM78");
        assert!(v.popover.is_none());
        // Browser assign mode: select, then tap an insert.
        v.dispatch(Action::Tap(cid("browser", Control::BrowserItem(0))), &mut model);
        v.dispatch(Action::Tap(cid("in0", Control::Insert(3))), &mut model);
        assert_eq!(model.inserts["in0"][3], InsertState::Loaded { name: "UAD 4K Buss Compressor".into(), power: true });
        assert_eq!(v.browser.selected, None);
    }

    #[test]
    fn context_menu_and_scroll() {
        let mut model = PreviewMixer::default();
        let mut v = ViewState::default();
        let hits = vec![Hit { rect: houston::Rect::new(0.0, 0.0, 10.0, 10.0), id: cid("in2", Control::Insert(1)), kind: crate::input::HitKind::Tap }];
        assert!(v.context_click(&hits, 5.0, 5.0));
        v.dispatch(Action::Tap(cid("in2", Control::InsertRemove(1))), &mut model);
        assert!(v.popover.is_none());
        assert_eq!(model.inserts["in2"][1], InsertState::Empty);
        let regions = vec![ScrollRegion { rect: houston::Rect::new(0.0, 0.0, 100.0, 100.0), target: ScrollTarget::Browser, max: 200.0 }];
        assert!(v.wheel(&regions, 50.0, 50.0, 100.0));
        assert_eq!(v.browser.scroll, 0.5);
        v.route(Action::Set(cid("browser", Control::BrowserScroll), 1.0), &model);
        assert_eq!(v.browser.scroll, 0.0);
    }
}
