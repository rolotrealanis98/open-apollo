//! Console's popup menus: context menus, option menus and the menu bar, with the look of
//! Rack's JUCE `PopupMenu` LookAndFeel. The catalog with provenance is
//! `console-port/surfaces/menus.md`.
//!
//! Look (Rack.dll 1.2.x; `PopupMenu::LookAndFeelMethods` is the vtable at +0x40 of
//! `Rack_LookAndFeelBase` 0x1838c8030, `Rack_ContextLookAndFeel` 0x18384d338 for context menus):
//! * background: `backgrounds:Popup-Menu-Background` 9-slice over the whole window
//!   (`drawPopupMenuBackground` 0x180cafc90); JUCE's default 2-pt window border.
//! * rows (`getIdealPopupMenuItemSize` 0x180cb3460 / context 0x180c9dc20): items 22 pt, separators
//!   2 pt; width = text + 3 × font height (base) or text + 2 × 22 (context). Section headers are
//!   JUCE's `HeaderItemComponent`: 1.5 × the row height, width + ¼.
//! * item (`drawPopupMenuItem` 0x180cafd50): separator = two 1-pt lines from x 1 to w − 1,
//!   (135,145,149) then (202,212,216); highlight = (227,231,233) rounded rect (0, 1, w, h − 2),
//!   corner 2 (`inset-background` 1,0,1,0 read top-first); text `views:Popup-Menu-Item` (black
//!   `text-on` / `text-off`) in (12, 2, w − 24, h − 2) centred-left, opacity 0.3 when disabled;
//!   shortcut right-aligned in (0, 0, w − 8, h); ticked = `misc/dropdown_in_use_dot`, submenu =
//!   `misc/dropdown_submenu_chevron`.
//! * context section header (`drawPopupMenuSectionHeader` 0x180c9dad0): band in palette
//!   `context_menu_section_header_bg`, bold popup font in `context_menu_section_header_text`
//!   at (x + 5, y + 1, w − 10, h), centred-left.
//! * menu bar (`Rack_WinMenuBarLookAndFeel`, Windows title bar): `views:Menu-Bar-Item-Win`.

use std::sync::Arc;

use houston::{Rect, Rgba};

use crate::draw::Align;
use crate::input::{Control, ControlId, Hit, HitKind};
use crate::model::{InsertState, MixerModel, StripInfo};
use crate::surfaces::{Popover, PopoverState, Sheet, TextEdit, TextTarget, ViewState};
use crate::ui::Ui;
use crate::window::{helvetica_bold_width, helvetica_width};

// MARK: metrics

/// Item row height (`getIdealPopupMenuItemSize`: 0x16).
pub const ITEM_H: f32 = 22.0;
/// Separator row height.
pub const SEPARATOR_H: f32 = 2.0;
/// Section header row: a row like the items (Console 1.3.1 captures s01/s02: 44 px bands).
pub const HEADER_H: f32 = ITEM_H;
/// Rows sit 2 pt inside the window on every side (JUCE `getPopupMenuBorderSize`; s01–s04).
pub const BORDER: f32 = 2.0;
/// Text inset from the row's left edge (s02/s04: the first glyph 10.5–11 pt in) and the total
/// horizontal text margin.
const TEXT_X: f32 = 10.0;
const TEXT_MARGIN: f32 = 20.0;
/// Current-value bullet (`dropdown_in_use_dot`, 4 pt): 3 pt into the row, centred (s04, s18).
const BULLET_X: f32 = 3.0;
const BULLET: f32 = 4.0;
// Colours from the colour-managed drive captures (`screencapture`, like refpairs p1/p2); the
// earlier clipboard captures s01–s42 carry no display profile and read 8–10 levels lighter.
/// Separator: a 1-pt dark line over a 1-pt light one (hov-pan).
const SEP_DARK: Rgba = Rgba::rgb8(124, 134, 138);
const SEP_LIGHT: Rgba = Rgba::rgb8(195, 206, 211);
/// Highlighted row, the full row (hov-ins / hov-lbl / hov-rate: 22 pt × the inner width).
const HIGHLIGHT: Rgba = Rgba::rgb8(223, 227, 230);
const HEADER_BG: Rgba = Rgba::rgb8(156, 164, 170);
const HEADER_TEXT: Rgba = Rgba::rgb8(29, 77, 115);
/// Section header text: the item font, bold (s01/s02: header and item glyph widths match).
const HEADER_FONT_SCALE: f32 = 1.0;
/// Window edge, outside in (ins-in-empty at 2 px per pt): a 0.5-pt black outline outside the
/// window (square), a 1-pt border with 2-pt rounded corners, a 1-pt light line, then the body.
const WINDOW_OUTLINE: Rgba = Rgba::rgb8(4, 4, 4);
const WINDOW_BORDER: Rgba = Rgba::rgb8(145, 151, 155);
const WINDOW_LIGHT: Rgba = Rgba::rgb8(177, 187, 193);
const WINDOW_BODY: Rgba = Rgba::rgb8(176, 186, 192);
/// Rounded corner of the border, per half-point row from the top (and mirrored at the bottom):
/// how far the row starts in from each side (ins-in-empty, 2 px per pt).
const BORDER_CORNER: [f32; 3] = [1.5, 0.5, 0.5];
/// The light line's first half-point row starts this much further in.
const LIGHT_CORNER: [f32; 1] = [0.5];
/// Disabled text (ins-in-empty "Paste": (124,131,135) on the body = black at 0.3).
const DISABLED_ALPHA: f32 = 0.3;
/// Console menus draw their text 1 pt below the row's centre (every row of lbl-mono and
/// ins-in-empty: ink 6.0 pt from the row top vs 5.0 centred).
const TEXT_DY: f32 = 1.0;
/// Hover / open menu-bar title overlay (`Menu-Bar-Item-Win` `color-mouseover`).
const BAR_HOVER: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.2 };

/// Which LookAndFeel draws a menu. Console has two styles: the light console menus
/// (`Rack_LookAndFeelBase` for menu-bar and combo menus, `Rack_ContextLookAndFeel` for right
/// clicks: wider rows, coloured section headers; captures s01–s04, s18–s40) and the dark
/// plug-in window menus (UAPW toolbar "more" menu, s42: check marks, white text).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuLook {
    #[default]
    Base,
    Context,
    /// The plug-in window's dark menu.
    Plugin,
}

/// Colours and insets of a [`MenuLook`], measured on Console 1.3.1 (2 px per pt).
struct LookStyle {
    outline: Option<Rgba>,
    border: Rgba,
    light: Rgba,
    body: Rgba,
    text: Rgba,
    highlight: Rgba,
    /// Highlight inset top and bottom (s42: 1 pt; s18: none).
    highlight_inset: f32,
    text_x: f32,
    /// Ticked rows: the console bullet, or the plug-in window's check mark.
    check: bool,
    /// Item font relative to `Popup-Menu-Item` (s42's UAPW menu text is 0.9 of it).
    font_scale: f32,
    /// Text offset below the row's centre.
    text_dy: f32,
    /// Light line width inside the border (the body starts at 1 + this).
    light_w: f32,
    /// Rounded border corners (the console menus).
    rounded: bool,
}

impl MenuLook {
    fn style(self) -> LookStyle {
        match self {
            MenuLook::Base | MenuLook::Context => LookStyle {
                outline: Some(WINDOW_OUTLINE),
                border: WINDOW_BORDER,
                light: WINDOW_LIGHT,
                body: WINDOW_BODY,
                text: Rgba::BLACK,
                highlight: HIGHLIGHT,
                highlight_inset: 0.0,
                text_x: TEXT_X,
                check: false,
                font_scale: 1.0,
                text_dy: TEXT_DY,
                light_w: 1.0,
                rounded: true,
            },
            // s42: 1-pt (70,80,86) border, a 0.5-pt (54,65,72) line, (50,62,68) body; rows of
            // white bold text 19 pt in (room for the check mark at 7 pt); highlight (108,117,121)
            // inset 1 pt; separators a 1-pt (118,125,129) line 3 pt in from each side.
            MenuLook::Plugin => LookStyle {
                outline: None,
                border: Rgba::rgb8(70, 80, 86),
                light: Rgba::rgb8(54, 65, 72),
                body: Rgba::rgb8(50, 62, 68),
                text: Rgba::WHITE,
                highlight: Rgba::rgb8(108, 117, 121),
                highlight_inset: 1.0,
                text_x: 19.0,
                check: true,
                font_scale: 0.9,
                text_dy: 0.0,
                light_w: 0.5,
                rounded: false,
            },
        }
    }
}

// MARK: menu model

/// One clickable row.
#[derive(Clone, Debug, PartialEq)]
pub struct PopupItem {
    pub label: String,
    /// What a click sends (None: drawn, not clickable).
    pub id: Option<ControlId>,
    /// `Toggle` for controls the model toggles (preamp switches, isolate); `Tap` otherwise.
    pub toggle: bool,
    pub enabled: bool,
    pub ticked: bool,
    /// Key description drawn right-aligned (menu bar).
    pub shortcut: Option<String>,
    /// The Console command id behind a menu-bar item (`show_browser`, …): native menus run it
    /// through `ViewState::run_command_id`.
    pub command: Option<&'static str>,
}

impl PopupItem {
    pub fn new(label: impl Into<String>, id: ControlId) -> Self {
        Self { label: label.into(), id: Some(id), toggle: false, enabled: true, ticked: false, shortcut: None, command: None }
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn ticked(mut self, on: bool) -> Self {
        self.ticked = on;
        self
    }
    pub fn toggle(mut self) -> Self {
        self.toggle = true;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PopupEntry {
    Item(PopupItem),
    Separator,
    Header(String),
    Submenu { label: String, enabled: bool, menu: PopupMenu },
}

/// A JUCE `PopupMenu`: entries in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PopupMenu {
    pub entries: Vec<PopupEntry>,
}

impl PopupMenu {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, item: PopupItem) -> &mut Self {
        self.entries.push(PopupEntry::Item(item));
        self
    }
    pub fn item(&mut self, label: impl Into<String>, id: ControlId) -> &mut Self {
        self.push(PopupItem::new(label, id))
    }
    /// JUCE `addSeparator`: never first, never twice in a row.
    pub fn separator(&mut self) -> &mut Self {
        if matches!(self.entries.last(), Some(e) if !matches!(e, PopupEntry::Separator)) {
            self.entries.push(PopupEntry::Separator);
        }
        self
    }
    pub fn header(&mut self, title: impl Into<String>) -> &mut Self {
        self.entries.push(PopupEntry::Header(title.into()));
        self
    }
    pub fn submenu(&mut self, label: impl Into<String>, menu: PopupMenu, enabled: bool) -> &mut Self {
        self.entries.push(PopupEntry::Submenu { label: label.into(), enabled, menu });
        self
    }
    /// Drops a trailing separator (JUCE never draws one last).
    pub fn finish(mut self) -> Self {
        while matches!(self.entries.last(), Some(PopupEntry::Separator)) {
            self.entries.pop();
        }
        self
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// The submenu at `path` (entry indices from this menu).
    pub fn at_path(&self, path: &[u16]) -> Option<&PopupMenu> {
        let mut m = self;
        for &i in path {
            match m.entries.get(i as usize)? {
                PopupEntry::Submenu { menu, .. } => m = menu,
                _ => return None,
            }
        }
        Some(m)
    }
    /// Every item, depth first (tests, native menus).
    pub fn items(&self) -> Vec<&PopupItem> {
        let mut out = Vec::new();
        for e in &self.entries {
            match e {
                PopupEntry::Item(i) => out.push(i),
                PopupEntry::Submenu { menu, .. } => out.extend(menu.items()),
                _ => {}
            }
        }
        out
    }
    pub fn find(&self, label: &str) -> Option<&PopupItem> {
        self.items().into_iter().find(|i| i.label == label)
    }
}

// MARK: commands

/// A keyboard shortcut from `Resources/shortcuts/shortcuts.json` (Console 1.3.1 defaults).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    /// `shortcuts.json` key: a character or `left` / `right`.
    pub key: &'static str,
    /// "command": ⌘ on macOS, Ctrl elsewhere.
    pub command: bool,
    pub shift: bool,
    /// "option": ⌥ / Alt.
    pub option: bool,
    /// "control": ⌃ on macOS (with "command" on Windows it is plain Ctrl).
    pub control: bool,
}

impl Shortcut {
    const fn cmd(key: &'static str) -> Self {
        Self { key, command: true, shift: false, option: false, control: false }
    }
    const fn plain(key: &'static str) -> Self {
        Self { key, command: false, shift: false, option: false, control: false }
    }
    /// JUCE `KeyPress::getTextDescriptionWithIcons`: "⇧⌘S" on macOS, "ctrl + shift + S" elsewhere.
    pub fn text(&self, mac: bool) -> String {
        let key = match self.key {
            "left" => if mac { "←" } else { "cursor left" }.to_string(),
            "right" => if mac { "→" } else { "cursor right" }.to_string(),
            k => k.to_uppercase(),
        };
        if mac {
            let mut s = String::new();
            if self.control {
                s.push('⌃');
            }
            if self.option {
                s.push('⌥');
            }
            if self.shift {
                s.push('⇧');
            }
            if self.command {
                s.push('⌘');
            }
            s + &key
        } else {
            let mut s = String::new();
            if self.command || self.control {
                s += "ctrl + ";
            }
            if self.shift {
                s += "shift + ";
            }
            if self.option {
                s += "alt + ";
            }
            s + &key
        }
    }
}

/// Who carries out a [`Command`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandScope {
    /// `ViewState` (pages, browser, navigation).
    View,
    /// The model (engine writes behind the write gate).
    Model,
    /// The app shell (`ViewState::take_commands`): quit, full screen, URLs, windows, sessions.
    Shell,
}

macro_rules! commands {
    ($($v:ident = $id:literal, $label:literal, $scope:ident, $sc:expr;)*) => {
        /// The Console menu-bar commands (`Rack_KeyboardShortcuts` command registry, Rack.dll
        /// 0x180946be0 / 0x1809381d0: id → name; labels as the menu builders pass them).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Command { $($v),* }
        impl Command {
            pub const ALL: &'static [Command] = &[$(Command::$v),*];
            /// Console's command id.
            pub fn id(self) -> &'static str { match self { $(Command::$v => $id),* } }
            /// Menu text.
            pub fn label(self) -> &'static str { match self { $(Command::$v => $label),* } }
            pub fn scope(self) -> CommandScope { match self { $(Command::$v => CommandScope::$scope),* } }
            pub fn shortcut(self) -> Option<Shortcut> { match self { $(Command::$v => $sc),* } }
            pub fn from_id(id: &str) -> Option<Command> {
                Self::ALL.iter().copied().find(|c| c.id() == id)
            }
        }
    };
}

const fn sc(key: &'static str, command: bool, shift: bool, option: bool, control: bool) -> Option<Shortcut> {
    Some(Shortcut { key, command, shift, option, control })
}

commands! {
    // File (`Rack_MenuBarModel_Console` slot 14, 0x18161dc00: labels passed explicitly).
    NewSession = "select_new_session_panel", "New", Shell, Some(Shortcut::cmd("n"));
    OpenSession = "open_session", "Open...", Shell, Some(Shortcut::cmd("o"));
    SaveSession = "save_bookmark", "Save", Shell, Some(Shortcut::cmd("s"));
    SaveSessionAs = "save_as", "Save As...", Shell, sc("s", true, true, false, false);
    Quit = "quit", "Quit", Shell, Some(Shortcut::cmd("q"));
    // Edit (`UpdateEditMenu` 0x18161cee0).
    Undo = "undo", "Undo", Model, Some(Shortcut::cmd("z"));
    Redo = "redo", "Redo", Model, sc("z", true, true, false, false);
    RemoveAllPlugins = "remove_all_plugins", "Remove All Plug-Ins", Model, None;
    RemoveAllInsertPlugins = "remove_all_insert_plugins", "Remove All Insert Plug-Ins", Model, None;
    RemoveAllAuxPlugins = "remove_all_aux_plugins", "Remove All Aux Plug-Ins", Model, None;
    RemoveAllUnisonPlugins = "remove_all_unison_plugins", "Remove All Unison Plug-Ins", Model, None;
    EnableAllPlugins = "enable_all_plugins", "Enable All Plug-Ins", Model, None;
    EnableAllInsertPlugins = "enable_all_insert_plugins", "Enable All Insert Plug-Ins", Model, None;
    EnableAllAuxPlugins = "enable_all_aux_plugins", "Enable All Aux Plug-Ins", Model, None;
    EnableAllUnisonPlugins = "enable_all_unison_plugins", "Enable All Unison Plug-Ins", Model, None;
    DisableAllPlugins = "disable_all_plugins", "Disable All Plug-Ins", Model, None;
    DisableAllInsertPlugins = "disable_all_insert_plugins", "Disable All Insert Plug-Ins", Model, None;
    DisableAllAuxPlugins = "disable_all_aux_plugins", "Disable All Aux Plug-Ins", Model, None;
    DisableAllUnisonPlugins = "disable_all_unison_plugins", "Disable All Unison Plug-Ins", Model, None;
    // Navigation (0x18161e3b0).
    MixerScrollLeft = "mixer_scroll_left", "Mixer Scroll Left", View, Some(Shortcut::plain("left"));
    MixerScrollRight = "mixer_scroll_right", "Mixer Scroll Right", View, Some(Shortcut::plain("right"));
    MixerBankLeft = "mixer_bank_left", "Mixer Bank Left", View, Some(Shortcut::cmd("left"));
    MixerBankRight = "mixer_bank_right", "Mixer Bank Right", View, Some(Shortcut::cmd("right"));
    MixerFarLeft = "mixer_far_left", "Mixer Far Left", View, sc("left", true, false, true, false);
    MixerFarRight = "mixer_far_right", "Mixer Far Right", View, sc("right", true, false, true, false);
    MixerTabLeft = "mixer_tab_left", "Mixer Tab Left", View, sc("left", true, false, false, true);
    MixerTabRight = "mixer_tab_right", "Mixer Tab Right", View, sc("right", true, false, false, true);
    // View (`UpdateViewMenu` 0x18161eba0), "Section" submenu first.
    ShowBrowser = "show_browser", "Show Browser", View, Some(Shortcut::plain("`"));
    ShowMixerNav = "show_mixer_nav", "Show Mixer Navigation", View, None;
    ShowPluginScenes = "show_plugin_scenes", "Show Plug-In Scenes", View, Some(Shortcut::cmd("e"));
    ShowBuses = "show_buses", "Show Auxes", View, Some(Shortcut::cmd("a"));
    ShowControlRoom = "show_control_room", "Show Control Room", View, Some(Shortcut::cmd("r"));
    ShowMonitor = "show_monitor", "Show Monitor", View, None;
    ShowMonitorCorrection = "show_monitor_correction", "Show Monitor Correction", View, None;
    ShowNotifications = "show_notification_browser", "Notifications", Shell, None;
    ToggleCompactMonitor = "toggle_compact_monitor_fader", "Toggle Compact Monitor", View, None;
    ShowHideChannels = "show_hide_channels", "Show/Hide Channels", View, Some(Shortcut::cmd("i"));
    ShowHideFloatingWindows = "show_hide_floating_windows", "Show/Hide Floating Windows", Shell, sc("w", false, true, false, false);
    ShowOfflineDevices = "show_offline_devices", "Show Offline Devices", View, None;
    ShowSettings = "show_settings", "Show Settings", View, Some(Shortcut::cmd(","));
    // Window (`Rack_MenuBarModel::UpdateWindowMenu` 0x18161ab70).
    PreviousWindow = "cycle_windows_previous", "Previous Window", Shell, sc("`", true, true, false, false);
    NextWindow = "cycle_windows_next", "Next Window", Shell, Some(Shortcut::cmd("`"));
    ToggleFullScreen = "toggle_full_screen", "Full Screen On/Off", Shell, sc("f", true, true, false, false);
    // Help (`UpdateHelpMenu` 0x18161dfd0) and the macOS application menu (0x1816078d0).
    Documentation = "open_documentation_url_app", "Documentation...", Shell, None;
    GiveFeedback = "give_feedback", "Give Feedback...", Shell, None;
    About = "about", "About...", Shell, None;
}

/// The Console command bound to a key (`shortcuts.json` defaults): `key` as `shortcuts.json`
/// names it (a lowercase character, `left`, `right`); `command` is ⌘ on macOS / Ctrl elsewhere.
pub fn command_for_key(key: &str, command: bool, shift: bool, option: bool) -> Option<Command> {
    Command::ALL.iter().copied().find(|c| {
        c.shortcut().is_some_and(|s| s.key == key && (s.command || s.control) == command && s.shift == shift && s.option == option)
            && c.id() != "mixer_tab_left"
            && c.id() != "mixer_tab_right"
    })
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn entries_json(menu: &PopupMenu) -> String {
    let items: Vec<String> = menu
        .entries
        .iter()
        .map(|e| match e {
            PopupEntry::Separator => r#"{"type":"separator"}"#.to_string(),
            PopupEntry::Header(t) => format!(r#"{{"type":"header","label":{}}}"#, json_str(t)),
            PopupEntry::Submenu { label, enabled, menu } => {
                format!(r#"{{"type":"submenu","label":{},"enabled":{},"items":{}}}"#, json_str(label), enabled, entries_json(menu))
            }
            PopupEntry::Item(i) => {
                let sc = i
                    .command
                    .and_then(Command::from_id)
                    .and_then(Command::shortcut)
                    .map(|s| format!(r#"{{"key":{},"command":{},"shift":{},"option":{},"control":{}}}"#, json_str(s.key), s.command, s.shift, s.option, s.control))
                    .unwrap_or_else(|| "null".into());
                format!(
                    r#"{{"type":"item","label":{},"command":{},"enabled":{},"ticked":{},"shortcut":{}}}"#,
                    json_str(&i.label),
                    i.command.map(json_str).unwrap_or_else(|| "null".into()),
                    i.enabled,
                    i.ticked,
                    sc
                )
            }
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// The menu bar as JSON for native menus (ConsoleLink's `NSMenu` / `UIMenu`):
/// `{"app": [entries], "menus": [{"title", "items": [entries]}]}`; an entry is
/// `{"type": "item", "label", "command", "enabled", "ticked", "shortcut": {"key", "command",
/// "shift", "option", "control"} | null}`, `{"type": "separator"}`, `{"type": "header", "label"}`
/// or `{"type": "submenu", "label", "enabled", "items"}`. Run an item with
/// `ViewState::run_command_id(command)`.
pub fn menu_bar_json(model: &dyn MixerModel, view: &ViewState, mac: bool) -> String {
    let menus: Vec<String> = menu_bar(model, view, mac).iter().map(|b| format!(r#"{{"title":{},"items":{}}}"#, json_str(b.title), entries_json(&b.menu))).collect();
    let app = if mac { entries_json(&app_menu(model, view)) } else { "[]".into() };
    format!(r#"{{"app":{},"menus":[{}]}}"#, app, menus.join(","))
}

/// `open_documentation_url_app` (`Rack_Configuration::GetDeviceDocumentationURL` 0x180a189a0).
pub const DOCUMENTATION_URL: &str = "https://help.uaudio.com/hc/en-us/sections/12262368930324-Apollo-UAD-2-Documentation";
/// UAD Console Manual (`Rack.dll` 0x180a19fb0).
pub const CONSOLE_MANUAL_URL: &str = "https://help.uaudio.com/hc/en-us/sections/25052396869908-UAD-Console-Manual";
/// NETWORK › DANTE CONTROLLER › LAUNCH when Dante Controller isn't installed (`Rack.dll`
/// 0x18085ca10; Console launches `DanteController.exe` when it is).
pub const DANTE_CONTROLLER_URL: &str = "https://www.audinate.com/products/software/dante-controller";
/// NETWORK › DANTE PATENT INFORMATION › MORE INFO (`Rack.dll` 0x18085cc00).
pub const DANTE_PATENTS_URL: &str = "https://www.audinate.com/legal/patents-trademarks";
/// FEEDBACK / Help › Give Feedback... Console opens its REPORT FEEDBACK dialog, which submits
/// to UA's feedback service with the user's UA account; this console has no access to that
/// service, so feedback goes to the open-apollo project instead.
pub const FEEDBACK_URL: &str = "https://github.com/rolotrealanis98/open-apollo/issues/new";

/// Plug-in operations of the Edit menu and the inserts context menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginOp {
    Remove,
    Enable,
    Disable,
}

/// Which slots an Edit-menu plug-in operation covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginScope {
    All,
    /// Input channel inserts.
    Inserts,
    /// Aux inserts.
    Aux,
    /// Unison (preamp) slots.
    Unison,
}

impl Command {
    /// The plug-in operation behind an Edit-menu "… All" command.
    pub fn plugin_op(self) -> Option<(PluginOp, PluginScope)> {
        use Command as C;
        use PluginOp as O;
        use PluginScope as S;
        Some(match self {
            C::RemoveAllPlugins => (O::Remove, S::All),
            C::RemoveAllInsertPlugins => (O::Remove, S::Inserts),
            C::RemoveAllAuxPlugins => (O::Remove, S::Aux),
            C::RemoveAllUnisonPlugins => (O::Remove, S::Unison),
            C::EnableAllPlugins => (O::Enable, S::All),
            C::EnableAllInsertPlugins => (O::Enable, S::Inserts),
            C::EnableAllAuxPlugins => (O::Enable, S::Aux),
            C::EnableAllUnisonPlugins => (O::Enable, S::Unison),
            C::DisableAllPlugins => (O::Disable, S::All),
            C::DisableAllInsertPlugins => (O::Disable, S::Inserts),
            C::DisableAllAuxPlugins => (O::Disable, S::Aux),
            C::DisableAllUnisonPlugins => (O::Disable, S::Unison),
            _ => return None,
        })
    }
}

/// What a menu row does: `Control::Menu(MenuCmd)`. View commands change `ViewState`; model
/// commands reach `MixerModel::apply` as `Action::Tap` (the strip is the id's strip).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MenuCmd {
    /// [view] Hover marker of entry `index` at menu level `level` (registered under the row's
    /// own hit); `index` = `u16::MAX` is the menu window, which swallows clicks between rows.
    Row { level: u8, index: u16 },
    /// [view] Open the submenu of entry `index` at menu level `level` (0 = root).
    OpenSub { level: u8, index: u16 },
    /// [view] A title of the in-window menu bar.
    BarTitle(u8),
    /// A menu-bar command (scope per [`Command::scope`]).
    App(Command),
    /// [view] Strip label "Rename..." → the rename editor.
    Rename,
    /// [view] Insert "Assign..." → the plug-in browser assigns to this slot (255 = Unison).
    Assign(u8),
    /// [view] Insert / strip "Presets..." and label "Channel Presets..." → the browser's
    /// PRESETS tab.
    Presets,
    /// [view] The assign-mode browser's close button: leave assign mode.
    AssignCancel,
    /// [model] Strip `SoloSafe`, `Stereo` (link, left channel) and `ChannelHidden` toggles.
    SoloSafe,
    Link,
    Hide,
    /// [model] Meter menu CLEAR: this strip's / every meter's clip and peak hold.
    ClearMeter,
    ClearAllMeters,
    /// [model] Meter menu METERING: `/PostFaderMetering`.
    PostFaderMetering(bool),
    /// [model] Paste the copied plug-in into the slot (`Paste "<name>"`).
    InsertPaste(u8),
    /// [model] This strip's inserts: Remove All / Enable All / Disable All.
    StripInserts(PluginOp),
    /// [model] S/PDIF `SRConvert` (preamp menu "Sample Rate Convertsion").
    SrConvert(bool),
    /// [model] Fader / pan "COPY MIX TO": `post /CopyToSends {"send": i}` (the monitor mix into
    /// send `i` of every input; capture fader-in / pan-in).
    CopyMixTo(u8),
    /// [model] A PLUG-IN SCENES row's menu (capture browser-row).
    Scene(SceneOp, u16),
    /// [view] A row with no action (Show In Finder): closes the menu.
    Noop,
}

/// The PLUG-IN SCENES row menu's operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SceneOp {
    Rename,
    Overwrite,
    Clear,
    Save,
    Recall,
}

impl MenuCmd {
    /// Consumed by `ViewState` (never reaches the model).
    pub fn is_view(&self) -> bool {
        match self {
            MenuCmd::Row { .. } | MenuCmd::OpenSub { .. } | MenuCmd::BarTitle(_) | MenuCmd::Rename | MenuCmd::Assign(_) | MenuCmd::Presets | MenuCmd::AssignCancel | MenuCmd::Noop => true,
            // Rename edits the row; Overwrite / Clear ask first (their OK sends the command).
            MenuCmd::Scene(SceneOp::Rename | SceneOp::Overwrite | SceneOp::Clear, _) => true,
            MenuCmd::App(c) => c.scope() != CommandScope::Model,
            _ => false,
        }
    }
}

// MARK: model state the menus show

/// Per-strip state for ticks and enable states ([`MixerModel::strip_menu_state`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripMenuState {
    pub solo_safe: bool,
    /// None: the strip has no link button (`Stereo/enabled` false).
    pub linked: Option<bool>,
    pub hidden: bool,
    /// Hi-Z jack in use (the input source menu shows a disabled "Hi-Z").
    pub hi_z: bool,
    /// S/PDIF `SRConvert`; None when the strip has none.
    pub sr_convert: Option<bool>,
}

/// Global menu state ([`MixerModel::menu_state`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuModelState {
    pub can_undo: bool,
    pub can_redo: bool,
    /// `/PostFaderMetering`; None hides the meter menu's METERING section.
    pub post_fader_metering: Option<bool>,
    /// The plug-in a slot Copy put on the clipboard (`Paste "<name>"`).
    pub clipboard: Option<String>,
    /// `/pluginscenes/0/RecalledSceneIndex` (None: -1). The scene row menu's Overwrite is off on
    /// the recalled scene (0x1811fb990).
    pub recalled_scene: Option<u16>,
}

// MARK: open-menu state

/// What an open menu belongs to.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuTarget {
    /// In-window menu-bar title `index` (into [`menu_bar`]).
    Bar(u8),
    /// Insert slot context menu (`Rack_Inserts` 0x18143be10 + slot section 0x18143c320); 255 =
    /// Unison.
    Insert { strip: Arc<str>, slot: u8 },
    /// Strip label (`Rack_ChannelStripLabel::SetupUAD2ButtonPopup` 0x181408e30).
    StripLabel { strip: Arc<str> },
    /// Solo button (0x181494a40): Clear Solos / Solo Safe.
    Solo { strip: Arc<str> },
    /// Fader meter (`Rack_Meter::HandlePopupMenu` 0x1809cadf0).
    Meter { strip: Arc<str> },
    /// Preamp / input area (`Rack_Overview_Preamp` 0x180dd7a60 → 0x180dd7bf0 / 0x180dd81a0 /
    /// 0x180dd8320 by input type).
    Preamp { strip: Arc<str> },
    /// Plug-in browser row (`Rack_PluginAssignBrowser::ShowTreeViewItemPopupMenu` 0x1812d6da0).
    BrowserItem(u16),
    /// Input fader or pan: COPY MIX TO the strip's sends (captures fader-in, pan-in, pan-stereo).
    CopyMix { strip: Arc<str> },
    /// The MIC / LINE button: SOURCE (capture preamp-mic; the same menu as a left click).
    InputSource { strip: Arc<str> },
    /// The monitor column's big meter: CLEAR Meter / All Meters (capture monitor-meter).
    MonitorMeter,
    /// A PLUG-IN SCENES row (captures browser-row, browser-hdr).
    SceneRow(u16),
    /// A Settings combo's dropdown (captures set-d-*), by field key.
    Setting(Arc<str>),
    /// The CURRENT SESSION panel's ••• button (sess-dots) and a tree row (sess-row, a file;
    /// browser-cat2, a folder), by panel row index.
    SessionDots,
    SessionRow(u16),
    /// Status bar RATE / CLOCK and the buffer size (`Rack_*Info`; sb-rate, sb-clock).
    SampleRate,
    ClockSource,
    BufferSize,
}

/// Where the root menu opens.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuAnchor {
    /// Right click / long press: top-left at the point.
    Point(f32, f32),
    /// Combo box: below the rect, at least its width.
    Below(Rect),
    /// A combo-like control's menu, below the control's hit rect at least its width (resolved
    /// when drawn): the status bar fields.
    BelowControl(ControlId),
    /// A context menu's target component: the menu's top-left at the component's bottom-left
    /// (JUCE `withTargetComponent`), above it when there is no room below. Strip labels,
    /// insert and Unison slots, solo and MIC / LINE buttons (captures lbl-*, ins-*, unison,
    /// solo-in, preamp-mic: every popup at the component's x and bottom).
    Under(Rect),
    /// Below in-window menu-bar title `i` ([`bar_titles`] from [`BAR_X`]).
    Bar(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuState {
    pub target: MenuTarget,
    pub anchor: MenuAnchor,
    /// Open submenus: entry index per level from the root.
    pub open: Vec<u16>,
    /// Highlighted row: (level, entry index).
    pub hover: Option<(u8, u16)>,
}

/// Where a pointer-anchored context menu opens relative to the click (meter, fader, pan,
/// monitor meter: every capture puts the window's top-left 20 pt left of and 10 pt below the
/// click, i.e. at the bottom-left of a 40 × 20 target area centred on it).
pub const POINTER_DX: f32 = -20.0;
pub const POINTER_DY: f32 = 10.0;

impl MenuState {
    /// A menu whose window's top-left is at (`x`, `y`).
    pub fn at(target: MenuTarget, x: f32, y: f32) -> Self {
        Self { target, anchor: MenuAnchor::Point(x, y), open: Vec::new(), hover: None }
    }

    /// A context menu for a click at (`x`, `y`) with no target component.
    pub fn pointer(target: MenuTarget, x: f32, y: f32) -> Self {
        Self::at(target, x + POINTER_DX, y + POINTER_DY)
    }

    /// A context menu under its target component `r`.
    pub fn under(target: MenuTarget, r: Rect) -> Self {
        Self { target, anchor: MenuAnchor::Under(r), open: Vec::new(), hover: None }
    }

    /// A combo menu below the control `id` (its hit rect when drawn).
    pub fn below_control(target: MenuTarget, id: ControlId) -> Self {
        Self { target, anchor: MenuAnchor::BelowControl(id), open: Vec::new(), hover: None }
    }
}

fn sid(strip: &Arc<str>, control: Control) -> ControlId {
    ControlId { strip: strip.clone(), control }
}

fn mid(strip: &Arc<str>, cmd: MenuCmd) -> ControlId {
    sid(strip, Control::Menu(cmd))
}

fn gid(cmd: MenuCmd) -> ControlId {
    ControlId { strip: "global".into(), control: Control::Menu(cmd) }
}

/// A hit that runs Console command `c` (the title bar's FEEDBACK button runs Give Feedback).
pub fn command_control(c: Command) -> ControlId {
    gid(MenuCmd::App(c))
}

fn find_strip(model: &dyn MixerModel, id: &str) -> Option<StripInfo> {
    model.inputs().into_iter().chain(model.auxes()).find(|s| &*s.id == id)
}

// MARK: menu bar

/// One menu-bar menu: title and entries.
#[derive(Clone, Debug, PartialEq)]
pub struct BarMenu {
    pub title: &'static str,
    pub menu: PopupMenu,
}

fn cmd_item(c: Command, mac: bool) -> PopupItem {
    PopupItem {
        label: c.label().to_string(),
        id: Some(gid(MenuCmd::App(c))),
        toggle: false,
        enabled: true,
        ticked: false,
        shortcut: c.shortcut().map(|s| s.text(mac)),
        command: Some(c.id()),
    }
}

/// Whether a command can run now (menu enable state).
pub fn command_enabled(c: Command, model: &dyn MixerModel, view: &ViewState) -> bool {
    use Command as C;
    let st = model.menu_state();
    match c {
        C::Undo => st.can_undo,
        C::Redo => st.can_redo,
        // Not built yet: Console's monitor page toggle, monitor correction, compact monitor
        // fader, channel visibility popover and offline-device list.
        C::ShowMonitor | C::ShowMonitorCorrection | C::ToggleCompactMonitor | C::ShowHideChannels | C::ShowOfflineDevices => false,
        C::ShowMixerNav | C::ShowBrowser => !view.compact,
        C::MixerTabLeft | C::MixerTabRight => false,
        C::ShowPluginScenes => model.status().plugin_scenes,
        _ => true,
    }
}

/// Whether a command shows a tick.
pub fn command_ticked(c: Command, view: &ViewState) -> bool {
    use Command as C;
    match c {
        C::ShowBrowser => view.show_browser && !view.compact,
        C::ShowMixerNav => view.show_navigator && !view.compact,
        C::ShowBuses => view.show_aux,
        C::ShowControlRoom => view.show_ctrl_room,
        C::ShowMonitor => true,
        C::ShowSettings => view.sheet == Some(Sheet::Settings),
        _ => false,
    }
}

/// Console's menu bar for the Apollo mixer page (`Rack_MenuBarModel_Console::getMenuBarNames`
/// 0x1816200b0: File, Edit, Navigation (mixer pages 7/8), View, Window, Help; Test and Debug are
/// build flags). `mac` picks shortcut text and moves About into the application menu.
pub fn menu_bar(model: &dyn MixerModel, view: &ViewState, mac: bool) -> Vec<BarMenu> {
    use Command as C;
    let item = |c: Command| {
        let mut i = cmd_item(c, mac);
        i.enabled = command_enabled(c, model, view);
        i.ticked = command_ticked(c, view);
        i
    };
    let mut file = PopupMenu::new();
    file.push(item(C::NewSession)).push(item(C::OpenSession)).push(item(C::SaveSession)).push(item(C::SaveSessionAs));
    if !mac {
        file.separator().push(item(C::Quit));
    }

    let mut edit = PopupMenu::new();
    edit.push(item(C::Undo)).push(item(C::Redo)).separator();
    for (title, cmds) in [
        ("Remove All", [C::RemoveAllPlugins, C::RemoveAllInsertPlugins, C::RemoveAllAuxPlugins, C::RemoveAllUnisonPlugins]),
        ("Enable All", [C::EnableAllPlugins, C::EnableAllInsertPlugins, C::EnableAllAuxPlugins, C::EnableAllUnisonPlugins]),
        ("Disable All", [C::DisableAllPlugins, C::DisableAllInsertPlugins, C::DisableAllAuxPlugins, C::DisableAllUnisonPlugins]),
    ] {
        let mut sub = PopupMenu::new();
        for c in cmds {
            sub.push(item(c));
        }
        edit.submenu(title, sub, true);
    }

    let mut nav = PopupMenu::new();
    for c in [C::MixerScrollLeft, C::MixerScrollRight, C::MixerBankLeft, C::MixerBankRight, C::MixerFarLeft, C::MixerFarRight, C::MixerTabLeft, C::MixerTabRight] {
        nav.push(item(c));
    }

    let mut section = PopupMenu::new();
    section.push(item(C::ShowBrowser)).push(item(C::ShowMixerNav));
    if model.status().plugin_scenes {
        section.push(item(C::ShowPluginScenes));
    }
    section.push(item(C::ShowBuses)).push(item(C::ShowControlRoom)).push(item(C::ShowMonitor));
    let opts = model.monitor_options();
    if opts.hp_correction.is_some() || opts.speaker_correction.is_some() {
        section.push(item(C::ShowMonitorCorrection));
    }
    section.push(item(C::ShowNotifications));
    let mut viewm = PopupMenu::new();
    viewm.submenu("Section", section, true);
    viewm.push(item(C::ToggleCompactMonitor)).separator();
    viewm.push(item(C::ShowHideChannels)).push(item(C::ShowHideFloatingWindows)).push(item(C::ShowOfflineDevices));
    viewm.separator().push(item(C::ShowSettings));

    let mut window = PopupMenu::new();
    window.push(item(C::PreviousWindow)).push(item(C::NextWindow)).push(item(C::ShowHideFloatingWindows));
    window.separator().push(item(C::ToggleFullScreen));

    let mut help = PopupMenu::new();
    help.push(item(C::Documentation)).separator().push(item(C::GiveFeedback));
    if !mac {
        help.push(item(C::About));
    }

    vec![
        BarMenu { title: "File", menu: file.finish() },
        BarMenu { title: "Edit", menu: edit.finish() },
        BarMenu { title: "Navigation", menu: nav.finish() },
        BarMenu { title: "View", menu: viewm.finish() },
        BarMenu { title: "Window", menu: window.finish() },
        BarMenu { title: "Help", menu: help.finish() },
    ]
}

/// The macOS application menu's extra items (`Rack_MenuBarModel` slot 13, 0x1816078d0): About,
/// then Show Settings. (The Keyboard Shortcuts submenu is LUNA's preset list.)
pub fn app_menu(model: &dyn MixerModel, view: &ViewState) -> PopupMenu {
    let mut m = PopupMenu::new();
    let mut about = cmd_item(Command::About, true);
    about.label = "About UAD Console".into();
    m.push(about).separator();
    let mut settings = cmd_item(Command::ShowSettings, true);
    settings.enabled = command_enabled(Command::ShowSettings, model, view);
    m.push(settings);
    m.finish()
}

// MARK: context menus

/// The menu for an open target, built from the model on every frame so ticks follow the engine.
pub fn build_menu(model: &dyn MixerModel, view: &ViewState, target: &MenuTarget, mac: bool) -> (PopupMenu, MenuLook) {
    match target {
        MenuTarget::Bar(i) => (menu_bar(model, view, mac).into_iter().nth(*i as usize).map(|b| b.menu).unwrap_or_default(), MenuLook::Base),
        MenuTarget::Insert { strip, slot } => (insert_menu(model, strip, *slot), MenuLook::Context),
        MenuTarget::StripLabel { strip } => (label_menu(model, strip), MenuLook::Context),
        MenuTarget::Solo { strip } => (solo_menu(model, strip), MenuLook::Context),
        MenuTarget::Meter { strip } => (meter_menu(model, strip), MenuLook::Context),
        MenuTarget::Preamp { strip } => (preamp_menu(model, strip), MenuLook::Context),
        MenuTarget::BrowserItem(i) => (browser_item_menu(model, *i), MenuLook::Context),
        MenuTarget::CopyMix { strip } => (copy_mix_menu(model, strip), MenuLook::Context),
        MenuTarget::InputSource { strip } => (input_source_menu(model, strip), MenuLook::Context),
        MenuTarget::MonitorMeter => (monitor_meter_menu(), MenuLook::Context),
        MenuTarget::SceneRow(i) => (scene_row_menu(model, *i), MenuLook::Context),
        MenuTarget::Setting(key) => (setting_menu(model, key), MenuLook::Base),
        MenuTarget::SessionDots => (session_dots_menu(model), MenuLook::Context),
        MenuTarget::SessionRow(i) => (session_row_menu(model, *i), MenuLook::Context),
        MenuTarget::SampleRate => (crate::surfaces::popover::sample_rate_menu(model), MenuLook::Base),
        MenuTarget::ClockSource => (crate::surfaces::popover::clock_source_menu(model), MenuLook::Base),
        MenuTarget::BufferSize => (crate::surfaces::popover::buffer_size_menu(model), MenuLook::Base),
    }
}

fn insert_state(model: &dyn MixerModel, s: &StripInfo, slot: u8) -> InsertState {
    if slot == 255 { model.unison(s) } else { model.inserts(s).get(slot as usize).cloned().unwrap_or(InsertState::Empty) }
}

/// Insert slot menu: the slot section (`Rack_Inserts` 0x18143c320), then the strip's inserts
/// section under the channel name (0x18143be10, Apollo page 8: Presets..., Remove All,
/// Disable All, Enable All).
pub fn insert_menu(model: &dyn MixerModel, strip: &Arc<str>, slot: u8) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let st = model.menu_state();
    let paste_label = |name: &str| format!("Paste \"{}\"", name.strip_prefix("UAD ").unwrap_or(name));
    match insert_state(model, &s, slot) {
        InsertState::Empty => {
            // The Unison slot reads "INSERT 1" too (capture unison).
            m.header(format!("INSERT {} (EMPTY)", if slot == 255 { 1 } else { slot as u16 + 1 }));
            m.push(PopupItem::new("Assign...", mid(strip, MenuCmd::Assign(slot))));
            match &st.clipboard {
                Some(name) => m.push(PopupItem::new(paste_label(name), mid(strip, MenuCmd::InsertPaste(slot)))),
                None => m.push(PopupItem::new("Paste", mid(strip, MenuCmd::InsertPaste(slot))).enabled(false)),
            };
        }
        InsertState::Loaded { name, power } => {
            // The catalogue name in capitals (s40: "UAD A-TYPE MULTIBAND ENHANCER").
            m.header(name.to_uppercase());
            m.push(PopupItem::new("Copy", sid(strip, Control::InsertCopy(slot))));
            match &st.clipboard {
                Some(c) => m.push(PopupItem::new(paste_label(c), mid(strip, MenuCmd::InsertPaste(slot)))),
                None => m.push(PopupItem::new("Paste", mid(strip, MenuCmd::InsertPaste(slot))).enabled(false)),
            };
            m.push(PopupItem::new("Assign...", mid(strip, MenuCmd::Assign(slot))));
            m.push(PopupItem::new("Presets...", mid(strip, MenuCmd::Presets)));
            m.push(PopupItem::new("Remove", sid(strip, Control::InsertRemove(slot))));
            m.push(PopupItem::new(if power { "Disable" } else { "Enable" }, sid(strip, Control::InsertPower(slot))));
        }
    }
    {
        // The channel name as the header (s01: "SM7B"; the Unison slot's menu too, capture
        // unison); the "… All" rows only when the strip has plug-ins (0x18143be10; s01 shows
        // Presets... alone on an empty strip).
        let loaded = model.inserts(&s).iter().any(|i| matches!(i, InsertState::Loaded { .. }));
        m.header(model.main(&s).name);
        m.push(PopupItem::new("Presets...", mid(strip, MenuCmd::Presets)));
        if loaded {
            m.push(PopupItem::new("Remove All", mid(strip, MenuCmd::StripInserts(PluginOp::Remove))));
            m.push(PopupItem::new("Disable All", mid(strip, MenuCmd::StripInserts(PluginOp::Disable))));
            m.push(PopupItem::new("Enable All", mid(strip, MenuCmd::StripInserts(PluginOp::Enable))));
        }
    }
    m.finish()
}

/// The plug-in window's ⋮ menu (`Rack_PlugInHeader` 0x180dff8f0, capture s41): INSERT with the
/// strip's loaded plug-ins (the window's own ticked; picking another opens it), then OPTIONS
/// Assign.... (PROCESSING UADx / UAD-2 and UTILITIES follow for native-capable plug-ins.)
pub fn plugin_insert_menu(model: &dyn MixerModel, strip: &Arc<str>, slot: u8) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let short = |n: &str| n.strip_prefix("UAD ").unwrap_or(n).to_string();
    if slot == 255 {
        m.header("UNISON");
        if let InsertState::Loaded { name, .. } = model.unison(&s) {
            m.push(PopupItem::new(short(&name), sid(strip, Control::Insert(255))).ticked(true));
        }
    } else {
        m.header("INSERT");
        for (k, st) in model.inserts(&s).iter().enumerate() {
            if let InsertState::Loaded { name, .. } = st {
                m.push(PopupItem::new(short(name), sid(strip, Control::Insert(k as u8))).ticked(k == slot as usize));
            }
        }
    }
    m.header("OPTIONS");
    m.push(PopupItem::new("Assign...", mid(strip, MenuCmd::Assign(slot))));
    m.finish()
}

/// Strip label (`SetupUAD2ButtonPopup` 0x181408e30): the channel name as header, Rename...,
/// Link (linkable pairs only), Hide, Isolate, Channel Presets....
pub fn label_menu(model: &dyn MixerModel, strip: &Arc<str>) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let st = model.strip_menu_state(&s);
    m.header(model.main(&s).name);
    m.push(PopupItem::new("Rename...", mid(strip, MenuCmd::Rename)));
    if let Some(linked) = st.linked {
        m.push(PopupItem::new("Link", mid(strip, MenuCmd::Link)).ticked(linked));
    }
    m.push(PopupItem::new("Hide", mid(strip, MenuCmd::Hide)).ticked(st.hidden));
    m.push(PopupItem::new("Isolate", sid(strip, Control::Isolate)).ticked(model.isolated(&s)).toggle());
    m.push(PopupItem::new("Channel Presets...", mid(strip, MenuCmd::Presets)));
    m.finish()
}

/// Solo button (0x181494a40): Clear Solos, separator, Solo Safe (ticked).
pub fn solo_menu(model: &dyn MixerModel, strip: &Arc<str>) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let st = model.strip_menu_state(&s);
    m.push(PopupItem::new("Clear Solos", sid(strip, Control::ClearSolos)));
    m.separator();
    m.push(PopupItem::new("Solo Safe", mid(strip, MenuCmd::SoloSafe)).ticked(st.solo_safe));
    m.finish()
}

/// Meter (`Rack_Meter::HandlePopupMenu` 0x1809cadf0): CLEAR Meter / All Meters, then METERING
/// Pre-Fader / Post-Fader when the mixer meters post-fader on request.
pub fn meter_menu(model: &dyn MixerModel, strip: &Arc<str>) -> PopupMenu {
    let mut m = PopupMenu::new();
    m.header("CLEAR");
    m.push(PopupItem::new("Meter", mid(strip, MenuCmd::ClearMeter)));
    m.push(PopupItem::new("All Meters", mid(strip, MenuCmd::ClearAllMeters)));
    if let Some(post) = model.menu_state().post_fader_metering {
        m.header("METERING");
        m.push(PopupItem::new("Pre-Fader", mid(strip, MenuCmd::PostFaderMetering(false))).ticked(!post));
        m.push(PopupItem::new("Post-Fader", mid(strip, MenuCmd::PostFaderMetering(true))).ticked(post));
    }
    m.finish()
}

/// Input fader / pan (captures fader-in, pan-in, pan-stereo): COPY MIX TO, then the strip's
/// sends by group (AUX 1, AUX 2 | CUE 1, CUE 2), a separator between groups.
pub fn copy_mix_menu(model: &dyn MixerModel, strip: &Arc<str>) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let sends = model.input_sends(&s);
    if sends.is_empty() {
        return m;
    }
    m.header("COPY MIX TO");
    let group = |n: &str| n.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ').to_ascii_uppercase();
    let mut last: Option<String> = None;
    for (i, send) in sends.iter().enumerate() {
        let g = group(&send.name);
        if last.as_ref().is_some_and(|l| *l != g) {
            m.separator();
        }
        last = Some(g);
        m.push(PopupItem::new(send.name.to_uppercase(), mid(strip, MenuCmd::CopyMixTo(i as u8))));
    }
    m.finish()
}

/// The MIC / LINE button (`Rack_PreampChannelStripControls::DoInputSourcePopup` 0x181476f00,
/// capture preamp-mic): SOURCE, then the sources with the current one ticked (or a disabled
/// Hi-Z while the jack is used).
pub fn input_source_menu(model: &dyn MixerModel, strip: &Arc<str>) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let Some(p) = model.preamp(&s) else { return m };
    m.header("SOURCE");
    if model.strip_menu_state(&s).hi_z {
        m.push(PopupItem { label: "Hi-Z".into(), id: None, toggle: false, enabled: false, ticked: true, shortcut: None, command: None });
    } else {
        // MIC, then LINE (preamp-mic). The engine also lists Dante (x4: Line, Mic, Dante) and the
        // jack selects Hi-Z, so neither is a choice here.
        let sources = model.input_sources(&s);
        for want in ["MIC", "LINE"] {
            if let Some((i, src)) = sources.iter().enumerate().find(|(_, s)| s.eq_ignore_ascii_case(want)) {
                m.push(PopupItem::new(want, sid(strip, Control::InputSource(i as u8))).ticked(src.eq_ignore_ascii_case(&p.source)));
            }
        }
    }
    m.finish()
}

/// A Settings combo's choices (captures set-d-*: the options, the current one ticked).
pub fn setting_menu(model: &dyn MixerModel, key: &str) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(field) = model.properties().and_then(|p| crate::settings::find_field(p, key)) else { return m };
    let crate::settings::FieldKind::Choice { options, enabled, selected } = field.kind else { return m };
    let id = |i: usize| ControlId { strip: "settings".into(), control: Control::SettingChoose(key.into(), i as u16) };
    // MIDI DEVICE: NONE, a separator, then the devices (set-d-mididev); `midi:` keys choose
    // NONE as 0 and device i as i + 1.
    let midi = key.starts_with("midi:");
    if midi {
        m.push(PopupItem::new("NONE", id(0)).ticked(selected.is_none()));
        m.separator();
    }
    for (i, o) in options.iter().enumerate() {
        m.push(PopupItem::new(o.clone(), id(i + midi as usize)).ticked(selected == Some(i)).enabled(enabled.get(i).copied().unwrap_or(true)));
    }
    m.finish()
}

/// The CURRENT SESSION panel's ••• (sess-dots): New Folder, Show In Finder.
pub fn session_dots_menu(model: &dyn MixerModel) -> PopupMenu {
    use crate::surfaces::session::{REVEAL_SESSIONS_FOLDER, SessionOp};
    let mut m = PopupMenu::new();
    m.push(PopupItem::new("New Folder", session_id(Control::Session(SessionOp::NewFolder))));
    m.push(show_in_finder(model, REVEAL_SESSIONS_FOLDER));
    m.finish()
}

/// "Show In Finder" ("Show In File Explorer" on Windows): Console reveals the file or folder
/// itself (`juce::File::revealToUser`, `Rack_PresetsView::ShowPopupMenu` case 5), so the row
/// works only when the engine's Sessions folder is on this computer.
fn show_in_finder(model: &dyn MixerModel, row: u16) -> PopupItem {
    use crate::surfaces::session::SessionOp;
    let local = model.session().local_dir.is_some();
    PopupItem::new(SHOW_IN_FINDER_LABEL, session_id(Control::Session(SessionOp::Reveal(row)))).enabled(local)
}

/// Console 1.3.1's label on macOS ("Show In File Explorer" on Windows; captures sess-row, browser-cat2).
pub const SHOW_IN_FINDER_LABEL: &str = "Show In Finder";

fn session_id(control: Control) -> ControlId {
    ControlId { strip: crate::surfaces::session::SESSION_STRIP.into(), control }
}

/// A CURRENT SESSION tree row: a file (sess-row: its name, Load '<name>' (disabled for the
/// open session), separator, Delete, Rename, Show In Finder) or a folder (browser-cat2: its
/// name, Delete, Rename (both disabled for the root "User"), New Folder, Show In Finder).
pub fn session_row_menu(model: &dyn MixerModel, row: u16) -> PopupMenu {
    use crate::surfaces::session::{PanelRowKind, SessionOp, panel_rows};
    let info = model.session();
    let rows = panel_rows(&info);
    let mut m = PopupMenu::new();
    let Some(r) = rows.get(row as usize) else { return m };
    m.header(r.label.clone());
    match &r.kind {
        PanelRowKind::File(i) => {
            let open = info.files.get(*i as usize).is_some_and(|f| f.path == info.current);
            m.push(PopupItem::new(format!("Load '{}'", r.label), session_id(Control::Session(SessionOp::Load(*i)))).enabled(!open));
            m.separator();
            m.push(PopupItem::new("Delete", session_id(Control::Session(SessionOp::Delete(*i)))));
            m.push(PopupItem::new("Rename", session_id(Control::Session(SessionOp::Rename(*i)))));
        }
        PanelRowKind::Folder { root, .. } => {
            // `Rack_PresetsView` folder ops: Delete → `DeleteFolder {folder, type: session}`,
            // Rename → `RenameSession {origName, newName}` (the engine's
            // `SessionFileManager::Rename` resolves files and directories alike), New Folder
            // inside this folder (the root's goes to the top level).
            m.push(PopupItem::new("Delete", session_id(Control::Session(SessionOp::FolderDelete(row)))).enabled(!root));
            m.push(PopupItem::new("Rename", session_id(Control::Session(SessionOp::FolderRename(row)))).enabled(!root));
            let new = if *root { SessionOp::NewFolder } else { SessionOp::NewFolderIn(row) };
            m.push(PopupItem::new("New Folder", session_id(Control::Session(new))));
        }
    }
    m.push(show_in_finder(model, row));
    m.finish()
}

/// The monitor meter (capture monitor-meter): CLEAR Meter / All Meters.
pub fn monitor_meter_menu() -> PopupMenu {
    let strip: Arc<str> = "monitor".into();
    let mut m = PopupMenu::new();
    m.header("CLEAR");
    m.push(PopupItem::new("Meter", mid(&strip, MenuCmd::ClearMeter)));
    m.push(PopupItem::new("All Meters", mid(&strip, MenuCmd::ClearAllMeters)));
    m.finish()
}

/// A scene with nothing saved in it (the engine names empty slots "EMPTY SCENE").
pub fn scene_is_empty(name: &str) -> bool {
    let n = name.trim();
    n.is_empty() || n.eq_ignore_ascii_case("EMPTY SCENE")
}

/// A PLUG-IN SCENES row (`Rack_ScenesListBoxRow::mouseDown` 0x1811fb990; captures
/// browser-row / browser-hdr). Rename, Clear and Recall Scene need a saved scene; Overwrite also
/// needs it not to be the recalled one; Save Scene is always on.
pub fn scene_row_menu(model: &dyn MixerModel, index: u16) -> PopupMenu {
    let scenes = model.plugin_scenes();
    let empty = scenes.get(index as usize).is_none_or(|n| scene_is_empty(n));
    let recalled = model.menu_state().recalled_scene == Some(index);
    let g = |op: SceneOp| gid(MenuCmd::Scene(op, index));
    let cmd = |op: SceneOp| ControlId { strip: "global".into(), control: Control::SceneCommand(op, index) };
    let mut m = PopupMenu::new();
    m.push(PopupItem::new("Rename", g(SceneOp::Rename)).enabled(!empty));
    m.push(PopupItem::new("Overwrite", g(SceneOp::Overwrite)).enabled(!empty && !recalled));
    m.push(PopupItem::new("Clear", g(SceneOp::Clear)).enabled(!empty));
    m.push(PopupItem::new("Save Scene", cmd(SceneOp::Save)));
    m.push(PopupItem::new("Recall Scene", cmd(SceneOp::Recall)).enabled(!empty));
    m.finish()
}

/// Console's tap tempo (`Rack_MIDI` 0x180cea600, `Rack_Message_SetTempo` 0x180cea060,
/// `Rack_Message_TapChanged` 0x180cea4b0): the last four taps; from the fourth on, BPM =
/// 60 / ((t₃ − t₀) / 3). A tap more than 2 s after the previous one starts over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TapTempo {
    taps: Vec<std::time::Instant>,
}

impl TapTempo {
    pub const TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

    /// Records a tap at `now`; the tempo once four taps are in.
    pub fn tap(&mut self, now: std::time::Instant) -> Option<f64> {
        if self.taps.last().is_none_or(|t| now.saturating_duration_since(*t) > Self::TIMEOUT) {
            self.taps.clear();
        }
        if self.taps.len() == 4 {
            self.taps.remove(0);
        }
        self.taps.push(now);
        let span = self.taps.last()?.saturating_duration_since(self.taps[0]).as_secs_f64();
        (self.taps.len() == 4 && span > 0.0).then(|| 60.0 / (span / 3.0))
    }
}

/// Preamp / input area (`Rack_Overview_Preamp` 0x180dd7a60): a preamp input gets Input Source
/// (MIC / LINE, or a disabled Hi-Z while the jack is used), Settings (Low Cut, Phase, +48V,
/// Pad) and Unison; S/PDIF gets "Sample Rate Convertsion" On / Off (Console's spelling).
pub fn preamp_menu(model: &dyn MixerModel, strip: &Arc<str>) -> PopupMenu {
    let mut m = PopupMenu::new();
    let Some(s) = find_strip(model, strip) else { return m };
    let st = model.strip_menu_state(&s);
    if let Some(p) = model.preamp(&s) {
        m.header("Input Source");
        if st.hi_z {
            m.push(PopupItem { label: "Hi-Z".into(), id: None, toggle: false, enabled: false, ticked: true, shortcut: None, command: None });
        } else {
            for (i, src) in model.input_sources(&s).iter().enumerate() {
                m.push(PopupItem::new(src.to_uppercase(), sid(strip, Control::InputSource(i as u8))).ticked(src.eq_ignore_ascii_case(&p.source)));
            }
        }
        m.header("Settings");
        let t = |name: &str| sid(strip, Control::Preamp(name.into()));
        m.push(PopupItem::new("Low Cut", t("low_cut")).ticked(p.low_cut).toggle());
        m.push(PopupItem::new("Phase", t("phase")).ticked(p.phase).toggle());
        m.push(PopupItem::new("+48V", t("48v")).ticked(p.phantom).enabled(p.phantom_enabled).toggle());
        m.push(PopupItem::new("Pad", t("pad")).ticked(p.pad).enabled(p.pad_enabled).toggle());
        m.header("Unison");
        match model.unison(&s) {
            InsertState::Empty => {
                m.push(PopupItem::new("Assign...", mid(strip, MenuCmd::Assign(255))));
            }
            InsertState::Loaded { power, .. } => {
                m.push(PopupItem::new("Open...", sid(strip, Control::Insert(255))));
                m.push(PopupItem::new("Assign...", mid(strip, MenuCmd::Assign(255))));
                m.push(PopupItem::new("Power", sid(strip, Control::InsertPower(255))).ticked(power));
                m.push(PopupItem::new("Remove", sid(strip, Control::InsertRemove(255))));
                m.push(PopupItem::new("Copy Settings", sid(strip, Control::InsertCopy(255))));
                m.push(PopupItem::new("Paste Settings", mid(strip, MenuCmd::InsertPaste(255))).enabled(model.menu_state().clipboard.is_some()));
            }
        }
    } else if let Some(on) = st.sr_convert {
        m.header("Sample Rate Convertsion");
        m.push(PopupItem::new("On", mid(strip, MenuCmd::SrConvert(true))).ticked(on));
        m.push(PopupItem::new("Off", mid(strip, MenuCmd::SrConvert(false))).ticked(!on));
    }
    m.finish()
}

/// Plug-in browser row (0x1812d6da0): the plug-in name, Assign (into the selected slot, or
/// arms assign mode), separator, Manage Plug-Ins.... ("Show in Explorer" is for file-based
/// plug-ins.)
pub fn browser_item_menu(model: &dyn MixerModel, index: u16) -> PopupMenu {
    let mut m = PopupMenu::new();
    let cat = model.plugin_catalog();
    let Some(p) = cat.get(index as usize) else { return m };
    m.header(p.display_name().to_string());
    m.push(PopupItem::new("Assign", ControlId { strip: "browser".into(), control: Control::BrowserItem(index) }));
    m.finish()
}

// MARK: layout

/// A laid-out menu window.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuFrame {
    pub frame: Rect,
    /// Each entry's row rect, by entry index.
    pub rows: Vec<Rect>,
}

/// Popup item font (`views:Popup-Menu-Item`, Mac 1.3.1: Helvetica 11 bold, dyn).
fn item_style(ui: &Ui) -> crate::draw::TextStyle {
    let mut s = ui.style("views", "Popup-Menu-Item");
    if ui.skin("views", "Popup-Menu-Item").is_none() {
        s = Ui::plain_style("Helvetica", 12.0, true, Rgba::BLACK);
    }
    s.color = Rgba::BLACK;
    s
}

fn header_style(item: &crate::draw::TextStyle) -> crate::draw::TextStyle {
    crate::draw::TextStyle { bold: true, size: item.size * HEADER_FONT_SCALE, color: HEADER_TEXT, ..item.clone() }
}

fn text_w(style: &crate::draw::TextStyle, text: &str) -> f32 {
    if style.bold { helvetica_bold_width(text, style.size) } else { helvetica_width(text, style.size) }
}

/// Entry widths and heights per [`MenuLook`].
fn measure(style: &crate::draw::TextStyle, look: MenuLook, e: &PopupEntry) -> (f32, f32) {
    let font_h = style.size * 1.15;
    let item_w = |t: &str| match look {
        // JUCE `getIdealPopupMenuItemSize`: the text plus twice the item height (set-d-ctlmode,
        // set-d-iomode, set-d-nin, set-d-tapev: every text-wide combo menu).
        MenuLook::Base => text_w(style, t) + 2.0 * ITEM_H,
        MenuLook::Plugin => text_w(style, t) + 19.0 + 2.0 * font_h,
        MenuLook::Context => text_w(style, t) + 2.0 * ITEM_H,
    };
    match e {
        PopupEntry::Separator => (0.0, SEPARATOR_H),
        PopupEntry::Header(t) => (text_w(&header_style(style), t) + 2.0 * ITEM_H, HEADER_H),
        PopupEntry::Item(i) => {
            let extra = i.shortcut.as_deref().map(|s| text_w(style, s) + 3.0 * font_h).unwrap_or(0.0);
            (item_w(&i.label) + extra, ITEM_H)
        }
        PopupEntry::Submenu { label, .. } => (item_w(label) + if look == MenuLook::Plugin { 6.0 } else { 12.0 }, ITEM_H),
    }
}

/// Lays out `menu` at `origin` (its window's top-left), at least `min_w` wide, kept inside
/// `bounds` (flipped left / up like JUCE when it would overflow).
pub fn layout_menu(ui: &Ui, menu: &PopupMenu, look: MenuLook, origin: (f32, f32), min_w: f32, bounds: Rect) -> MenuFrame {
    let mut style = item_style(ui);
    style.size *= look.style().font_scale;
    let mut w: f32 = min_w - 2.0 * BORDER;
    let mut h = 0.0;
    let mut sizes = Vec::with_capacity(menu.entries.len());
    for e in &menu.entries {
        let (ew, eh) = measure(&style, look, e);
        w = w.max(ew.ceil());
        sizes.push(eh);
        h += eh;
    }
    let fw = w + 2.0 * BORDER;
    let fh = h + 2.0 * BORDER;
    let mut x = origin.0;
    let mut y = origin.1;
    if x + fw > bounds.max_x() {
        x = (bounds.max_x() - fw).max(bounds.x);
    }
    if y + fh > bounds.max_y() {
        y = (bounds.max_y() - fh).max(bounds.y);
    }
    let frame = Rect::new(x, y, fw, fh);
    let mut rows = Vec::with_capacity(sizes.len());
    let mut ry = y + BORDER;
    for eh in sizes {
        rows.push(Rect::new(x + BORDER, ry, w, eh));
        ry += eh;
    }
    MenuFrame { frame, rows }
}

/// Where a submenu of the row `row` (in a window `parent`) opens: to the right, flipped left
/// when it does not fit.
fn submenu_origin(parent: Rect, row: Rect, size_w: f32, bounds: Rect) -> (f32, f32) {
    let x = if parent.max_x() + size_w <= bounds.max_x() { parent.max_x() - BORDER } else { parent.x - size_w + BORDER };
    (x, row.y - BORDER)
}

// MARK: drawing

impl Ui<'_> {
    /// `r` filled with stepped corners: `corner[k]` is how far half-point row `k` from the top
    /// (and from the bottom) starts in from each side.
    fn stepped_fill(&mut self, r: Rect, corner: &[f32], color: Rgba) {
        let band = corner.len() as f32 * 0.5;
        if band * 2.0 >= r.h {
            self.fill(r, color);
            return;
        }
        for (k, inset) in corner.iter().enumerate() {
            let dy = k as f32 * 0.5;
            self.fill(Rect::new(r.x + inset, r.y + dy, r.w - 2.0 * inset, 0.5), color);
            self.fill(Rect::new(r.x + inset, r.max_y() - dy - 0.5, r.w - 2.0 * inset, 0.5), color);
        }
        self.fill(Rect::new(r.x, r.y + band, r.w, r.h - 2.0 * band), color);
    }

    /// One menu window: background, rows, hits. `hover` / `open` are entry indices of this
    /// level; submenu rows get `MenuCmd::OpenSub { level, index }` hits.
    pub fn popup_menu(&mut self, menu: &PopupMenu, look: MenuLook, f: &MenuFrame, level: u8, hover: Option<u16>, open: Option<u16>) {
        let r = f.frame;
        let st = look.style();
        // `Popup-Menu-Background` (dropdown_bg.png) as Console 1.3.1 shows it, measured: the art's
        // 9-slice scaling leaves a 1-pt (156,161,165) border and a light inner line.
        if let Some(o) = st.outline {
            self.fill(r.inset(-0.5, -0.5, -0.5, -0.5), o);
        }
        let corner: &[f32] = if st.rounded { &BORDER_CORNER } else { &[] };
        self.stepped_fill(r, corner, st.border);
        let light_corner: &[f32] = if st.rounded { &LIGHT_CORNER } else { &[] };
        self.stepped_fill(r.inset(1.0, 1.0, 1.0, 1.0), light_corner, st.light);
        let b = 1.0 + st.light_w;
        self.fill(r.inset(b, b, b, b), st.body);
        // The window swallows clicks between rows.
        self.hit(r, row_id(level, u16::MAX), HitKind::Tap);
        let mut style = item_style(self);
        style.color = st.text;
        style.size *= st.font_scale;
        let lit_rect = |row: &Rect| Rect::new(row.x, row.y + st.highlight_inset, row.w, row.h - 2.0 * st.highlight_inset);
        for (i, (e, row)) in menu.entries.iter().zip(&f.rows).enumerate() {
            let i = i as u16;
            let lit = hover == Some(i) || open == Some(i);
            self.hit(*row, row_id(level, i), HitKind::Tap);
            match e {
                PopupEntry::Separator => match look {
                    MenuLook::Plugin => self.fill(Rect::new(row.x + 3.0, row.y, row.w - 6.0, 1.0), Rgba::rgb8(118, 125, 129)),
                    // pan-in / solo-in: a 0.5-pt dark line over a 1-pt light one, across the
                    // light line from 1 pt inside the border on each side.
                    _ => {
                        self.fill(Rect::new(row.x - 1.0, row.y, row.w + 2.0, 0.5), SEP_DARK);
                        self.fill(Rect::new(row.x - 1.0, row.y + 0.5, row.w + 2.0, 1.0), SEP_LIGHT);
                    }
                },
                PopupEntry::Header(t) => {
                    let mut hs = header_style(&style);
                    match look {
                        MenuLook::Context | MenuLook::Plugin => self.fill(*row, HEADER_BG),
                        MenuLook::Base => hs.color = Rgba::BLACK,
                    }
                    self.text(t, hs, Rect::new(row.x + 5.0, row.y + st.text_dy, row.w - 10.0, row.h), Align::Left);
                }
                PopupEntry::Item(it) => {
                    if lit && it.enabled && it.id.is_some() {
                        self.fill(lit_rect(row), st.highlight);
                    }
                    let mut ts = style.clone();
                    if !it.enabled {
                        ts.color = ts.color.with_alpha(DISABLED_ALPHA);
                    }
                    if it.ticked && st.check {
                        // s42: a ✓ 9 × 12 pt at 7 pt in.
                        let check = crate::draw::TextStyle { size: 15.0, bold: true, ..ts.clone() };
                        self.text("✓", check, Rect::new(row.x + 6.0, row.y, 12.0, row.h), Align::Left);
                    } else if it.ticked {
                        let dot = Rect::new(row.x + BULLET_X, row.y + (row.h - BULLET) / 2.0, BULLET, BULLET);
                        if self.assets.images.get("misc/dropdown_in_use_dot.png").is_some() {
                            self.image("misc/dropdown_in_use_dot.png", dot, None, None, if it.enabled { 1.0 } else { DISABLED_ALPHA });
                        } else {
                            self.fill(dot, ts.color);
                        }
                    }
                    self.text(&it.label, ts.clone(), Rect::new(row.x + st.text_x, row.y + st.text_dy, row.w - st.text_x - TEXT_MARGIN / 2.0, row.h), Align::Left);
                    if let Some(k) = &it.shortcut {
                        self.text(k, ts, Rect::new(row.x, row.y + st.text_dy, row.w - 8.0, row.h), Align::Right);
                    }
                    if let (true, Some(id)) = (it.enabled, &it.id) {
                        self.hit(*row, id.clone(), if it.toggle { HitKind::Toggle } else { HitKind::Tap });
                    }
                }
                PopupEntry::Submenu { label, enabled, .. } => {
                    if lit && *enabled {
                        self.fill(lit_rect(row), st.highlight);
                    }
                    let mut ts = style.clone();
                    if !*enabled {
                        ts.color = ts.color.with_alpha(DISABLED_ALPHA);
                    }
                    self.text(label, ts.clone(), Rect::new(row.x + st.text_x, row.y + st.text_dy, row.w - st.text_x - TEXT_MARGIN / 2.0, row.h), Align::Left);
                    let chevron = Rect::new(row.max_x() - 4.0 - 8.0, row.y + (row.h - 11.0) / 2.0, 8.0, 11.0);
                    if look == MenuLook::Plugin {
                        // s42: a white 4 × 6 pt chevron 5 pt from the right.
                        self.text("›", ts, Rect::new(row.max_x() - 5.0 - 6.0, row.y, 6.0, row.h), Align::Right);
                    } else if self.assets.images.get("misc/dropdown_submenu_chevron.png").is_some() {
                        self.image("misc/dropdown_submenu_chevron.png", chevron, None, None, if *enabled { 1.0 } else { DISABLED_ALPHA });
                    } else {
                        self.text("›", ts, chevron, Align::Center);
                    }
                    if *enabled {
                        self.hit(*row, ControlId { strip: "menu".into(), control: Control::Menu(MenuCmd::OpenSub { level, index: i }) }, HitKind::Tap);
                    }
                }
            }
        }
    }
}

/// Left edge of the in-window menu bar.
pub const BAR_X: f32 = 8.0;

/// Menu-bar title rects: the in-window bar (Windows Console's title bar, `Menu-Bar-Item-Win`),
/// laid left to right from `x` in a bar of height `h`.
pub fn bar_titles(titles: &[&str], x: f32, h: f32) -> Vec<Rect> {
    let mut out = Vec::with_capacity(titles.len());
    let mut x = x;
    for t in titles {
        let w = (helvetica_bold_width(t, 12.0) + 16.0).ceil();
        out.push(Rect::new(x, 1.0, w, h - 2.0));
        x += w;
    }
    out
}

/// The in-window menu bar (when `ViewState::menu_bar`): titles from `x`; returns its right edge.
pub fn draw_menu_bar(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, x: f32, h: f32) -> f32 {
    let bar = menu_bar(model, view, false);
    let titles: Vec<&str> = bar.iter().map(|b| b.title).collect();
    let rects = bar_titles(&titles, x, h);
    let open = match &view.menu {
        Some(MenuState { target: MenuTarget::Bar(i), .. }) => Some(*i as usize),
        _ => None,
    };
    let mut style = ui.style("views", "Menu-Bar-Item-Win");
    if ui.skin("views", "Menu-Bar-Item-Win").is_none() {
        style = Ui::plain_style("Helvetica", 12.0, true, Rgba::WHITE);
    }
    let off = ui.skin("views", "Menu-Bar-Item-Win").and_then(|s| s.color("color-text-off")).unwrap_or(Rgba::rgb8(229, 246, 254));
    let on = ui.skin("views", "Menu-Bar-Item-Win").and_then(|s| s.color("color-text-on")).unwrap_or(Rgba::WHITE);
    for (i, (t, r)) in titles.iter().zip(&rects).enumerate() {
        let lit = open == Some(i);
        if lit {
            ui.fill(*r, BAR_HOVER);
        }
        let s = crate::draw::TextStyle { color: if lit { on } else { off }, ..style.clone() };
        ui.text(t, s, *r, Align::Center);
        ui.hit(*r, ControlId { strip: "menubar".into(), control: Control::Menu(MenuCmd::BarTitle(i as u8)) }, HitKind::Tap);
    }
    rects.last().map(|r| r.max_x()).unwrap_or(x)
}

/// Draws the open menu (root and submenus) on top of the frame, over a dismiss catcher.
pub fn draw_menu(ui: &mut Ui, model: &dyn MixerModel, view: &ViewState, bounds: Rect) {
    let Some(state) = &view.menu else { return };
    let (menu, look) = build_menu(model, view, &state.target, false);
    if menu.is_empty() {
        return;
    }
    ui.popover_catcher(bounds);
    if let MenuTarget::Bar(_) = state.target {
        // The titles stay live above the catcher: a click on another one switches menus.
        let titles: Vec<&str> = menu_bar(model, view, false).iter().map(|b| b.title).collect();
        for (i, r) in bar_titles(&titles, BAR_X, crate::geometry::window::CONTENT_TOP).into_iter().enumerate() {
            ui.hit(r, ControlId { strip: "menubar".into(), control: Control::Menu(MenuCmd::BarTitle(i as u8)) }, HitKind::Tap);
        }
    }
    let (origin, min_w) = match state.anchor.clone() {
        MenuAnchor::Point(x, y) => ((x, y), 0.0),
        MenuAnchor::Below(r) => ((r.x, r.max_y()), r.w),
        MenuAnchor::BelowControl(id) => {
            let r = ui.hit_rect(&id).unwrap_or_else(|| Rect::new(bounds.x, bounds.max_y(), 0.0, 0.0));
            if &*id.strip == crate::surfaces::status::STATUS_ID {
                crate::surfaces::popover::status_field_open(ui, r);
            }
            let probe = layout_menu(ui, &menu, look, (r.x, r.max_y()), r.w, Rect::new(bounds.x, bounds.y, bounds.w, f32::MAX));
            let y = if r.max_y() + probe.frame.h <= bounds.max_y() { r.max_y() } else { (r.y - probe.frame.h).max(bounds.y) };
            ((r.x, y), r.w)
        }
        MenuAnchor::Under(r) => {
            let probe = layout_menu(ui, &menu, look, (r.x, r.max_y()), 0.0, Rect::new(bounds.x, bounds.y, bounds.w, f32::MAX));
            let y = if r.max_y() + probe.frame.h <= bounds.max_y() { r.max_y() } else { (r.y - probe.frame.h).max(bounds.y) };
            ((r.x, y), 0.0)
        }
        MenuAnchor::Bar(i) => {
            let titles: Vec<&str> = menu_bar(model, view, false).iter().map(|b| b.title).collect();
            let r = bar_titles(&titles, BAR_X, crate::geometry::window::CONTENT_TOP).get(i as usize).copied().unwrap_or_default();
            ((r.x, r.max_y()), 0.0)
        }
    };
    let mut frame = layout_menu(ui, &menu, look, origin, min_w, bounds);
    let mut level_menu = &menu;
    let mut level = 0u8;
    loop {
        let open = state.open.get(level as usize).copied();
        let hover = state.hover.filter(|(l, _)| *l == level).map(|(_, i)| i);
        // Each level is its own JUCE window (`crate::layers`).
        ui.begin_layer(format!("menu:{level}"), crate::layers::LayerKind::Menu, frame.frame, 0.0);
        ui.popup_menu(level_menu, look, &frame, level, hover, open);
        ui.end_layer();
        let Some(i) = open else { break };
        let (Some(PopupEntry::Submenu { menu: sub, .. }), Some(row)) = (level_menu.entries.get(i as usize), frame.rows.get(i as usize)) else { break };
        let probe = layout_menu(ui, sub, look, (0.0, 0.0), 0.0, bounds);
        let o = submenu_origin(frame.frame, *row, probe.frame.w, bounds);
        frame = layout_menu(ui, sub, look, o, 0.0, bounds);
        level_menu = sub;
        level += 1;
    }
}

// MARK: ViewState integration

impl ViewState {
    /// Opens a context menu for the control at (`x`, `y`) (right click / long press). True when
    /// a menu opened.
    /// A combo-like field's menu (status bar RATE / CLOCK, buffer size, Settings combos) below
    /// the field. A Settings combo (JUCE `ComboBox`) opens with its current item highlighted
    /// (every set-d-* capture); the status-bar menus open without (sb-rate, sb-clock).
    pub fn open_field_menu(&mut self, target: MenuTarget, id: ControlId, model: &dyn MixerModel) {
        self.popover = None;
        let mut state = MenuState::below_control(target, id);
        if let MenuTarget::Setting(key) = &state.target {
            let menu = setting_menu(model, key);
            state.hover = menu.entries.iter().position(|e| matches!(e, PopupEntry::Item(i) if i.ticked)).map(|i| (0, i as u16));
        }
        self.menu = Some(state);
    }

    pub fn open_context_menu(&mut self, hits: &[Hit], x: f32, y: f32) -> bool {
        let Some(state) = context_menu_at(hits, x, y) else { return false };
        self.popover = None;
        self.menu = Some(state);
        true
    }

    /// Pointer motion while a menu is open: highlight the row under it, open submenus on hover,
    /// switch menu-bar menus. True when something changed.
    pub fn menu_hover(&mut self, hits: &[Hit], x: f32, y: f32) -> bool {
        let Some(before) = self.menu.clone() else { return false };
        let mut state = before.clone();
        state.hover = None;
        let bar = hits.iter().rev().find(|h| h.rect.contains(x, y) && matches!(h.id.control, Control::Menu(MenuCmd::BarTitle(_))));
        if let (Some(h), MenuTarget::Bar(open)) = (bar, &before.target) {
            if let Control::Menu(MenuCmd::BarTitle(i)) = h.id.control {
                if i != *open {
                    state = self.bar_menu_state(i);
                }
            }
        } else if let Some((level, index, sub)) = row_under(hits, x, y) {
            if index != u16::MAX {
                state.open.truncate(level as usize);
                if sub {
                    state.open.push(index);
                }
                state.hover = Some((level, index));
            }
        }
        let changed = state != before;
        self.menu = Some(state);
        changed
    }

    fn bar_menu_state(&self, i: u8) -> MenuState {
        MenuState { target: MenuTarget::Bar(i), anchor: MenuAnchor::Bar(i), open: Vec::new(), hover: None }
    }

    /// Handles a press on a menu control (`Control::Menu`) that `ViewState` owns.
    pub(crate) fn press_menu(&mut self, id: &ControlId, cmd: &MenuCmd, model: &dyn MixerModel) {
        match cmd {
            MenuCmd::Row { .. } => {} // a header, separator, disabled row or the window
            MenuCmd::OpenSub { level, index } => {
                if let Some(m) = &mut self.menu {
                    m.open.truncate(*level as usize);
                    m.open.push(*index);
                    m.hover = Some((*level, *index));
                }
            }
            MenuCmd::BarTitle(i) => {
                if matches!(self.menu, Some(MenuState { target: MenuTarget::Bar(j), .. }) if j == *i) {
                    self.menu = None;
                } else {
                    self.popover = None;
                    self.menu = Some(self.bar_menu_state(*i));
                }
            }
            MenuCmd::App(c) => self.run_command(*c, model),
            MenuCmd::Rename => {
                let name = find_strip(model, &id.strip).map(|s| model.main(&s).name).unwrap_or_default();
                self.text_edit = Some(TextEdit { target: TextTarget::Rename(id.strip.clone()), text: name });
                self.popover = Some(PopoverState { kind: Popover::Rename { strip: id.strip.clone() }, anchor: ControlId { strip: id.strip.clone(), control: Control::Name } });
            }
            MenuCmd::Assign(slot) => {
                self.show_browser = true;
                self.compact = false;
                self.browser.tab = crate::surfaces::BrowserTab::Plugins;
                self.browser.assign_to = Some(ControlId { strip: id.strip.clone(), control: Control::Insert(*slot) });
            }
            MenuCmd::Presets => {
                self.show_browser = true;
                self.compact = false;
                self.browser.assign_to = None;
                self.browser.tab = crate::surfaces::BrowserTab::Presets;
            }
            MenuCmd::AssignCancel => self.browser.assign_to = None,
            MenuCmd::Scene(op, i) => {
                let name = model.plugin_scenes().get(*i as usize).cloned().unwrap_or_default();
                let anchor = ControlId { strip: "settings".into(), control: Control::SceneSelect(*i) };
                match op {
                    SceneOp::Rename => {
                        self.settings.scene = Some(*i);
                        self.text_edit = Some(TextEdit { target: TextTarget::SceneName(*i), text: name });
                    }
                    SceneOp::Overwrite | SceneOp::Clear => {
                        let (title, verb) = if *op == SceneOp::Overwrite { ("Overwrite Scene", "overwrite") } else { ("Clear Scene", "clear") };
                        let ok = ControlId { strip: "global".into(), control: Control::SceneCommand(*op, *i) };
                        let message = format!("Are you sure you want to {verb} \"{name}\"?");
                        self.popover = Some(PopoverState { kind: Popover::Confirm { title: title.into(), message, ok, ok_label: "OK".into() }, anchor });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Runs a view- or shell-scope command; model commands are left to `route`.
    pub fn run_command(&mut self, c: Command, model: &dyn MixerModel) {
        use Command as C;
        match c {
            C::ShowBrowser => self.show_browser = !self.show_browser,
            C::ShowMixerNav => self.show_navigator = !self.show_navigator,
            C::ShowPluginScenes => self.toggle_scenes(model),
            C::ShowBuses => self.show_aux ^= true,
            C::ShowControlRoom => self.show_ctrl_room ^= true,
            C::ShowSettings => self.sheet = if self.sheet == Some(Sheet::Settings) { None } else { Some(Sheet::Settings) },
            C::MixerScrollLeft => self.first_strip = self.first_strip.saturating_sub(1),
            C::MixerScrollRight => self.first_strip = (self.first_strip + 1).min(model.inputs().len().saturating_sub(1)),
            C::MixerBankLeft | C::MixerBankRight => {
                let bank = self.visible_strips(model).max(1);
                self.first_strip = if c == C::MixerBankLeft { self.first_strip.saturating_sub(bank) } else { (self.first_strip + bank).min(model.inputs().len().saturating_sub(1)) };
            }
            C::Documentation => self.open_urls.push(DOCUMENTATION_URL.into()),
            C::GiveFeedback => self.open_urls.push(FEEDBACK_URL.into()),
            C::About => {
                self.open_named("about");
            }
            C::MixerFarLeft => self.first_strip = 0,
            C::MixerFarRight => self.first_strip = model.inputs().len().saturating_sub(1),
            // File > New / Open... / Save / Save As...: the session layer (save-changes question,
            // name and path fields), not the shell.
            C::NewSession | C::OpenSession | C::SaveSession | C::SaveSessionAs => {
                use crate::surfaces::SessionOp as S;
                let op = match c {
                    C::NewSession => S::New,
                    C::OpenSession => S::Open,
                    C::SaveSession => S::Save,
                    _ => S::SaveAs,
                };
                let fx = self.route_session(op, model);
                self.deferred.extend(fx);
            }
            _ if c.scope() == CommandScope::Shell => self.commands.push(c),
            _ => {}
        }
    }

    fn visible_strips(&self, model: &dyn MixerModel) -> usize {
        crate::window::WindowLayout::new(self, model.inputs().len(), model.auxes().len()).visible_inputs()
    }

    /// Runs a Console command by id (native menus, keyboard). Model commands go to `model`.
    /// False for an unknown or disabled id.
    pub fn run_command_id(&mut self, id: &str, model: &mut dyn MixerModel) -> bool {
        let Some(c) = Command::from_id(id) else { return false };
        if !command_enabled(c, model, self) {
            return false;
        }
        self.menu = None;
        if c.scope() == CommandScope::Model {
            model.apply(&crate::input::Action::Tap(gid(MenuCmd::App(c))));
        } else {
            self.run_command(c, model);
            for e in std::mem::take(&mut self.deferred) {
                match e {
                    crate::surfaces::Effect::Model(a) => model.apply(&a),
                    crate::surfaces::Effect::SetText(id, text) => model.set_text(&id, &text),
                }
            }
        }
        true
    }

    /// Shell commands requested since the last call (quit, full screen, URLs, sessions…), oldest
    /// first.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }
}

impl ViewState {
    /// Opens a menu from a text spec (CLI `--menu`, tests): `bar:N`, `insert:STRIP:SLOT`,
    /// `label:STRIP`, `solo:STRIP`, `meter:STRIP`, `preamp:STRIP` or `browser:N`, optionally
    /// followed by `>I>J…` to open submenu entries. Context menus open at the control's centre
    /// in `hits` (a frame built without the menu).
    pub fn open_menu_spec(&mut self, hits: &[Hit], spec: &str, model: &dyn MixerModel) -> Result<(), String> {
        // `~N` highlights row N of the innermost menu (renders of a hovered row).
        let (spec, hover) = match spec.split_once('~') {
            Some((s, h)) => (s, Some(h.parse::<u16>().map_err(|_| format!("--menu {spec}: bad row {h}"))?)),
            None => (spec, None),
        };
        // `setting=KEY`: a Settings combo's dropdown (the window open with `--open settings:TAB`).
        if let Some(key) = spec.strip_prefix("setting=") {
            let key: Arc<str> = key.into();
            self.open_field_menu(MenuTarget::Setting(key.clone()), ControlId { strip: "settings".into(), control: Control::SettingMenu(key) }, model);
            if let (Some(m), Some(i)) = (&mut self.menu, hover) {
                m.hover = Some((0, i));
            }
            return Ok(());
        }
        let status = |target: MenuTarget, control: Control| MenuState::below_control(target, ControlId { strip: "status".into(), control });
        let quick = match spec {
            "rate" => Some(status(MenuTarget::SampleRate, Control::StatusRate)),
            "clock" => Some(status(MenuTarget::ClockSource, Control::StatusClock)),
            _ => None,
        };
        if let Some(mut m) = quick {
            m.hover = hover.map(|i| (0, i));
            self.popover = None;
            self.menu = Some(m);
            return Ok(());
        }
        let mut parts = spec.split('>');
        let head = parts.next().unwrap_or_default();
        // `@X,Y`: the click point (pointer-anchored menus open relative to it).
        let (head, at) = match head.split_once('@') {
            Some((h, p)) => {
                let (px, py) = p.split_once(',').ok_or_else(|| format!("--menu {spec}: @X,Y"))?;
                let n = |v: &str| v.parse::<f32>().map_err(|_| format!("--menu {spec}: bad point"));
                (h, Some((n(px)?, n(py)?)))
            }
            None => (head, None),
        };
        let fields: Vec<&str> = head.split(':').collect();
        let rect = |strip: Option<&str>, f: &dyn Fn(&Control) -> bool| {
            hits.iter().rev().find(|h| strip.is_none_or(|s| &*h.id.strip == s) && f(&h.id.control)).map(|h| h.rect).ok_or_else(|| format!("--menu {spec}: no such control on screen"))
        };
        // The click point for each kind of menu (the control's centre, or `@X,Y`).
        let point: (f32, f32) = match fields.as_slice() {
            ["bar", n] => {
                let i: u8 = n.parse().map_err(|_| format!("--menu {spec}: bad menu index"))?;
                self.menu = Some(self.bar_menu_state(i));
                (f32::NAN, f32::NAN)
            }
            ["browser", n] => {
                let i: u16 = n.parse().map_err(|_| format!("--menu {spec}: bad row"))?;
                rect(None, &|c| *c == Control::BrowserItem(i))?.center()
            }
            ["scene", n] => {
                let i: u16 = n.parse().map_err(|_| format!("--menu {spec}: bad row"))?;
                rect(None, &|c| *c == Control::SceneSelect(i))?.center()
            }
            ["monitor-meter"] => rect(None, &|c| *c == Control::ShowPanel(crate::settings::Panel::MeterWindow))?.center(),
            ["session-row", n] => {
                let i: u16 = n.parse().map_err(|_| format!("--menu {spec}: bad row"))?;
                rect(None, &|c| *c == Control::SessionPanelRow(i))?.center()
            }
            ["session-dots"] => {
                rect(None, &|c| *c == Control::SessionPanelDots)?;
                let id = ControlId { strip: crate::surfaces::session::SESSION_STRIP.into(), control: Control::SessionPanelDots };
                self.menu = Some(MenuState::below_control(MenuTarget::SessionDots, id));
                (f32::NAN, f32::NAN)
            }
            [kind, strip, rest @ ..] => {
                let s = Some(*strip);
                match (*kind, rest) {
                    ("insert", [slot]) => {
                        let slot: u8 = if slot.eq_ignore_ascii_case("unison") { 255 } else { slot.parse().map_err(|_| format!("--menu {spec}: bad slot"))? };
                        rect(s, &|c| *c == Control::Insert(slot))?.center()
                    }
                    ("label", []) => rect(s, &|c| *c == Control::Name)?.center(),
                    ("solo", []) => rect(s, &|c| *c == Control::Solo)?.center(),
                    ("fader", []) => rect(s, &|c| *c == Control::Fader)?.center(),
                    ("pan", []) => rect(s, &|c| matches!(c, Control::PanL | Control::PanR))?.center(),
                    ("meter", []) => {
                        let f = rect(s, &|c| *c == Control::Fader)?;
                        (f.max_x() + METER_W / 2.0, f.y + f.h / 2.0)
                    }
                    ("preamp" | "source", []) => rect(s, &|c| matches!(c, Control::Preamp(k) if &**k == "input_source"))?.center(),
                    _ => return Err(format!("--menu {spec}: unknown menu")),
                }
            }
            _ => return Err(format!("--menu {spec}: unknown menu")),
        };
        if !point.0.is_nan() {
            let (x, y) = at.unwrap_or(point);
            self.popover = None;
            self.menu = Some(context_menu_at(hits, x, y).ok_or_else(|| format!("--menu {spec}: no menu at {x},{y}"))?);
        }
        if let Some(m) = &mut self.menu {
            for p in parts {
                m.open.push(p.parse().map_err(|_| format!("--menu {spec}: bad submenu index {p}"))?);
            }
            m.hover = hover.map(|i| (m.open.len() as u8, i));
        }
        Ok(())
    }
}

/// A PLUG-IN SCENES row's menu opens at this offset from the row's top-left (captures
/// browser-row and browser-hdr: x 143 in the 288-pt column, 23.5 pt below the 32-pt row's top,
/// wherever the click was).
const SCENE_MENU_DX: f32 = 143.0;
const SCENE_MENU_DY: f32 = 23.5;

/// The context menu Console opens for a secondary click at (`x`, `y`), from the frame's hits
/// (captures lbl-*, ins-*, unison, solo-*, meter-*, preamp-*, pan-*, fader-*, monitor-*). Aux
/// and talkback strips (no solo button) have no label or fader menu; the gain knob and gain
/// field have none either.
pub fn context_menu_at(hits: &[Hit], x: f32, y: f32) -> Option<MenuState> {
    let top = hits.iter().rev().find(|h| h.rect.contains(x, y));
    let channel = |strip: &Arc<str>| hits.iter().any(|h| h.id.strip == *strip && h.id.control == Control::Solo);
    if let Some(hit) = top {
        let strip = hit.id.strip.clone();
        let state = match &hit.id.control {
            Control::Insert(slot) => Some(MenuState::under(MenuTarget::Insert { strip, slot: *slot }, hit.rect)),
            Control::Name if channel(&strip) => Some(MenuState::under(MenuTarget::StripLabel { strip }, hit.rect)),
            Control::Solo => Some(MenuState::under(MenuTarget::Solo { strip }, hit.rect)),
            Control::Preamp(k) if &**k == "input_source" => Some(MenuState::under(MenuTarget::InputSource { strip }, hit.rect)),
            Control::InputSource(_) => Some(MenuState::under(MenuTarget::InputSource { strip }, hit.rect)),
            Control::Fader | Control::PanL | Control::PanR if channel(&strip) => Some(MenuState::pointer(MenuTarget::CopyMix { strip }, x, y)),
            Control::ShowPanel(crate::settings::Panel::MeterWindow) => Some(MenuState::pointer(MenuTarget::MonitorMeter, x, y)),
            Control::SceneSelect(i) => Some(MenuState::at(MenuTarget::SceneRow(*i), hit.rect.x + SCENE_MENU_DX, hit.rect.y + SCENE_MENU_DY)),
            Control::SessionPanelRow(i) => {
                // Under the row from x 33 in the column (the row's hit starts 5 pt in).
                let dx = crate::surfaces::session::ROW_MENU_X - 5.0;
                Some(MenuState::under(MenuTarget::SessionRow(*i), Rect::new(hit.rect.x + dx, hit.rect.y, hit.rect.w - dx, hit.rect.h)))
            }
            Control::BrowserItem(i) => Some(MenuState::pointer(MenuTarget::BrowserItem(*i), x, y)),
            _ => None,
        };
        if state.is_some() {
            return state;
        }
    }
    // The fader meter (`Rack_Meter`) has no hit of its own: it is the strip's column right of
    // the fader, over the fader's height.
    let meter = hits.iter().find(|h| h.id.control == Control::Fader && y >= h.rect.y && y <= h.rect.max_y() && x >= h.rect.max_x() && x <= h.rect.max_x() + METER_W)?;
    Some(MenuState::pointer(MenuTarget::Meter { strip: meter.id.strip.clone() }, x, y))
}

/// The meter column right of a strip's fader hit (fader 48 pt of the 96-pt strip).
const METER_W: f32 = 46.0;

fn row_id(level: u8, index: u16) -> ControlId {
    ControlId { strip: "menu".into(), control: Control::Menu(MenuCmd::Row { level, index }) }
}

/// The menu row under (`x`, `y`): the topmost row marker or submenu opener. `(level, index,
/// submenu)`.
fn row_under(hits: &[Hit], x: f32, y: f32) -> Option<(u8, u16, bool)> {
    hits.iter().rev().filter(|h| h.rect.contains(x, y)).find_map(|h| match h.id.control {
        Control::Menu(MenuCmd::Row { level, index }) => Some((level, index, false)),
        Control::Menu(MenuCmd::OpenSub { level, index }) => Some((level, index, true)),
        _ => None,
    })
}
