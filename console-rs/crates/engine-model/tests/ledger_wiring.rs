//! Ledger: the wiring audit. Every hit region of every surface, in every view state, is pressed
//! (or dragged, or right-clicked) through the real input path — `Interaction` → `ViewState` →
//! `EngineMixer` — and classified by what it did:
//!
//! - `engine`: an engine write (the writer's suppressed commands; writes stay off offline),
//! - `view`: `ViewState` changed (a popover, panel, tab, section, scroll …),
//! - `shell`: handed to the app shell (a tap queued for the plug-in window, a menu command),
//! - `model`: model-only state (the writes gate, a local tree value with no engine write),
//! - `noop`: none of these.
//!
//! Instances are grouped by control shape (indices folded to `#`) and strip class; a group is
//! wired when any instance did something. Context menus are opened with a right click on every
//! hit and each row is pressed too. The report goes to `$LEDGER_OUT/wiring.json`; the test
//! fails on a no-op group that is not in [`KNOWN_NOOPS`] (a ratchet: remove entries as they get
//! wired). With `CONSOLE_RESOURCES` set the real Console art and layouts are used (more hits:
//! preamp layouts); otherwise a minimal fixture Appearance. `LEDGER_SNAPSHOT` overrides the
//! engine snapshot (default: the x4 12.0.0 fixture).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use engine_model::EngineMixer;
use ua_engine::{Snapshot, Store};
use houston::Resources;
use rack::menu::PopupEntry;
use rack::{Assets, Control, Hit, HitKind, Interaction, MenuCmd, Ui, ViewState, build_window_with};

const FIXTURE_APPEARANCE: &str = r#"{
    "file_format": "appearance",
    "views": {
        "Switch": {}, "Switch-Small": {}, "Switch-Small-Red": {}, "Switch-Red": {}, "Switch-Large": {},
        "Switch-Tight-Kerning": {}, "Switch-Red-Tight-Kerning": {}, "Mini-Send-Mute": {}, "Mini-Send-Pre": {}, "Send-Mute": {},
        "Console-Nav-Row-Button-Open": {}, "Console-Nav-Row-Button-Close": {}, "Console-Nav-Row-Button-Large": {},
        "Console-Nav-Row-Button-Small": {}, "Console-Nav-Row-Button-Fixed-Slots": {},
        "Switch-Power-Mode": {}, "Switch-Remove-Mode": {}, "Switch-Copy-Mode": {}, "Switch-Isolate-Mode": {},
        "Switch-Large-Tight-Kerning": {}, "Switch-Popover-Left": {}, "Channel-Input-Name": {}, "Channel-Aux-Name": {}
    },
    "inserts": { "Small-Normal": {} },
    "knobs": { "Mini-Send-Level": {}, "Pan-Black": {} }
}"#;

fn assets() -> (Assets, &'static str) {
    if let Some(dir) = std::env::var_os("CONSOLE_RESOURCES") {
        let res = Resources::at(&PathBuf::from(dir)).expect("CONSOLE_RESOURCES");
        return (Assets::load(res).expect("Console appearance"), "console-resources");
    }
    let dir = std::env::temp_dir().join(format!("ledger-wiring-fixture-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("appearance")).unwrap();
    std::fs::write(dir.join("appearance/Appearance.json"), FIXTURE_APPEARANCE).unwrap();
    (Assets::load(Resources::at(&dir).unwrap()).unwrap(), "fixture-appearance")
}

fn snapshot() -> String {
    std::env::var("LEDGER_SNAPSHOT").unwrap_or_else(|_| format!("{}/../ua-engine/tests/fixtures/apollo-x4-12.0.0.snapshot.json", env!("CARGO_MANIFEST_DIR")))
}

fn out_dir() -> PathBuf {
    let d = std::env::var_os("LEDGER_OUT").map(Into::into).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ledger"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `/devices/0/inputs/3` → `input`, `/devices/0/auxs/1` → `aux`, others as they are.
fn strip_class(strip: &str) -> String {
    for (needle, class) in [("/inputs/", "input"), ("/auxs/", "aux"), ("/outputs/", "output"), ("/effects/", "plugin-slot")] {
        if strip.contains(needle) {
            return class.into();
        }
    }
    strip.trim_end_matches(|c: char| c.is_ascii_digit()).to_owned()
}

/// `Insert(3)` → `Insert(#)`, `SettingMenu("/devices/0/X")` → `SettingMenu("/devices/#/X")`.
fn shape(c: &Control) -> String {
    let s = format!("{c:?}");
    let mut out = String::with_capacity(s.len());
    let mut prev_digit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            if !prev_digit {
                out.push('#');
            }
            prev_digit = true;
        } else {
            out.push(ch);
            prev_digit = false;
        }
    }
    out
}

/// The command shape for the report: `set /devices/#/inputs/#/Mute`.
fn cmd_shape(text: &str) -> String {
    let mut parts = text.splitn(3, ' ');
    let verb = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default().split('?').next().unwrap_or_default();
    let path = path.strip_suffix("/value").unwrap_or(path);
    let path: Vec<&str> = path.split('/').map(|s| if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) { "#" } else { s }).collect();
    format!("{verb} {}", path.join("/"))
}

#[derive(Default)]
struct Group {
    strip_class: String,
    control: String,
    instances: usize,
    engine: usize,
    view: usize,
    shell: usize,
    model: usize,
    /// Disabled menu items (not pressed: Console greys them out too).
    disabled: usize,
    scenarios: BTreeSet<String>,
    via_menu: bool,
    commands: BTreeSet<String>,
}

impl Group {
    fn outcome(&self) -> &'static str {
        if self.engine > 0 {
            "engine"
        } else if self.shell > 0 {
            "shell"
        } else if self.model > 0 {
            "model"
        } else if self.view > 0 {
            "view"
        } else {
            "noop"
        }
    }
}

struct Audit {
    assets: Assets,
    snap: Snapshot,
    /// The model of the press in progress; every press starts from the snapshot again, so one
    /// press (say SOURCE › LINE) cannot disable a later one (48V).
    model: EngineMixer,
    groups: BTreeMap<String, Group>,
    seen: u64,
    /// The last press changed the model: rebuild it before the next one.
    dirty: bool,
    /// Control groups whose hit centre is covered by another hit (never pressed there).
    occluded: BTreeMap<String, BTreeSet<String>>,
}

impl Audit {
    fn fresh(&mut self) {
        if !self.dirty {
            return;
        }
        self.model = EngineMixer::new(Store::from_snapshot(self.snap.clone()));
        self.seen = self.model.store().writer().suppressed_count();
        self.dirty = false;
    }

    fn hits(&self, view: &ViewState) -> Vec<Hit> {
        let mut ui = Ui::new(&self.assets);
        build_window_with(&mut ui, &self.model, view);
        ui.hits
    }

    fn written(&mut self) -> Vec<String> {
        self.model.store_mut().flush();
        let w = self.model.store().writer();
        let n = (w.suppressed_count() - self.seen) as usize;
        self.seen = w.suppressed_count();
        let recent: Vec<&str> = w.recent_suppressed().collect();
        recent[recent.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect()
    }

    /// One press of `hit` (drags move `dy` points); what it wrote, whether it reached the
    /// shell, the resulting view, and whether model-only state changed.
    fn one_press(&mut self, base: &ViewState, hits: &[Hit], hit: &Hit, dy: f32) -> (Vec<String>, bool, ViewState, bool) {
        self.fresh();
        let mut view = base.clone();
        let (cx, cy) = hit.rect.center();
        let mut i = Interaction::default();
        let mut actions = i.pointer_down(hits, cx, cy, false);
        if matches!(hit.kind, HitKind::Drag { .. }) {
            actions.extend(i.pointer_move(cx, cy + dy, false));
        }
        actions.extend(i.pointer_up());
        let before = self.written().len();
        debug_assert_eq!(before, 0);
        let gate = self.model.store().writer().writes_enabled;
        let revision = self.model.store().revision();
        for a in actions {
            view.dispatch(a, &mut self.model);
        }
        let cmds: Vec<String> = self.written().into_iter().filter(|c| !c.contains("/UndoRecording/")).collect();
        let shell = !self.model.take_taps().is_empty() | !view.take_commands().is_empty();
        let model_only = gate != self.model.store().writer().writes_enabled || revision != self.model.store().revision();
        self.dirty = model_only || !cmds.is_empty() || shell;
        (cmds, shell, view, model_only)
    }

    /// Presses `hit` in `view` (drags move 20 pt up, then down when up did nothing: a thumb or
    /// knob at its top) and records the outcome.
    fn press(&mut self, scenario: &str, base: &ViewState, hits: &[Hit], hit: &Hit, via_menu: Option<(String, String)>) -> ViewState {
        let (mut cmds, mut shell, mut view, mut model_only) = self.one_press(base, hits, hit, -20.0);
        if matches!(hit.kind, HitKind::Drag { .. }) && cmds.is_empty() && !shell && !model_only && view == *base {
            (cmds, shell, view, model_only) = self.one_press(base, hits, hit, 20.0);
        }
        let changed = view != *base;
        // Menu rows are keyed by their menu and label (`menu Insert(input) › Remove`).
        let (class, control) = match &via_menu {
            Some((menu, label)) => (format!("menu {menu}"), label.clone()),
            None => (strip_class(&hit.id.strip), shape(&hit.id.control)),
        };
        let key = format!("{class} {control}");
        let g = self.groups.entry(key).or_insert_with(|| Group { strip_class: class, control, ..Default::default() });
        g.instances += 1;
        g.scenarios.insert(scenario.to_owned());
        g.via_menu |= via_menu.is_some();
        if !cmds.is_empty() {
            g.engine += 1;
            g.commands.extend(cmds.iter().map(|c| cmd_shape(c)));
        } else if shell {
            g.shell += 1;
        } else if changed {
            g.view += 1;
        } else if model_only {
            g.model += 1;
        }
        view
    }

    /// Every hit of `base`, then a right click on every hit and every row of the menu it opens.
    fn scenario(&mut self, name: &str, base: &ViewState) {
        self.fresh();
        let hits = self.hits(base);
        // Two instances per group (strip class + control shape) per scenario: every press
        // rebuilds the model from the snapshot, so pressing all eight faders adds nothing.
        let mut done: BTreeMap<String, usize> = BTreeMap::new();
        for hit in &hits {
            let n = done.entry(format!("{} {}", strip_class(&hit.id.strip), shape(&hit.id.control))).or_default();
            *n += 1;
            if *n > 2 {
                continue;
            }
            // A hit whose centre is covered by a later one cannot be pressed there: record it
            // (overlaps are usually backgrounds; a covered control is a layout bug).
            let (cx, cy) = hit.rect.center();
            if let Some(top) = hits.iter().rev().find(|h| h.rect.contains(cx, cy)) {
                if top.id != hit.id {
                    self.occluded.entry(format!("{} {}", strip_class(&hit.id.strip), shape(&hit.id.control))).or_default().insert(format!("{name}: under {} {}", strip_class(&top.id.strip), shape(&top.id.control)));
                    continue;
                }
            }
            self.press(name, base, &hits, hit, None);
            // Right click: a context menu (or a Sends window) for this control? A fader also
            // gets one on its meter, which has no hit of its own (the meter menu).
            let mut points = vec![hit.rect.center()];
            if hit.id.control == Control::Fader {
                points.push((hit.rect.max_x() + 8.0, hit.rect.y + hit.rect.h / 2.0));
            }
            for (cx, cy) in points {
            self.fresh();
            let mut v = base.clone();
            if v.context_click(&hits, cx, cy) && v.menu.is_some() {
                let menu_hits = self.hits(&v);
                let target = v.menu.as_ref().map(|m| m.target.clone()).expect("open menu");
                let (popup, _) = rack::menu::build_menu(&self.model, &v, &target, true);
                let menu_name = format!("{}({})", format!("{target:?}").split([' ', '(', '{']).next().unwrap_or("?"), strip_class(&hit.id.strip));
                let rows: Vec<&Hit> = menu_hits.iter().filter(|h| matches!(h.id.control, Control::Menu(_))).collect();
                let mut seen_rows = BTreeSet::new();
                for row in rows {
                    if !seen_rows.insert(format!("{:?}", row.id.control)) {
                        continue;
                    }
                    // Root-level rows only (submenu rows show once a submenu opens); headers and
                    // separators are inert by design; disabled items are recorded as such.
                    let Control::Menu(MenuCmd::Row { level: 0, index }) = &row.id.control else { continue };
                    let label = match popup.entries.get(*index as usize) {
                        Some(PopupEntry::Item(i)) if i.enabled => i.label.clone(),
                        Some(PopupEntry::Item(i)) => {
                            let key = format!("menu {menu_name} {}", i.label);
                            let g = self.groups.entry(key).or_insert_with(|| Group { strip_class: format!("menu {menu_name}"), control: i.label.clone(), ..Default::default() });
                            g.instances += 1;
                            g.disabled += 1;
                            g.via_menu = true;
                            g.scenarios.insert(name.to_owned());
                            continue;
                        }
                        Some(PopupEntry::Submenu { label, .. }) => format!("{label} ▸"),
                        _ => continue,
                    };
                    self.press(&format!("{name} › right-click {}", shape(&hit.id.control)), &v, &menu_hits, row, Some((menu_name.clone(), label)));
                }
            }
            }
        }
    }
}

/// Control groups that do nothing today (strip class + control shape). A ratchet: the test fails
/// when a group not listed here does nothing, and prints the listed groups that now work.
const KNOWN_NOOPS: &[&str] = &[
    // Control-room SOURCE (MONITOR / CUE n): no engine write (`outputs/<MON>/MixInSource`). Gap.
    "monitor CrSource(#)",
    // Settings › NETWORK / I/O MATRIX buttons that launch other apps or URLs (Dante Controller,
    // Dante discovery refresh, patent info, CASCADE editor): not implemented. Gap.
    "settings SettingPress(\"launch:dante_controller\")",
    "settings SettingPress(\"launch:dante_discovery\")",
    "settings SettingPress(\"launch:dante_patents\")",
    "settings SettingPress(\"launch:io_cascade\")",
    // Settings `post:` buttons: `write_settings` sends them on the connection directly, outside
    // the writer gate, and drops them silently while read-only — so offline they do nothing
    // visible. Gap: route them through `Store::post` (counted, gated, dev-panel visible).
    "settings SettingPress(\"post:Identify:/devices/#\")",
    "settings SettingPress(\"post:RemoveDevice:/devices/#\")",
    "settings SettingPress(\"post:Import:IOMapPreset\")",
    "settings SettingPress(\"post:SaveIOMapPreset:Custom\")",
    "settings SettingPress(\"post:ShowSoundIDReference:headphone\")",
    "settings SettingPress(\"post:ShowSoundIDReference:speaker\")",
    // Insert menu Paste is enabled with an empty clipboard; Console greys it out (s01). Gap.
    "menu Insert(aux) Paste",
    "menu Insert(input) Paste",
    // PLUG-IN SCENES row menu: Clear, Overwrite, Rename and Recall send nothing. In progress
    // (rc/wiring, MAP.md P0 #1): post /SceneOperation {"RecallScene"|"StoreScene"|"OverwriteScene"|
    // "ClearScene": i}; rename = set scenes/i/SceneName.
    "menu SceneRow(popover) Clear",
    "menu SceneRow(popover) Overwrite",
    "menu SceneRow(popover) Recall Scene",
    "menu SceneRow(popover) Rename",
    "menu SceneRow(settings) Clear",
    "menu SceneRow(settings) Overwrite",
    "menu SceneRow(settings) Recall Scene",
    "menu SceneRow(settings) Rename",
    // CURRENT SESSION row menu › Load on the session that is already open: nothing sent. Check
    // what Console does (reload?). Gap.
    "menu SessionRow(session) Load 'Session'",
    // Side section S / OL: S clears soloed inputs one by one (nothing to do with no solo; Console
    // posts `/ClearSolos {}`), OL has no engine command (Console: `post /ClearClips {}`). Gap.
    "global ClearClips",
    "global ClearSolos",
    // Benign: INSERT EFFECTS UAD MON when every input already records post-inserts; a session
    // navigator slot for a strip that is already in view.
    "nav AllRecordPreEffects(true)",
    "session SessionNavigate(#)",
    // Benign without plug-in scenes (the x4 fixture has none): nothing to step through.
    "settings SceneStep(false)",
    "settings SceneStep(true)",
    // Benign with one device: selecting the device that is already selected.
    "settings SettingsDevice(#)",
];

#[test]
fn every_control_is_wired() {
    let (assets, art) = assets();
    let snap = Snapshot::load(snapshot()).expect("snapshot");
    let model = EngineMixer::new(Store::from_snapshot(snap.clone()));
    let seen = model.store().writer().suppressed_count();
    let mut audit = Audit { assets, snap, model, groups: BTreeMap::new(), seen, dirty: false, occluded: BTreeMap::new() };
    let mut scenarios: Vec<(String, ViewState)> = Vec::new();
    let spec = |s: &str| {
        let mut v = ViewState::default();
        v.apply_spec(s).unwrap_or_else(|e| panic!("{s}: {e}"));
        v
    };
    for s in ["", "inputs=1,inserts=1,sends=1,fader=1", "overview=1", "small,fixed=1", "aux=1,ctrl=1", "aux=0,ctrl=1", "tab=presets", "scenes=1", "browser=0", "compact=1", "assign=/devices/0/inputs/0:0"] {
        scenarios.push((if s.is_empty() { "default".into() } else { s.into() }, spec(s)));
    }
    for name in [
        "settings:hardware", "settings:io-matrix", "settings:options", "settings:midi", "settings:network", "settings:plug-ins", "settings:dante",
        "cue-outputs", "monitor-controller", "monitor-controller:1", "headphone-correction", "meters", "talkback", "aux-sends:0", "input-sends:0",
        "add-device", "io-presets", "input-source:0", "tempo", "about",
    ] {
        let mut v = ViewState::default();
        if v.open_named(name) {
            scenarios.push((format!("open {name}"), v));
        }
    }
    // The CURRENT SESSION panel and the menu bar's menus.
    {
        let mut v = ViewState::default();
        let hits = audit.hits(&v);
        if let Some(h) = hits.iter().find(|h| h.id.control == Control::SessionMenu) {
            let (x, y) = h.rect.center();
            let mut i = Interaction::default();
            for a in i.pointer_down(&hits, x, y, false).into_iter().chain(i.pointer_up()) {
                v.dispatch(a, &mut audit.model);
            }
            scenarios.push(("session panel".into(), v));
        }
        for bar in 0..6 {
            let mut v = ViewState::default();
            let hits = audit.hits(&v);
            if v.open_menu_spec(&hits, &format!("bar:{bar}"), &audit.model).is_ok() {
                scenarios.push((format!("menu bar {bar}"), v));
            }
        }
    }
    for (name, v) in &scenarios {
        audit.scenario(name, v);
    }

    let groups = &audit.groups;
    let count = |o: &str| groups.values().filter(|g| g.outcome() == o).count();
    let report: Vec<serde_json::Value> = groups
        .iter()
        .map(|(k, g)| {
            serde_json::json!({
                "key": k, "strip_class": g.strip_class, "control": g.control, "outcome": g.outcome(),
                "instances": g.instances, "engine": g.engine, "view": g.view, "shell": g.shell, "model": g.model, "disabled": g.disabled,
                "via_menu": g.via_menu, "commands": g.commands, "scenarios": g.scenarios,
            })
        })
        .collect();
    let summary = serde_json::json!({
        "art": art, "snapshot": snapshot(), "scenarios": scenarios.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "groups": groups.len(), "engine": count("engine"), "view": count("view"), "shell": count("shell"), "model": count("model"), "disabled": count("disabled"), "noop": count("noop"),
    });
    // Groups only ever seen covered (never pressed anywhere).
    let covered: BTreeMap<&String, &BTreeSet<String>> = audit.occluded.iter().filter(|(k, _)| !groups.contains_key(*k)).collect();
    std::fs::write(out_dir().join("wiring.json"), serde_json::to_string_pretty(&serde_json::json!({ "summary": summary, "groups": report, "only_occluded": covered })).unwrap()).unwrap();
    for (k, why) in &covered {
        eprintln!("OCCLUDED {k}: {}", why.iter().next().map(String::as_str).unwrap_or(""));
    }
    eprintln!("wiring ({art}): {} groups — engine {}, view {}, shell {}, model {}, disabled {}, noop {}", groups.len(), count("engine"), count("view"), count("shell"), count("model"), count("disabled"), count("noop"));

    let noops: BTreeSet<&str> = groups.iter().filter(|(_, g)| g.outcome() == "noop").map(|(k, _)| k.as_str()).collect();
    let known: BTreeSet<&str> = KNOWN_NOOPS.iter().copied().collect();
    let now_wired: Vec<&&str> = known.iter().filter(|k| groups.contains_key(**k) && !noops.contains(**k)).collect();
    if !now_wired.is_empty() {
        eprintln!("now wired (remove from KNOWN_NOOPS): {now_wired:?}");
    }
    let new: Vec<&&str> = noops.iter().filter(|k| !known.contains(**k)).collect();
    for k in &new {
        eprintln!("NOOP {k}");
    }
    assert!(new.is_empty(), "{} control groups do nothing and are not in KNOWN_NOOPS (see wiring.json)", new.len());
}
