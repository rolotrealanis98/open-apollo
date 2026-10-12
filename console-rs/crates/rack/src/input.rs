//! Hit regions and pointer interaction → model actions.

use std::sync::Arc;

use houston::Rect;

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
    MonitorLevel,
    MonitorMono,
    MonitorMute,

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
    BrowserGroupReveal,
    /// [view] Browser scrollbar thumb (drag; value = 1 − scroll fraction).
    BrowserScroll,
    /// [view] Select a catalog entry (index into `plugin_catalog`); the next insert tap assigns it.
    BrowserItem(u16),
    /// [view] Session navigator: make this input the first visible strip.
    SessionNavigate(u16),
    /// [view] Cells scrollbar thumb (drag; value = 1 − scroll fraction).
    CellsScroll,
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
    MonitorAlt(u8),
    /// [model] Dim attenuation knob (drag 0…1).
    DimLevel,
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
}

#[derive(Clone, Debug, PartialEq)]
pub enum HitKind {
    /// Click toggles.
    Toggle,
    /// Click activates (e.g. open an insert).
    Tap,
    /// Vertical drag over `span` points covers 0…1; `value` is the current value; double-click
    /// resets to `reset`.
    Drag { value: f64, span: f32, reset: Option<f64> },
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
    GestureEnd(ControlId),
    Toggle(ControlId),
    Tap(ControlId),
}

impl Action {
    pub fn id(&self) -> &ControlId {
        match self {
            Action::GestureBegin(id) | Action::Set(id, _) | Action::GestureEnd(id) | Action::Toggle(id) | Action::Tap(id) => id,
        }
    }
}

struct Active {
    id: ControlId,
    start_y: f32,
    start_x: f32,
    start_value: f64,
    span: f32,
}

/// Pointer state machine. Hits are searched topmost-first (last registered wins).
#[derive(Default)]
pub struct Interaction {
    active: Option<Active>,
}

impl Interaction {
    pub fn pointer_down(&mut self, hits: &[Hit], x: f32, y: f32, double: bool) -> Vec<Action> {
        let Some(hit) = hits.iter().rev().find(|h| h.rect.contains(x, y)) else { return vec![] };
        match &hit.kind {
            HitKind::Toggle => vec![Action::Toggle(hit.id.clone())],
            HitKind::Tap => vec![Action::Tap(hit.id.clone())],
            HitKind::Drag { value, span, reset } => {
                if double {
                    if let Some(r) = reset {
                        return vec![
                            Action::GestureBegin(hit.id.clone()),
                            Action::Set(hit.id.clone(), *r),
                            Action::GestureEnd(hit.id.clone()),
                        ];
                    }
                }
                self.active = Some(Active { id: hit.id.clone(), start_y: y, start_x: x, start_value: *value, span: *span });
                vec![Action::GestureBegin(hit.id.clone())]
            }
        }
    }

    /// `fine` divides the motion by 10 (Console's modifier-drag).
    pub fn pointer_move(&mut self, x: f32, y: f32, fine: bool) -> Vec<Action> {
        let Some(a) = &self.active else { return vec![] };
        let scale = if fine { 10.0 } else { 1.0 };
        let delta = ((a.start_y - y) as f64 + (x - a.start_x) as f64 * 0.25) / (a.span.max(1.0) as f64 * scale);
        vec![Action::Set(a.id.clone(), (a.start_value + delta).clamp(0.0, 1.0))]
    }

    pub fn pointer_up(&mut self) -> Vec<Action> {
        match self.active.take() {
            Some(a) => vec![Action::GestureEnd(a.id)],
            None => vec![],
        }
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
        assert_eq!(reset[1], Action::Set(id(), 0.78));
    }
}
