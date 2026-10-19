//! Hit regions and pointer interaction → model actions.

use std::sync::Arc;

use houston::Rect;

use crate::settings::{Panel, SettingsTab};
use crate::surfaces::session::{SaveChoice, SessionOp};
use crate::surfaces::{BrowserTab, InsertSize, Modifier, MonitorPage, Section};

/// Which control of which strip ("in0", "aux1", "monitor", …).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ControlId {
    pub strip: Arc<str>,
    pub control: Control,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Fader,
    PanL,
    PanR,
    Solo,
    Mute,
    PrePost,
    Mono,
    RecMon,
    /// A preamp layout component id (`preamp_gain`, `48v`, `pad`, `low_cut`, `phase`, …).
    Preamp(Arc<str>),
    Insert(u8),
    SendLevel(u8),
    SendMute(u8),
    /// Pan of send k (full send strips: `sends/k/Pan`).
    SendPan(u8),
    MonitorLevel,
    MonitorMono,
    MonitorMute,
    /// Control-room SOURCE: 0 = MONITOR, n = CUE n (`outputs/MON/MixInSource`).
    CrSource(u8),

    // ---- UI surfaces (`surfaces/`). [view] = UI-only, consumed by `ViewState::route`;
    // [model] = forwarded to `MixerModel::apply` (or `set_text`). ----
    /// [view] Navigator INPUTS/INSERTS/SENDS/FADER and the side-section chevrons: collapse a cell row.
    Section(Section),
    /// [view] Navigator OPEN (expand every row) / CLOSE (collapse every row).
    SectionsOpenAll,
    SectionsCloseAll,
    /// [view] Navigator LARGE / SMALL insert slots.
    InsertSize(InsertSize),
    /// [view] Navigator FIXED SLOTS (show every insert slot vs. loaded + one empty).
    FixedSlots,
    /// [view] Navigator SENDS OVERVIEW (compact 35-pt sends).
    SendsOverview,
    /// [model] Navigator INSERT EFFECTS: every input's `RecordPreEffects` (false = UAD REC, true = UAD MON).
    AllRecordPreEffects(bool),
    /// [view] Navigator MODIFIERS (exclusive; tapping the active one clears it).
    Modifier(Modifier),
    /// [view] Navigator SHOW: BROWSER / PLUG-IN SCENES (popover) / SETTINGS (sheet).
    ShowBrowser,
    ShowPluginScenes,
    ShowSettings,
    /// [model] Side section S (clear every solo) and OL (clear clip/overload indicators).
    ClearSolos,
    ClearClips,
    /// [model] Insert operations (context menu or modifier + tap). Any press toggles power.
    InsertPower(u8),
    InsertRemove(u8),
    InsertCopy(u8),
    InsertSetDefault(u8),
    /// [model] Drag and drop (`crate::dnd`): the loaded insert `from_slot` of strip `from`
    /// dropped on slot `slot` of this strip (`post /MoveInsert`); `copy` with the copy modifier.
    InsertMove { slot: u8, from: Arc<str>, from_slot: u8, copy: bool },
    /// [model] Drag and drop: a channel preset (its path) dropped on this strip.
    PresetDrop(Arc<str>),
    /// [model] Send pre/post (`PostFaderOverride`).
    SendPre(u8),
    /// [view] Strip name plate: rename popover, or Isolate with the ISOLATE modifier. As a
    /// `set_text` target: the new channel name.
    Name,
    /// [model] Strip `Isolate`.
    Isolate,
    /// [model] Input source: index into `MixerModel::input_sources` (`IOType`).
    InputSource(u8),
    /// [view] Plug-in browser.
    BrowserTab(BrowserTab),
    BrowserSearch,
    BrowserCategoriesReveal,
    BrowserCategory(u8),
    BrowserFavorites,
    BrowserLargeRows,
    BrowserHintClose,
    /// [view] The assign header's close button: leave insert-assign mode.
    BrowserAssignClose,
    /// [view] PRESETS tab: open / close the k-th top-level preset folder.
    BrowserPresetFolder(u16),
    /// [view] PRESETS tab file row `(top, child)` (`u16::MAX` child = a top-level file): selects;
    /// with a target (Channel Presets...) or tapped again, loads it.
    BrowserPresetFile(u16, u16),
    /// [view → model] A preset file operation on the PRESETS target (`presets::PresetCmd`).
    PresetCmd(crate::presets::PresetScope, crate::presets::PresetCmd),
    /// [model] `set_text` target: load `[folder/]name` into the strip's or an insert's `Preset`.
    Preset(crate::presets::PresetScope),
    BrowserGroupReveal,
    /// The "Universal Audio (Inactive)" group's reveal toggle.
    BrowserInactiveReveal,
    /// [view] Browser scrollbar thumb (drag; value = 1 − scroll fraction).
    BrowserScroll,
    /// [view] Select a catalog entry (index into `plugin_catalog`); the next insert tap assigns it.
    BrowserItem(u16),
    /// Explicit activation (double click or Assign), distinct from row selection.
    BrowserActivate(u16),
    BrowserPresetActivate(u16, u16),
    /// [view] Session navigator: make this input the first visible strip.
    SessionNavigate(u16),
    /// [view] Cells scrollbar thumb (drag; value = 1 − scroll fraction).
    CellsScroll,
    /// [view] Bus-section cells scrollbar thumb (drag; value = 1 − scroll fraction).
    AuxCellsScroll,
    /// [view] Status bar fields: open the tempo editor / rate / clock / scene popovers.
    StatusTempo,
    StatusRate,
    StatusClock,
    StatusScene,
    /// [model] Choices from `engine_settings()` / `plugin_scenes()` (indices into those lists).
    SampleRate(u8),
    ClockSource(u8),
    BufferSize(u8),
    RecallScene(u8),
    /// [view] Monitor column SHOW: AUX / CTRL ROOM.
    MonitorPage(MonitorPage),
    /// [view] Monitor column CUE OUTPUTS popover.
    CueOutputs,
    /// [view] Headphone output source popover (index into `headphone_outputs`).
    HeadphoneMenu(u8),
    /// [model] Monitor DIM / talkback, and the speaker set (0 = main MONITOR, 1 = ALT 1, 2 = ALT 2).
    MonitorDim,
    Talkback,
    /// [model] Talkback strip TALKBACK TO MONITOR (`/TalkbackInPhysicalCR`).
    TalkbackToMonitor,
    /// [model] View > Show/Hide Channels row: toggles the strip's `ChannelHidden` (strip = the
    /// channel's id from `MixerModel::channel_list`).
    ChannelVisible,
    /// [view] Closes the Show/Hide Channels window.
    ChannelsClose,
    MonitorAlt(u8),
    /// [model] Dim attenuation knob (drag 0…1).
    DimLevel,
    /// [model] ALT 1 / ALT 2 TRIM knob (k = 0, 1; drag 0…1 = −30…+30 dB, `AltMonTrim`).
    AltTrim(u8),
    /// [model] Cue output `MixInSource` (cue mix ↔ monitor mix) and `MixToMono`.
    CueSource(u8),
    CueMono(u8),
    /// [model] Headphone output source (`MixInSource`): option index into `HeadphoneOutput::options`.
    HeadphoneSource { output: u8, option: u8 },
    /// [view] Popover backdrop (tap outside), text-edit OK / CANCEL.
    PopoverDismiss,
    TextCommit,
    TextCancel,
    /// [view] Settings sheet: close, and the rate / clock / buffer pickers.
    SettingsClose,
    SettingsRate,
    SettingsClock,
    SettingsBuffer,
    /// [model] Settings "Writes enabled" (the connection's write gate).
    WritesEnabled,
    /// [model] `set_text` target for the session tempo (`/Tempo`).
    Tempo,
    // Plug-in windows (`plugin-ui`); the strip is the window's slot id. Append-only below.
    /// A plug-in parameter by ordinal (`<slot>/parameters/<k>`): knob and bar drags.
    PluginParam(u16),
    /// A plug-in panel spec element by index (switch, button, zone, menu): tap.
    PluginElement(u16),
    /// Set plug-in parameter `.0` to step `.1` (segmented switches, step selectors): tap.
    PluginStep(u16, u16),
    /// A button of the plug-in window's chrome (title bar, UAPW toolbar, footer): tap.
    PluginWindow(PluginWindowButton),
    // Settings window and secondary panels (`surfaces::settings`, `surfaces::panels`); fields
    // are named by their `settings::SettingField::key`. Append-only below.
    /// [view] Settings window tab.
    SettingsTab(SettingsTab),
    /// [view] HARDWARE › DEVICES: show the OPTIONS of device `.0` (engine device index).
    SettingsDevice(u8),
    /// [view] Open the dropdown of a settings combo.
    SettingMenu(Arc<str>),
    /// [model] Choose option `.1` of a settings combo (or a segmented pair).
    SettingChoose(Arc<str>, u16),
    /// [model] Press a settings toggle or button.
    SettingPress(Arc<str>),
    /// [view] Focus a settings text field; as a `set_text` target, the field's new text.
    SettingEdit(Arc<str>),
    /// [view] Open a secondary panel; close the open one; select its tab.
    ShowPanel(Panel),
    PanelClose,
    PanelTab(u8),
    /// [view] PLUG-IN SCENES: select (prime) scene `.0`; step the selection up / down.
    SceneSelect(u16),
    SceneStep(bool),
    /// [model] `post /SceneOperation {"<op>Scene": index}` (`MxClnt_*PluginScenesData`
    /// 0x181903810 / 0x181904080 / 0x181b0ffd0 / 0x181b105b0): Save (StoreScene), Overwrite,
    /// Clear, Recall. Overwrite and Clear reach this only from their confirmation.
    SceneCommand(crate::menu::SceneOp, u16),
    /// [model] `set_text` target: scene `.0`'s new name (`SetSceneName`, upper-cased).
    SceneName(u16),
    /// [view] TEMPO window TAP (`Rack_TempoPopover::buttonClicked` 0x1811a0680).
    TempoTap,
    /// [view] Settings page scrollbar (drag; value = 1 − scroll fraction).
    SettingsScroll,
    /// [view] Select row `.0` of a panel list (ADD DEVICE, I/O PRESETS, source categories).
    ListSelect(u16),
    /// [model] Drag a settings value (send faders): `Action::Set(.., 0…1)` on the key.
    SettingSet(Arc<str>),
    // Sessions (`surfaces::session`).
    /// [view] Navigator SESSION box: the session menu.
    SessionMenu,
    /// [model] A session request. `ViewState::route` asks about unsaved changes and opens the
    /// Save As / Open fields first; what is left reaches the model as a tap.
    Session(SessionOp),
    /// [view] The save-changes question's buttons.
    SessionSaveChoice(SaveChoice),
    /// [model] `set_text` targets: Save As name, Open path (engine host).
    SessionSaveAsName,
    SessionOpenPath,
    /// [model] `set_text` targets: a session file's new name, a new folder's name.
    SessionRenameTo(u16),
    SessionNewFolderName,
    /// [model] `set_text` targets of a CURRENT SESSION folder row (by panel row index): its new
    /// name (`RenameSession` renames folders too) and a new sub-folder's name.
    SessionFolderRenameTo(u16),
    SessionNewFolderIn(u16),
    /// [view] CURRENT SESSION panel: a tree row (folder or file, by panel row index) and the
    /// ••• button.
    SessionPanelRow(u16),
    SessionPanelDots,
    /// A popup-menu row or menu-bar title (`menu`): view commands (`MenuCmd::is_view`) are
    /// consumed by `ViewState`, the rest reach `MixerModel::apply` as `Action::Tap`.
    Menu(crate::menu::MenuCmd),
    // Control interaction rules (manual "Controls Shortcuts"). Append-only below.
    /// [view → model] Option/Alt-click on a Mute or Solo (`.0`): every strip of the clicked
    /// one's kind (inputs or auxes) takes the clicked one's opposite state (`ViewState::route`).
    ToggleAll(Box<Control>),
}

/// Plug-in window chrome buttons: UAD Console's `Rack_PluginWindow` title bar, the
/// `Rack_NewPlugInHeader` toolbar (element ids in comments) and `Rack_PlugInFooter`.
/// Append-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginWindowButton {
    /// Title-bar close.
    Close,
    /// `channel_strip`: rack-controls / plug-in GUI view toggle.
    RackView,
    /// `assign` (⋮): the insert menu.
    Assign,
    /// A row of the preset menu (the presets field), by index into the window's flattened list.
    PresetItem(u16),
    /// The preset menu's "Preset Browser...": the mixer's PRESETS tab targeted at this insert.
    PresetBrowser,
    /// The preset menu's "Save Preset": over the current preset (`SaveAsPreset`).
    PresetSave,
    /// `prev_preset`.
    PresetPrev,
    /// `next_preset`.
    PresetNext,
    /// `presets`: the current-preset field; opens the preset browser.
    Presets,
    /// `sidechain`: sidechain source picker.
    Sidechain,
    /// `power`: slot bypass.
    Power,
    /// `in_out`.
    InOut,
    /// `a` / `b`: compare.
    CompareA,
    CompareB,
    /// `copy` / `paste`.
    Copy,
    Paste,
    /// `mixer`: multi-out mixer.
    Mixer,
    /// `more` (•••, "UAD-2 •••"): scale, overflowed items, Resize, Apply To All, Help & Video.
    More,
    /// Footer `state_button` (license/device status).
    FooterState,
    /// Footer `action_button` (BUY / START DEMO).
    FooterAction,
    /// Preset browser: favourite toggle.
    Favorite,
    /// "Help & Video".
    Help,
    /// "Resize Plug-In" scale in percent (the "more" menu).
    Scale(u16),
    /// "Apply To All" (the scale to every plug-in window).
    ApplyToAll,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HitKind {
    /// Click toggles.
    Toggle,
    /// Press toggles (on the way down, like [`HitKind::Toggle`]); releasing after holding it
    /// for [`LATCH_HOLD`] or longer turns it off again with `Action::Set(id, 0.0)`: a click
    /// latches, a long press is momentary. Console's TALK button (`Rack_Talkback`: mouse down
    /// 0x1814975b0 writes the opposite value, mouse up 0x1814977a0 writes false after the
    /// `Rack_ButtonOwner` hold time, 0.5 s at 0x1808f5b90).
    Latch,
    /// Click activates (e.g. open an insert).
    Tap,
    /// Vertical drag over `span` points covers 0…1; `value` is the current value; double-click
    /// resets to `reset`.
    Drag { value: f64, span: f32, reset: Option<f64> },
    /// A rotary knob: a [`HitKind::Drag`] that follows Settings › CONTROLS MODE
    /// ([`Interaction::controls_mode`]); the hit rect is centred on the knob.
    Knob { value: f64, span: f32, reset: Option<f64> },
}

/// Settings › OPTIONS › CONTROLS MODE (`/ControlsMode`): how a knob follows a drag. Faders
/// and other drags stay linear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControlsMode {
    /// Vertical (and a quarter of horizontal) motion, as for faders.
    #[default]
    Linear,
    /// The knob jumps to the pointer's angle around its centre.
    Circular,
    /// The angle turned around the centre moves the knob from where it was.
    RelativeCircular,
}

impl ControlsMode {
    /// The engine's `/ControlsMode` ("Circular", "Relative Circular", "Linear").
    pub fn of(model: &dyn crate::MixerModel) -> ControlsMode {
        match model.properties().and_then(|p| p.prop("/ControlsMode")).as_ref().and_then(|v| v.as_str()) {
            Some("Circular") => ControlsMode::Circular,
            Some("Relative Circular") => ControlsMode::RelativeCircular,
            _ => ControlsMode::Linear,
        }
    }
}

/// Modifier keys held at a press, in Console's terms: `alt` = Option (macOS) / Alt, `cmd` =
/// Command (macOS) / Ctrl (Windows, Linux). Shift (fine) reaches `pointer_move` as `fine`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods {
    pub alt: bool,
    pub cmd: bool,
}

/// A knob's travel: 270° from 7:30 to 4:30, 0 at 12 o'clock. [I] Console's exact sweep is not
/// measured; the knob films draw about this arc.
const KNOB_SWEEP_DEG: f64 = 270.0;

/// The pointer's angle (degrees, clockwise from 12 o'clock, −180…180) around `c`.
fn angle(c: (f32, f32), x: f32, y: f32) -> f64 {
    ((x - c.0) as f64).atan2((c.1 - y) as f64).to_degrees()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub rect: Rect,
    pub id: ControlId,
    pub kind: HitKind,
}

/// What a [`ScrollRegion`] scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollTarget {
    Browser,
    Cells,
    /// The settings page or panel list.
    Settings,
    /// The bus section's cells (aux and talkback strips).
    AuxCells,
}

/// A wheel/trackpad-scrollable area; `max` is the content overflow in points.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrollRegion {
    pub rect: Rect,
    pub target: ScrollTarget,
    pub max: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    GestureBegin(ControlId),
    Set(ControlId, f64),
    /// Option/Alt-click, or a double click / tap: back to the default. `.1` is the UI's default (what to show);
    /// an engine model sends Console's `set <prop>/value "default"` instead of a number.
    Reset(ControlId, f64),
    GestureEnd(ControlId),
    Toggle(ControlId),
    Tap(ControlId),
}

impl Action {
    pub fn id(&self) -> &ControlId {
        match self {
            Action::GestureBegin(id) | Action::Set(id, _) | Action::Reset(id, _) | Action::GestureEnd(id) | Action::Toggle(id) | Action::Tap(id) => id,
        }
    }
}

struct Active {
    id: ControlId,
    start_y: f32,
    start_x: f32,
    start_value: f64,
    span: f32,
    /// A knob under a circular CONTROLS MODE: its centre and the last pointer angle.
    circular: Option<((f32, f32), f64)>,
    /// The pressed control's value at the press, and every control the drag moves with their
    /// values at the press (Cmd: the same control on each strip of its kind; else just it).
    origin: f64,
    group: Vec<(ControlId, f64)>,
}

impl Active {
    /// The pressed control at `v`: the group moves by the same amount, stopping when one of
    /// them reaches 0 or 1 (Console: relative offsets kept until one hits min/max).
    fn sets(&self, v: f64) -> Vec<Action> {
        let lo = self.group.iter().map(|g| g.1).fold(1.0, f64::min);
        let hi = self.group.iter().map(|g| g.1).fold(0.0, f64::max);
        let d = (v - self.origin).clamp(-lo, 1.0 - hi);
        self.group.iter().map(|(id, s)| Action::Set(id.clone(), (s + d).clamp(0.0, 1.0))).collect()
    }
}

/// The same control as `hit` on every strip of its kind (inputs, auxes; the talkback strip is
/// its own kind), topmost hit per strip, `hit` first. ponytail: strips with no hit on screen
/// (cells scrolled out of view) are not included; use the model's strip list if that matters.
fn peers<'a>(hits: &'a [Hit], hit: &'a Hit) -> Vec<&'a Hit> {
    use crate::settings::{Panel, SendsOf};
    let talkback = |s: &str| hits.iter().any(|h| &*h.id.strip == s && h.id.control == Control::ShowPanel(Panel::Sends(SendsOf::Talkback)));
    let kind = |s: &str| if talkback(s) { s.to_owned() } else { s.trim_end_matches(|c: char| c.is_ascii_digit()).to_owned() };
    let want = kind(&hit.id.strip);
    let mut out = vec![hit];
    for h in hits.iter().rev() {
        if h.id.control == hit.id.control && !out.iter().any(|o| o.id.strip == h.id.strip) && kind(&h.id.strip) == want {
            out.push(h);
        }
    }
    out
}

/// How long a [`HitKind::Latch`] press is held before its release turns it off.
pub const LATCH_HOLD: std::time::Duration = std::time::Duration::from_millis(500);

/// Pointer state machine. Hits are searched topmost-first (last registered wins).
#[derive(Default)]
pub struct Interaction {
    /// CONTROLS MODE for [`HitKind::Knob`] drags (the host sets it from [`ControlsMode::of`]).
    pub controls_mode: ControlsMode,
    /// The modifier keys held at the next press (the host sets them before `pointer_down`).
    pub mods: KeyMods,
    active: Option<Active>,
    /// The [`HitKind::Latch`] control pressed, and when.
    latched: Option<(ControlId, std::time::Instant)>,
}

impl Interaction {
    /// A press. Console's modifiers (manual "Controls Shortcuts"): Option/Alt-click returns a
    /// knob or fader to its default (`Action::Reset`: the engine gets `"default"`), with Cmd
    /// (Ctrl) every strip's; Option/Alt-click on a Mute or Solo sets every strip
    /// ([`Control::ToggleAll`]); Cmd (Ctrl)-drag moves the same control on every strip.
    pub fn pointer_down(&mut self, hits: &[Hit], x: f32, y: f32, double: bool) -> Vec<Action> {
        let Some(hit) = hits.iter().rev().find(|h| h.rect.contains(x, y)) else { return vec![] };
        let mods = self.mods;
        match &hit.kind {
            HitKind::Toggle if mods.alt && matches!(hit.id.control, Control::Mute | Control::Solo) => {
                vec![Action::Toggle(ControlId { strip: hit.id.strip.clone(), control: Control::ToggleAll(Box::new(hit.id.control.clone())) })]
            }
            HitKind::Toggle => vec![Action::Toggle(hit.id.clone())],
            HitKind::Latch => {
                self.latched = Some((hit.id.clone(), std::time::Instant::now()));
                vec![Action::Toggle(hit.id.clone())]
            }
            HitKind::Tap => {
                let mut id = hit.id.clone();
                if double {
                    id.control = match id.control {
                        Control::BrowserItem(i) => Control::BrowserActivate(i),
                        Control::BrowserPresetFile(top, child) => Control::BrowserPresetActivate(top, child),
                        other => other,
                    };
                }
                vec![Action::Tap(id)]
            }
            HitKind::Drag { value, span, reset } | HitKind::Knob { value, span, reset } => {
                if mods.alt {
                    // Back to the default (the engine resolves `"default"`; a control without a
                    // known UI default shows its value until the engine's echo).
                    let group = if mods.cmd { peers(hits, hit) } else { vec![hit] };
                    let mut out = vec![Action::GestureBegin(hit.id.clone())];
                    for h in group {
                        if let HitKind::Drag { value, reset, .. } | HitKind::Knob { value, reset, .. } = &h.kind {
                            out.push(Action::Reset(h.id.clone(), reset.unwrap_or(*value)));
                        }
                    }
                    out.push(Action::GestureEnd(hit.id.clone()));
                    return out;
                }
                if double {
                    if let Some(r) = reset {
                        return vec![
                            Action::GestureBegin(hit.id.clone()),
                            Action::Reset(hit.id.clone(), *r),
                            Action::GestureEnd(hit.id.clone()),
                        ];
                    }
                }
                let knob = matches!(hit.kind, HitKind::Knob { .. });
                let circular = (knob && self.controls_mode != ControlsMode::Linear).then(|| {
                    let c = hit.rect.center();
                    (c, angle(c, x, y))
                });
                let group = if mods.cmd { peers(hits, hit) } else { vec![hit] };
                let group = group
                    .into_iter()
                    .filter_map(|h| match h.kind {
                        HitKind::Drag { value, .. } | HitKind::Knob { value, .. } => Some((h.id.clone(), value)),
                        _ => None,
                    })
                    .collect();
                let a = Active { id: hit.id.clone(), start_y: y, start_x: x, start_value: *value, span: *span, circular, origin: *value, group };
                let mut out = vec![Action::GestureBegin(hit.id.clone())];
                // Circular: the press itself moves the knob to the pointer.
                if let (Some((_, angle)), ControlsMode::Circular) = (circular, self.controls_mode) {
                    out.extend(a.sets(Self::circular_value(angle)));
                }
                self.active = Some(a);
                out
            }
        }
    }

    /// The knob value at pointer angle `a` (the dead zone at 6 o'clock snaps to the nearer end).
    fn circular_value(a: f64) -> f64 {
        (a / KNOB_SWEEP_DEG + 0.5).clamp(0.0, 1.0)
    }

    /// `fine` (Shift) divides the motion by 10 (Console's fine adjust).
    pub fn pointer_move(&mut self, x: f32, y: f32, fine: bool) -> Vec<Action> {
        let mode = self.controls_mode;
        let Some(a) = &mut self.active else { return vec![] };
        let scale = if fine { 10.0 } else { 1.0 };
        if let Some((c, last)) = &mut a.circular {
            let now = angle(*c, x, y);
            let v = if mode == ControlsMode::Circular {
                Self::circular_value(now)
            } else {
                // Relative: the turn since the last move (unwrapped across 6 o'clock) accumulates.
                let turn = (now - *last + 540.0).rem_euclid(360.0) - 180.0;
                *last = now;
                a.start_value = (a.start_value + turn / (KNOB_SWEEP_DEG * scale)).clamp(0.0, 1.0);
                a.start_value
            };
            return a.sets(v);
        }
        let delta = ((a.start_y - y) as f64 + (x - a.start_x) as f64 * 0.25) / (a.span.max(1.0) as f64 * scale);
        a.sets((a.start_value + delta).clamp(0.0, 1.0))
    }

    pub fn pointer_up(&mut self) -> Vec<Action> {
        self.pointer_up_at(std::time::Instant::now())
    }

    /// [`Interaction::pointer_up`] at `now`: ends a drag; a [`HitKind::Latch`] held for
    /// [`LATCH_HOLD`] turns off (`Action::Set(id, 0.0)`).
    pub fn pointer_up_at(&mut self, now: std::time::Instant) -> Vec<Action> {
        let mut out = match self.active.take() {
            Some(a) => vec![Action::GestureEnd(a.id)],
            None => vec![],
        };
        if let Some((id, at)) = self.latched.take()
            && now.saturating_duration_since(at) >= LATCH_HOLD
        {
            out.push(Action::Set(id, 0.0));
        }
        out
    }

    pub fn is_dragging(&self) -> bool {
        self.active.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ControlId {
        ControlId { strip: "in0".into(), control: Control::Fader }
    }

    #[test]
    fn drag_and_reset() {
        let hits = vec![Hit { rect: Rect::new(0.0, 0.0, 50.0, 300.0), id: id(), kind: HitKind::Drag { value: 0.5, span: 200.0, reset: Some(0.78) } }];
        let mut i = Interaction::default();
        assert_eq!(i.pointer_down(&hits, 10.0, 100.0, false), vec![Action::GestureBegin(id())]);
        assert_eq!(i.pointer_move(10.0, 60.0, false), vec![Action::Set(id(), 0.7)]);
        assert_eq!(i.pointer_up(), vec![Action::GestureEnd(id())]);
        let reset = i.pointer_down(&hits, 10.0, 100.0, true);
        assert_eq!(reset[1], Action::Reset(id(), 0.78));
    }

    fn cid(strip: &str, control: Control) -> ControlId {
        ControlId { strip: strip.into(), control }
    }

    /// Two input faders and an aux fader (in0 at 0.5, in1 at 0.9, aux0 at 0.3), two input
    /// mutes.
    fn strip_hits() -> Vec<Hit> {
        let drag = |value| HitKind::Drag { value, span: 100.0, reset: Some(0.78) };
        vec![
            Hit { rect: Rect::new(0.0, 0.0, 50.0, 300.0), id: cid("/d/inputs/0", Control::Fader), kind: drag(0.5) },
            Hit { rect: Rect::new(100.0, 0.0, 50.0, 300.0), id: cid("/d/inputs/1", Control::Fader), kind: drag(0.9) },
            Hit { rect: Rect::new(200.0, 0.0, 50.0, 300.0), id: cid("/d/auxs/0", Control::Fader), kind: drag(0.3) },
            Hit { rect: Rect::new(0.0, 310.0, 50.0, 20.0), id: cid("/d/inputs/0", Control::Mute), kind: HitKind::Toggle },
            Hit { rect: Rect::new(100.0, 310.0, 50.0, 20.0), id: cid("/d/inputs/1", Control::Mute), kind: HitKind::Toggle },
        ]
    }

    fn with(alt: bool, cmd: bool) -> Interaction {
        Interaction { mods: KeyMods { alt, cmd }, ..Interaction::default() }
    }

    /// Option-click: the control's default (`Reset`, sent as `"default"`), no drag.
    #[test]
    fn option_click_resets() {
        let hits = strip_hits();
        let mut i = with(true, false);
        let in0 = cid("/d/inputs/0", Control::Fader);
        assert_eq!(i.pointer_down(&hits, 10.0, 100.0, false), vec![Action::GestureBegin(in0.clone()), Action::Reset(in0.clone(), 0.78), Action::GestureEnd(in0)]);
        assert!(!i.is_dragging());
    }

    /// Cmd+Option-click: every input's fader resets (not the aux's).
    #[test]
    fn cmd_option_click_resets_every_strip_of_the_kind() {
        let hits = strip_hits();
        let out = with(true, true).pointer_down(&hits, 110.0, 100.0, false);
        let resets: Vec<_> = out.iter().filter_map(|a| if let Action::Reset(id, v) = a { Some((&*id.strip, *v)) } else { None }).collect();
        assert_eq!(resets, vec![("/d/inputs/1", 0.78), ("/d/inputs/0", 0.78)]);
    }

    /// Option-click on a Mute: `ToggleAll(Mute)` (the view sets every strip).
    #[test]
    fn option_click_mute_toggles_all() {
        let hits = strip_hits();
        assert_eq!(with(true, false).pointer_down(&hits, 10.0, 315.0, false), vec![Action::Toggle(cid("/d/inputs/0", Control::ToggleAll(Box::new(Control::Mute))))]);
        assert_eq!(with(false, true).pointer_down(&hits, 10.0, 315.0, false), vec![Action::Toggle(cid("/d/inputs/0", Control::Mute))]);
    }

    /// Cmd-drag: both input faders move by the same amount and stop when one reaches the top.
    #[test]
    fn cmd_drag_moves_every_strip_keeping_offsets() {
        let hits = strip_hits();
        let mut i = with(false, true);
        i.pointer_down(&hits, 10.0, 100.0, false);
        let (in0, in1) = (cid("/d/inputs/0", Control::Fader), cid("/d/inputs/1", Control::Fader));
        let set = |a: Vec<Action>| a.into_iter().map(|a| if let Action::Set(id, v) = a { (id, (v * 1000.0).round() / 1000.0) } else { panic!("{a:?}") }).collect::<Vec<_>>();
        assert_eq!(set(i.pointer_move(10.0, 95.0, false)), vec![(in0.clone(), 0.55), (in1.clone(), 0.95)]);
        assert_eq!(set(i.pointer_move(10.0, 50.0, false)), vec![(in0.clone(), 0.6), (in1.clone(), 1.0)], "stops at in1's max");
        assert_eq!(set(i.pointer_move(10.0, 150.0, true)), vec![(in0, 0.45), (in1, 0.85)], "Shift: a tenth");
    }

    /// TALK: a click latches (toggle on press, nothing on release); held for 0.5 s it is
    /// momentary (released: off).
    #[test]
    fn latch_click_toggles_and_a_long_press_is_momentary() {
        let talk = ControlId { strip: "monitor".into(), control: Control::Talkback };
        let hits = vec![Hit { rect: Rect::new(0.0, 0.0, 80.0, 40.0), id: talk.clone(), kind: HitKind::Latch }];
        let mut i = Interaction::default();
        assert_eq!(i.pointer_down(&hits, 10.0, 10.0, false), vec![Action::Toggle(talk.clone())]);
        let pressed = std::time::Instant::now();
        assert_eq!(i.pointer_up_at(pressed + std::time::Duration::from_millis(200)), vec![]);
        assert_eq!(i.pointer_down(&hits, 10.0, 10.0, false), vec![Action::Toggle(talk.clone())]);
        let held = std::time::Instant::now() + LATCH_HOLD;
        assert_eq!(i.pointer_up_at(held), vec![Action::Set(talk.clone(), 0.0)]);
        // A release with no press does nothing.
        assert_eq!(i.pointer_up_at(held), vec![]);
    }
}
