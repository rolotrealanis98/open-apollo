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
use crate::surfaces::session::SessionOp;

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

/// Monitor column SHOW toggles. Console 1.3.1 treats them independently: AUX shows the aux
/// strips, CTRL ROOM adds the talkback strip to the bus section and the control-room column
/// before the monitor column (user captures s45–s49: both lit).
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
    /// An insert slot's "Assign...": the next catalog tap loads into this slot.
    pub assign_to: Option<ControlId>,
    /// PRESETS tab: open preset folders (by name) and the list scroll (0…1 of the overflow).
    pub presets_open: Vec<String>,
    pub presets_scroll: f32,
    /// "Channel Presets..." / an insert's "Presets...": the strip and scope the PRESETS tab
    /// loads into and saves from (Rack's `channel_preset_assign_browser`).
    pub preset_target: Option<(Arc<str>, crate::presets::PresetScope)>,
    /// The selected PRESETS row `(top, child)` (`u16::MAX` child = top-level).
    pub preset_selected: Option<(u16, u16)>,
}

impl Default for BrowserView {
    fn default() -> Self {
        Self { tab: BrowserTab::Plugins, search: String::new(), categories_open: true, category: None, favorites_only: false, large_rows: true, hint: true, group_open: true, inactive_open: true, scroll: 0.0, selected: None, assign_to: None, presets_open: Vec::new(), presets_scroll: 0.0, preset_target: None, preset_selected: None }
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
    /// The navigator SESSION box's menu.
    Session,
    /// "SAVE SESSION": the session has unsaved changes; `then` runs after the answer.
    SaveChanges { then: SessionOp },
    /// Session name field (Save As; uses `ViewState::text_edit`).
    SessionSaveAs,
    /// Session file path field (Open...; uses `ViewState::text_edit`).
    SessionOpen,
    /// Rename / New Folder name fields (CURRENT SESSION panel menus).
    SessionRename,
    SessionNewFolder,
    /// Console's confirmation (`Rack_Window` 0x180a40730: a Popover-Header title, the message,
    /// CANCEL and a primed OK). OK taps `ok`.
    /// A question with CANCEL and `ok_label` (OK, or YES for SWITCH NETWORK INTERFACE?).
    Confirm { title: String, message: String, ok: ControlId, ok_label: String },
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
    /// Save As; `then` is the session op waiting on the save (from the save-changes question).
    SessionSaveAs { then: Option<SessionOp> },
    SessionOpen,
    /// A session file's new name (panel row menu Rename), by file index.
    SessionRename(u16),
    /// The CURRENT SESSION panel's New Folder.
    SessionNewFolder,
    /// A folder row's Rename and New Folder, by panel row index.
    SessionFolderRename(u16),
    SessionNewFolderIn(u16),
    /// A PLUG-IN SCENES row's Rename (`Rack_ScenesListBoxRow::EditSceneName`), by scene index.
    SceneName(u16),
    /// A preset name for SAVE AS / RENAME / NEW FOLDER on the PRESETS target's strip.
    Preset(Arc<str>, crate::presets::PresetScope, crate::presets::PresetCmd),
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
    /// The selected row of a panel list (ADD DEVICE, I/O PRESETS, source categories).
    pub list: Option<u16>,
    /// A window Settings opened over itself (ADD DEVICE, Route Presets, the I/O Matrix source
    /// picker): Console keeps Settings open underneath (set-adddev, set-d-iopresets,
    /// set-d-insrc).
    pub child: Option<crate::settings::Panel>,
}

/// Panels Console opens over the Settings window rather than in its place.
pub fn opens_over_settings(p: crate::settings::Panel) -> bool {
    use crate::settings::Panel as P;
    matches!(p, P::AddDevice | P::IoPresets | P::RouteSource { .. })
}

/// UI-only state: owned by the app shell, read by `build_window_with`, changed by `route`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    /// TEMPO window TAP timing.
    pub tap: crate::menu::TapTempo,
    /// No browser / navigator; aux and monitor packed after the last strip.
    pub compact: bool,
    pub sections: Sections,
    pub insert_size: InsertSize,
    pub fixed_slots: bool,
    pub sends_overview: bool,
    pub modifier: Option<Modifier>,
    pub show_browser: bool,
    pub browser: BrowserView,
    /// SHOW AUX: the aux strips in the bus section.
    pub show_aux: bool,
    /// SHOW CTRL ROOM: the talkback strip in the bus section and the control-room column.
    pub show_ctrl_room: bool,
    /// The PLUG-IN SCENES panel column, left of the browser (user capture s10).
    pub show_scenes: bool,
    /// The CURRENT SESSION panel in the browser column (the SESSION box; capture sess-panel).
    pub session_panel: crate::surfaces::session::SessionPanel,
    /// The status bar has its SCENE field. Console creates it lazily: the 08:27 captures show
    /// it only after the PLUG-IN SCENES panel was opened (s08 without, s10 with), p1 with, p2
    /// without.
    pub scene_info: bool,
    /// Cells column scroll, 0…1 of the overflow.
    pub cells_scroll: f32,
    /// Bus-section cells scroll, 0…1 of the overflow (its own scrollbar).
    pub aux_scroll: f32,
    /// First visible input strip (session navigator); clamped when drawn.
    pub first_strip: usize,
    pub popover: Option<PopoverState>,
    pub text_edit: Option<TextEdit>,
    pub sheet: Option<Sheet>,
    pub settings: SettingsView,
    /// Window size in points; the full layout follows it (None: Console's 1920 × 1050).
    pub window: Option<(f32, f32)>,
    /// The open popup menu (context menu, menu-bar menu), drawn last (`menu::draw_menu`).
    pub menu: Option<crate::menu::MenuState>,
    /// Draw Windows Console's in-window menu bar in the title bar (desktop shells without a
    /// native menu bar). Off for renders compared against macOS Console.
    pub menu_bar: bool,
    /// View > Section > Show Mixer Navigation.
    pub show_navigator: bool,
    /// View > Section > Show Monitor (`Rack_ShowSection` 0, 0x18091baa0): the monitor column.
    pub show_monitor: bool,
    /// View > Section > Show Monitor Correction (`Rack_ShowSection` 0xf): the monitor column's
    /// MONITOR CORRECTION rows.
    pub show_monitor_correction: bool,
    /// View > Show/Hide Channels (⌘I, `Rack_ShowHideChannels` 0x180918150: toggles window
    /// page 8): the channel list in which hidden strips can be shown again.
    pub show_channels: bool,
    /// View > Show Offline Devices (`Rack_ShowOfflineDevices`): device tabs include offline units.
    pub show_offline_devices: bool,
    /// Rack_Selection is client state (LoadProperties 0x180a7cd40), not
    /// SelectedOnFront. Selection-changing gestures are an explicit Rust
    /// policy: the last mixer strip pressed/dragged, pending native parity.
    pub selected_channel: Option<Arc<str>>,
    /// Shell-scope commands the menus requested (`take_commands`).
    pub commands: Vec<crate::menu::Command>,
    /// Model effects a menu command produced outside `route` (File > Save → the session tap).
    pub deferred: Vec<Effect>,
    /// URLs the console asked the host to open, oldest first (`take_open_urls`): Show In
    /// Finder's `file://` URLs, documentation, Dante Controller and patent pages, feedback.
    pub open_urls: Vec<String>,
    /// When the armed navigator modifier was armed or last used: it disarms after OPTIONS ›
    /// MODIFIERS TIMEOUT seconds (`expire_modifier`).
    pub modifier_armed: Option<std::time::Instant>,
    /// Console's menus, Settings and panels as separate OS windows placed on the screen
    /// (`crate::layers`); None draws them inside the mixer window (iPad, plain `--render`).
    pub windowing: Option<crate::layers::Windowing>,
    /// Drag and drop in progress (`crate::dnd`).
    pub drag: Option<crate::dnd::DragState>,
    /// A press on a drag source, not yet moved far enough to drag.
    pub drag_pending: Option<crate::dnd::PendingDrag>,
    /// Console's `GUI_BlinkManager` slow flag (a 125 ms timer flipped every fourth tick: 500 ms
    /// on, 500 ms off; ctor 0x181c345b0, tick 0x181c34850). The status bar's CLOCK outline
    /// blinks with it while the clock is not locked. Off until the host calls
    /// [`ViewState::tick_blink`], so one-shot renders are deterministic.
    pub blink: bool,
    /// When [`ViewState::tick_blink`] first ran (the blink phase counts from it).
    pub blink_epoch: Option<std::time::Instant>,
}

impl ViewState {
    /// Advances [`ViewState::blink`] to `now`; true when it flipped.
    pub fn tick_blink(&mut self, now: std::time::Instant) -> bool {
        let epoch = *self.blink_epoch.get_or_insert(now);
        let on = (now.saturating_duration_since(epoch).as_millis() / 500) % 2 == 0;
        std::mem::replace(&mut self.blink, on) != on
    }
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            tap: Default::default(),
            compact: false,
            sections: Sections::default(),
            insert_size: InsertSize::Large,
            fixed_slots: true,
            sends_overview: false,
            modifier: None,
            show_browser: true,
            browser: BrowserView::default(),
            show_aux: true,
            show_ctrl_room: false,
            show_scenes: false,
            session_panel: Default::default(),
            scene_info: true,
            cells_scroll: 0.0,
            aux_scroll: 0.0,
            first_strip: 0,
            popover: None,
            text_edit: None,
            sheet: None,
            settings: SettingsView::default(),
            window: None,
            menu: None,
            menu_bar: false,
            show_navigator: true,
            show_monitor: true,
            show_monitor_correction: true,
            show_channels: false,
            show_offline_devices: false,
            selected_channel: None,
            commands: Vec::new(),
            deferred: Vec::new(),
            drag: None,
            drag_pending: None,
            open_urls: Vec::new(),
            modifier_armed: None,
            windowing: None,
            blink: false,
            blink_epoch: None,
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
            | C::ChannelsClose
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
            | C::BrowserAssignClose
            | C::BrowserPresetFolder(_)
            | C::BrowserPresetFile(..)
            | C::PresetCmd(..)
            | C::BrowserGroupReveal
            | C::BrowserInactiveReveal
            | C::BrowserScroll
            | C::BrowserItem(_)
            | C::BrowserActivate(_)
            | C::BrowserPresetActivate(..)
            | C::SessionNavigate(_)
            | C::CellsScroll
            | C::AuxCellsScroll
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
            | C::TempoTap
            | C::SettingsScroll
            | C::ListSelect(_)
            | C::SessionMenu
            | C::SessionPanelRow(_)
            | C::SessionPanelDots
            | C::SessionSaveChoice(_)
    ) || matches!(c, C::Preamp(p) if &**p == "input_source")
        || matches!(c, C::Menu(m) if m.is_view())
}

fn find_strip(model: &dyn MixerModel, id: &str) -> Option<StripInfo> {
    model.inputs().into_iter().chain(model.auxes()).find(|s| &*s.id == id)
}

impl ViewState {
    pub fn compact() -> Self {
        Self { compact: true, ..Self::default() }
    }

    /// Insert-assign mode for `slot` of `strip` ("Assign..." in the slot's context menu): the
    /// browser shows its PLUG-INS tab with the "STRIP: INSERT N" header, and the next plug-in
    /// picked goes into that slot.
    pub fn begin_assign(&mut self, strip: Arc<str>, slot: u8) {
        self.show_browser = true;
        self.browser.tab = BrowserTab::Plugins;
        self.browser.assign_to = Some(ControlId { strip, control: Control::Insert(slot) });
    }

    /// Applies a view spec: comma-separated `key=value` items (renders, `--view`, tests).
    ///
    /// `inputs|inserts|sends|fader=0|1`, `small` / `large`, `fixed=0|1`, `overview=0|1`,
    /// `scroll=F`, `aux-scroll=F` (0…1), `browser=0|1`, `tab=plugins|presets`, `hint=0|1`,
    /// `aux=0|1`, `ctrl=0|1`, `scenes=0|1`, `scene-info=0|1`, `first=N`,
    /// `presets-open=Folder|Folder`, `presets-scroll=F`,
    /// `assign=STRIP:SLOT` (slot `u` = Unison), `compact=0|1`.
    pub fn apply_spec(&mut self, spec: &str) -> Result<(), String> {
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = item.split_once('=').unwrap_or((item, "1"));
            let flag = || match v {
                "1" | "on" | "true" | "yes" => Ok(true),
                "0" | "off" | "false" | "no" => Ok(false),
                _ => Err(format!("view: {k}={v}: expected 0 or 1")),
            };
            let frac = || v.parse::<f32>().map(|f| f.clamp(0.0, 1.0)).map_err(|_| format!("view: {k}={v}: expected 0…1"));
            match k {
                "inputs" => self.sections.inputs = flag()?,
                "inserts" => self.sections.inserts = flag()?,
                "sends" => self.sections.sends = flag()?,
                "fader" => self.sections.fader = flag()?,
                "small" => self.insert_size = InsertSize::Small,
                "large" => self.insert_size = InsertSize::Large,
                "fixed" => self.fixed_slots = flag()?,
                "overview" => self.sends_overview = flag()?,
                "scroll" => self.cells_scroll = frac()?,
                "aux-scroll" => self.aux_scroll = frac()?,
                "browser" => self.show_browser = flag()?,
                "tab" => {
                    self.browser.tab = match v {
                        "plugins" => BrowserTab::Plugins,
                        "presets" => BrowserTab::Presets,
                        _ => return Err(format!("view: tab={v}: expected plugins or presets")),
                    }
                }
                "hint" => self.browser.hint = flag()?,
                "presets-open" => self.browser.presets_open = v.split('|').map(str::to_owned).filter(|n| !n.is_empty()).collect(),
                "presets-scroll" => self.browser.presets_scroll = frac()?,
                "aux" => self.show_aux = flag()?,
                "ctrl" => self.show_ctrl_room = flag()?,
                "scenes" => self.show_scenes = flag()?,
                "scene-info" => self.scene_info = flag()?,
                "compact" => self.compact = flag()?,
                "first" => self.first_strip = v.parse().map_err(|_| format!("view: first={v}: expected a strip index"))?,
                "assign" => {
                    let (strip, slot) = v.rsplit_once(':').ok_or_else(|| format!("view: assign={v}: expected STRIP:SLOT"))?;
                    let slot = if slot.eq_ignore_ascii_case("u") { 255 } else { slot.parse().map_err(|_| format!("view: assign={v}: bad slot"))? };
                    self.begin_assign(strip.into(), slot);
                }
                _ => return Err(format!("view: unknown item {k:?}")),
            }
        }
        Ok(())
    }

    /// Seconds an armed modifier stays armed (OPTIONS › MODIFIERS TIMEOUT, Console's default 6;
    /// 0 or blank: until disarmed by hand).
    pub fn modifier_timeout(model: &dyn MixerModel) -> Option<f32> {
        let text = model.properties().and_then(|p| p.prop("pref:modifiers_timeout")).map(|v| v.display());
        let secs = text.as_deref().map(str::trim).unwrap_or("6").parse::<f32>().ok()?;
        (secs > 0.0).then_some(secs)
    }

    /// When the armed modifier disarms (`None` when none is armed or there is no timeout).
    pub fn modifier_deadline(&self, model: &dyn MixerModel) -> Option<std::time::Instant> {
        self.modifier?;
        Some(self.modifier_armed? + std::time::Duration::from_secs_f32(Self::modifier_timeout(model)?))
    }

    /// Disarms the modifier once MODIFIERS TIMEOUT has passed since it was armed or last used.
    /// Call once per frame; true when it changed the view.
    pub fn expire_modifier(&mut self, model: &dyn MixerModel, now: std::time::Instant) -> bool {
        match self.modifier_deadline(model) {
            Some(t) if now >= t => {
                self.modifier = None;
                self.modifier_armed = None;
                true
            }
            _ => false,
        }
    }

    /// URLs to open (Show In Finder, links, launch buttons) since the last call, oldest first.
    pub fn take_open_urls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.open_urls)
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
        if (press || matches!(action, Action::GestureBegin(_))) && find_strip(model, &id.strip).is_some() {
            self.selected_channel = Some(id.strip.clone());
        }
        if is_view_control(&id.control) {
            if let Action::Set(_, v) = action {
                let frac = (1.0 - v as f32).clamp(0.0, 1.0);
                match id.control {
                    C::BrowserScroll => self.browser.scroll = frac,
                    C::CellsScroll => self.cells_scroll = frac,
                    C::SettingsScroll => self.settings.scroll = frac,
                    C::AuxCellsScroll => self.aux_scroll = frac,
                    _ => {}
                }
            }
            return if press { self.press_view(id, model) } else { Vec::new() };
        }
        if !press {
            return vec![Effect::Model(action)];
        }
        // NETWORK's LAUNCH / MORE INFO open pages (Console: `Rack_NetworkSettings` 0x18085ca10,
        // 0x18085cc00); they send nothing to the engine.
        if let C::SettingPress(k) = &id.control {
            let url = match &**k {
                "launch:dante_controller" => Some(crate::menu::DANTE_CONTROLLER_URL),
                "launch:dante_patents" => Some(crate::menu::DANTE_PATENTS_URL),
                _ => None,
            };
            if let Some(url) = url {
                self.open_urls.push(url.into());
                return Vec::new();
            }
        }
        // NETWORK INTERFACE: picking another adapter asks first (`Rack_NetworkSettings`
        // 0x180e3f230); YES sends the choice as `nic-set:` (a helper write).
        if let C::SettingChoose(k, i) = &id.control
            && let Some(path) = k.strip_prefix("nic:")
        {
            self.settings.menu = None;
            let current = model.properties().and_then(|p| {
                let cur = p.prop(&format!("{}{}", crate::settings::HELPER_PREFIX, crate::settings::NIC_PATH))?;
                p.choices(&format!("{}{}", crate::settings::HELPER_PREFIX, crate::settings::NIC_LIST_PATH)).iter().position(|c| c.value.same(&cur))
            });
            if current != Some(*i as usize) {
                let ok = ControlId { strip: id.strip.clone(), control: C::SettingChoose(format!("nic-set:{path}").into(), *i) };
                self.popover = Some(PopoverState {
                    kind: Popover::Confirm {
                        title: crate::settings::NIC_CONFIRM_TITLE.into(),
                        message: crate::settings::NIC_CONFIRM_MESSAGE.into(),
                        ok,
                        ok_label: "YES".into(),
                    },
                    anchor: id.clone(),
                });
            }
            return Vec::new();
        }
        // Picking a settings option closes its dropdown.
        if matches!(id.control, C::SettingChoose(..)) {
            self.settings.menu = None;
        }
        if let C::Session(op) = id.control {
            self.menu = None;
            return self.route_session(op, model);
        }
        // Any other press while a popup menu is open came from one of its rows: it closes.
        let from_popup = self.menu.take().is_some();
        if from_popup {
            if let C::Insert(_) = id.control {
                return vec![Effect::Model(action)]; // "Open..." (Unison) from the menu
            }
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
            if self.modifier.is_some() {
                self.modifier_armed = Some(std::time::Instant::now()); // a use restarts the timeout
            }
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
        if let C::Menu(cmd) = &id.control {
            use crate::menu::MenuCmd as M;
            if !matches!(cmd, M::Row { .. } | M::OpenSub { .. } | M::BarTitle(_)) {
                self.menu = None;
            }
            self.press_menu(&id, cmd, model);
            out.append(&mut self.deferred);
            return out;
        }
        if self.menu.take().is_some() && id.control == C::PopoverDismiss {
            return out;
        }
        let keep_popover = matches!(id.control, C::PopoverDismiss | C::TextCommit | C::TextCancel);
        if !matches!(id.control, C::SettingMenu(_) | C::PopoverDismiss) {
            self.settings.menu = None;
        }
        let mut closed = None;
        if !keep_popover && self.popover.is_some() {
            // Opening something else (or any view press) replaces the current popover.
            closed = self.popover.take();
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
            C::Modifier(m) => {
                self.modifier = if self.modifier == Some(m) { None } else { Some(m) };
                self.modifier_armed = self.modifier.map(|_| std::time::Instant::now());
            }
            C::ShowBrowser => self.show_browser ^= true,
            C::ShowPluginScenes => self.toggle_scenes(model),
            C::ShowSettings => {
                if id.strip.as_ref() == "delay-comp" {
                    self.sheet = Some(Sheet::Settings);
                    self.settings.tab = SettingsTab::Hardware;
                } else {
                    self.sheet = if self.sheet == Some(Sheet::Settings) { None } else { Some(Sheet::Settings) };
                }
                self.settings.scroll = 0.0;
                self.settings.child = None;
            }
            // A window over Settings closes alone (its X); Settings' own X closes both.
            C::PanelClose if self.settings.child.is_some() => self.settings.child = None,
            C::ChannelsClose => self.show_channels = false,
            C::SettingsClose | C::PanelClose => {
                self.sheet = None;
                self.settings.child = None;
                self.text_edit = self.text_edit.take().filter(|t| !matches!(t.target, TextTarget::Setting(_)));
            }
            C::SettingsTab(t) => {
                self.settings.tab = t;
                self.settings.scroll = 0.0;
            }
            C::SettingsDevice(d) => self.settings.device = d,
            // A Console menu under the combo (captures set-d-*), with hover like any menu.
            C::SettingMenu(k) => self.open_field_menu(crate::menu::MenuTarget::Setting(k), id, model),
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
            C::ListSelect(i) => self.settings.list = Some(i),
            C::TempoTap => {
                if let Some(bpm) = self.tap.tap(std::time::Instant::now()) {
                    // Console shows it with two decimals and applies it at once (0x1811a05d0).
                    if let Some(e) = self.text_edit.as_mut().filter(|e| e.target == TextTarget::Tempo) {
                        e.text = format!("{bpm:.2}");
                    }
                    out.push(Effect::SetText(ControlId { strip: "global".into(), control: C::Tempo }, format!("{bpm}")));
                }
            }
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
            C::BrowserAssignClose => self.browser.assign_to = None,
            C::BrowserPresetFile(top, child) => out.extend(self.press_preset_file(top, child, model)),
            C::PresetCmd(scope, ref cmd) => out.extend(self.press_preset_cmd(&id.strip, scope, cmd.clone(), model)),
            C::BrowserPresetFolder(k) => {
                if let Some(f) = self.preset_tree(model).get(k as usize) {
                    if let Some(i) = self.browser.presets_open.iter().position(|n| *n == f.name) {
                        self.browser.presets_open.remove(i);
                    } else {
                        self.browser.presets_open.push(f.name.clone());
                    }
                }
            }
            C::BrowserGroupReveal => self.browser.group_open ^= true,
            C::BrowserInactiveReveal => self.browser.inactive_open ^= true,
            C::BrowserItem(i) => match self.browser.assign_to.take() {
                Some(slot) => {
                    if let Some(p) = model.plugin_catalog().get(i as usize) {
                        out.push(Effect::SetText(slot, p.name.clone()));
                    }
                    self.browser.selected = None;
                }
                None => self.browser.selected = if self.browser.selected == Some(i) { None } else { Some(i) },
            },
            C::BrowserActivate(i) => {
                let target = self.browser.assign_to.take().or_else(|| {
                    let strip = self.selected_channel.as_ref()?;
                    let s = find_strip(model, strip)?;
                    let k = model.inserts(&s).iter().position(|slot| matches!(slot, crate::model::InsertState::Empty))?;
                    Some(ControlId { strip: strip.clone(), control: C::Insert(k as u8) })
                });
                if let (Some(target), Some(plugin)) = (target, model.plugin_catalog().get(i as usize)) {
                    out.push(Effect::SetText(target, plugin.name.clone()));
                    self.browser.selected = None;
                }
            }
            C::BrowserPresetActivate(top, child) => {
                if self.browser.preset_target.is_some() {
                    out.extend(self.press_preset_file(top, child, model));
                } else if let Some(strip) = self.selected_channel.as_ref().filter(|id| find_strip(model, id).is_some()) {
                    if let Some(effect) = self.preset_drop(top, child, strip.clone(), model) {
                        out.push(effect);
                    }
                }
            }
            C::SessionNavigate(first) => self.first_strip = first as usize,
            // TEMPO opens Console's dialog (capture s05).
            C::StatusTempo => self.toggle_panel(Panel::Tempo, model),
            // Console menus under the field (sb-rate / sb-clock), with hover like any menu.
            C::StatusRate | C::SettingsRate => self.open_field_menu(crate::menu::MenuTarget::SampleRate, id, model),
            C::StatusClock | C::SettingsClock => self.open_field_menu(crate::menu::MenuTarget::ClockSource, id, model),
            C::SettingsBuffer => self.open_field_menu(crate::menu::MenuTarget::BufferSize, id, model),
            // The status bar's SCENE opens the PLUG-IN SCENES column (Console 1.3.1 capture).
            C::StatusScene => {
                if !self.show_scenes {
                    self.toggle_scenes(model);
                }
            }
            C::MonitorPage(MonitorPage::Aux) => self.show_aux ^= true,
            C::MonitorPage(MonitorPage::ControlRoom) => self.show_ctrl_room ^= true,
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
            C::TextCommit => {
                out.extend(self.commit_text(model));
                if self.sheet == Some(Sheet::Panel(Panel::Tempo)) {
                    self.sheet = None;
                }
            }
            C::SessionMenu => self.open_session_menu(id),
            C::SessionPanelRow(i) => self.session_panel.selected = Some(i),
            C::SessionPanelDots => {
                self.popover = None;
                self.menu = Some(crate::menu::MenuState::below_control(crate::menu::MenuTarget::SessionDots, id));
            }
            C::SessionSaveChoice(c) => {
                // press_view closed the question; reopen it so the answer sees `then`.
                if let Some(p) = closed.take() {
                    self.popover = Some(p);
                }
                out.extend(self.session_choice(c, model));
            }
            C::TextCancel => {
                self.popover = None;
                self.text_edit = None;
                if self.sheet == Some(Sheet::Panel(Panel::Tempo)) {
                    self.sheet = None;
                }
            }
            _ => {}
        }
        out
    }

    fn commit_text(&mut self, model: &dyn MixerModel) -> Vec<Effect> {
        self.popover = None;
        let Some(edit) = self.text_edit.take() else { return Vec::new() };
        match edit.target {
            TextTarget::Setting(k) => vec![Effect::SetText(ControlId { strip: "settings".into(), control: Control::SettingEdit(k) }, edit.text)],
            TextTarget::Rename(strip) => vec![Effect::SetText(ControlId { strip, control: Control::Name }, edit.text)],
            TextTarget::Tempo => vec![Effect::SetText(ControlId { strip: "global".into(), control: Control::Tempo }, edit.text)],
            TextTarget::Search => Vec::new(),
            TextTarget::SceneName(i) => vec![Effect::SetText(ControlId { strip: "global".into(), control: Control::SceneName(i) }, edit.text)],
            TextTarget::Preset(strip, scope, cmd) => self.commit_preset_text(strip, scope, cmd, edit.text),
            t @ (TextTarget::SessionSaveAs { .. } | TextTarget::SessionOpen | TextTarget::SessionRename(_) | TextTarget::SessionNewFolder | TextTarget::SessionFolderRename(_) | TextTarget::SessionNewFolderIn(_)) => self.commit_session_text(&t, edit.text, model),
        }
    }

    /// Opens a sheet by name (`settings[:tab]`, `cue-outputs`, `monitor-controller`,
    /// `headphone-correction`, `plugin-scenes`, `about`) for `--open` and tests. Tabs:
    /// `hardware`, `io-matrix`, `plug-ins`, `options`, `midi`, `dante`. False if unknown.
    pub fn open_named(&mut self, name: &str) -> bool {
        // `settings:io-matrix+io-presets`: each in turn (a window over Settings stays over it).
        if let Some((a, b)) = name.split_once('+') {
            return self.open_named(a) && self.open_named(b);
        }
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
                    "network" => SettingsTab::Network,
                    _ => return false,
                };
                return true;
            }
            "cue-outputs" => Panel::CueOutputs,
            "monitor-controller" => Panel::MonitorController,
            "headphone-correction" => Panel::HeadphoneCorrection,
            "plugin-scenes" => Panel::PluginScenes,
            "about" => Panel::About,
            "meters" => Panel::MeterWindow,
            "talkback" => Panel::Sends(crate::settings::SendsOf::Talkback),
            "aux-sends" | "input-sends" => {
                let (device, index) = tab.split_once(':').map(|(d, n)| (d.parse().unwrap_or(0), n.parse().unwrap_or(0))).unwrap_or((0, tab.parse().unwrap_or(0)));
                Panel::Sends((if what == "aux-sends" { crate::settings::SendsOf::Aux(index) } else { crate::settings::SendsOf::Input(index) }).on_device(device))
            }
            "add-device" => Panel::AddDevice,
            "io-presets" => Panel::IoPresets,
            "input-source" => Panel::RouteSource { route: tab.parse().unwrap_or(0), outputs: false },
            "tempo" => Panel::Tempo,
            _ => return false,
        };
        if panel == Panel::PluginScenes {
            self.show_scenes = true;
            self.scene_info = true;
            return true;
        }
        if self.sheet == Some(Sheet::Settings) && opens_over_settings(panel) {
            self.settings.child = Some(panel);
            return true;
        }
        self.sheet = Some(Sheet::Panel(panel));
        self.settings.panel_tab = tab.parse().unwrap_or(0);
        true
    }

    /// Opens panel `p` (or closes it when it is already open).
    fn toggle_panel(&mut self, p: Panel, model: &dyn MixerModel) {
        if p == Panel::PluginScenes {
            self.toggle_scenes(model);
            return;
        }
        if self.sheet == Some(Sheet::Settings) && opens_over_settings(p) {
            self.settings.child = if self.settings.child == Some(p) { None } else { Some(p) };
            self.settings.list = None;
            return;
        }
        if self.sheet == Some(Sheet::Panel(p)) {
            self.sheet = None;
            return;
        }
        self.sheet = Some(Sheet::Panel(p));
        self.settings.panel_tab = 0;
        self.settings.scroll = 0.0;
        self.settings.list = None;
        if p == Panel::Tempo {
            self.text_edit = Some(TextEdit { target: TextTarget::Tempo, text: format!("{:.2}", model.status().tempo) });
        }
    }

    /// Shows or hides the PLUG-IN SCENES column (s10: 288 pt left of the browser, which moves
    /// right). Opening it selects the current scene and creates the status bar's SCENE field.
    pub fn toggle_scenes(&mut self, model: &dyn MixerModel) {
        self.show_scenes ^= true;
        if self.show_scenes {
            self.scene_info = true;
            self.settings.scroll = 0.0;
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
        if self.menu.is_some() {
            self.menu = None;
            return true;
        }
        if self.open_context_menu(hits, x, y) {
            return true;
        }
        let Some(hit) = hits.iter().rev().find(|h| h.rect.contains(x, y)) else { return false };
        let id = hit.id.clone();
        // A strip's sends cell (its mini send faders): that strip's Sends window (send-cell,
        // sends-aux).
        if let Control::SendLevel(_) = id.control {
            if let Some(of) = sends_of(hits, &id.strip) {
                self.sheet = Some(Sheet::Panel(Panel::Sends(of)));
                return true;
            }
        }
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
            ScrollTarget::Browser if self.browser.tab == BrowserTab::Presets => &mut self.browser.presets_scroll,
            ScrollTarget::Browser => &mut self.browser.scroll,
            ScrollTarget::Cells => &mut self.cells_scroll,
            ScrollTarget::Settings => &mut self.settings.scroll,
            ScrollTarget::AuxCells => &mut self.aux_scroll,
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
            let effects = self.commit_text(&*model);
            apply_effects(effects, model);
        }
    }

    /// Closes the innermost thing: text focus / popover, then the sheet, then the modifier.
    pub fn key_escape(&mut self) {
        if let Some(m) = &mut self.menu {
            // Innermost submenu first, like JUCE.
            if m.open.pop().is_none() {
                self.menu = None;
            }
        } else if self.popover.is_some() || self.text_edit.is_some() {
            self.popover = None;
            self.text_edit = None;
        } else if self.settings.menu.is_some() {
            self.settings.menu = None;
        } else if self.settings.child.is_some() {
            self.settings.child = None;
        } else if self.sheet.is_some() {
            self.sheet = None;
        } else if self.browser.assign_to.is_some() {
            self.browser.assign_to = None;
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

/// Which Sends window a strip's sends belong to: the talkback strip's, an aux's (`auxs/N`) or
/// an input's (`inputs/N`), from its id and the frame's hits.
fn sends_of(hits: &[Hit], strip: &Arc<str>) -> Option<crate::settings::SendsOf> {
    use crate::settings::SendsOf;
    if hits.iter().any(|h| h.id.strip == *strip && h.id.control == Control::ShowPanel(Panel::Sends(SendsOf::Talkback))) {
        return Some(SendsOf::Talkback);
    }
    let n = strip.rsplit('/').next().and_then(|t| t.trim_start_matches(|c: char| !c.is_ascii_digit()).parse::<u8>().ok())?;
    let device = strip.split('/').nth(2).and_then(|d| d.parse().ok()).unwrap_or(0);
    Some((if strip.contains("aux") { SendsOf::Aux(n) } else { SendsOf::Input(n) }).on_device(device))
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
