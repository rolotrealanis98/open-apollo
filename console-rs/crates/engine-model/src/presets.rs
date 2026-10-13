//! Presets against the engine (UA Mixer Engine 12.0.0; `rack::presets` has the rules):
//! loading is a `Preset` set (`CMxAssignPreset`, one undo step), files are root posts
//! (`SaveAsPreset`, `MovePreset`, `DeletePreset`, `NewFolder`), the plug-in bar's copy/paste is
//! `PluginBarCopySettings` / `PluginBarPasteSettings`. Every write goes through the store's
//! write gate.

use rack::model::{PluginFooter, StripInfo};
use rack::presets::{PresetCmd, PresetScope, renamed};
use rack::{Action, Control, ControlId, PresetNode};
use ua_engine::{Json, SessionEntry, Value};

use crate::EngineMixer;

/// The engine's `pluginName` for Channel Strip presets.
pub const CHANNEL_STRIP: &str = "Channel Strip";

fn convert(e: &SessionEntry) -> PresetNode {
    match e {
        SessionEntry::File { name, locked, .. } => PresetNode::file(name).locked(*locked),
        SessionEntry::Folder { name, children, locked, .. } => {
            let mut c: Vec<_> = children.iter().map(convert).collect();
            c.sort_by_key(|n| n.name.to_lowercase());
            PresetNode::folder(name, c).locked(*locked)
        }
    }
}

fn obj(pairs: &[(&str, Json)]) -> Json {
    Json::Object(pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect())
}

fn s(v: &str) -> Json {
    Json::String(v.to_string())
}

impl EngineMixer {
    /// A preset tree from a `Preset` property's `values` (folders sorted, then files, by name).
    pub(crate) fn preset_tree_at(&self, path: &str) -> Vec<PresetNode> {
        let Some(values) = self.store.tree().meta(path).and_then(|m| m.values.clone()) else { return Vec::new() };
        let mut out: Vec<_> = ua_engine::session::parse_entries(&values).iter().map(convert).collect();
        out.sort_by_key(|n| n.name.to_lowercase());
        out
    }

    /// The `/plugins/N` catalogue entry named `name` (an insert's `EffectName`).
    pub(crate) fn plugin_entry(&self, name: &str) -> Option<String> {
        self.store.tree().indices("/plugins").into_iter().map(|p| format!("/plugins/{p}")).find(|base| self.store.value(&format!("{base}/Name")).and_then(|v| v.as_str()) == Some(name))
    }

    /// `(object path, pluginName)` a scope's preset commands name: the strip and "Channel Strip",
    /// or the insert's effect and its plug-in.
    fn preset_object(&self, strip: &str, scope: PresetScope) -> Option<(String, String)> {
        match scope {
            PresetScope::Channel => Some((strip.to_string(), CHANNEL_STRIP.to_string())),
            PresetScope::Insert(k) => {
                let slot = self.insert_slot(&ControlId { strip: strip.into(), control: Control::Insert(k) })?;
                let name = self.store.value(&slot.effect_name()).and_then(|v| v.as_str()).filter(|n| !n.is_empty())?.to_string();
                Some((slot.base, name))
            }
        }
    }

    pub(crate) fn surface_plugin_presets(&self, s: &StripInfo, slot: u8) -> Vec<PresetNode> {
        let Some((base, name)) = self.preset_object(&s.id, PresetScope::Insert(slot)) else { return Vec::new() };
        // The effect's own `Preset` values when the engine sends them, else the catalogue's.
        let own = self.preset_tree_at(&format!("{base}/Preset"));
        if !own.is_empty() {
            return own;
        }
        self.plugin_entry(&name).map(|p| self.preset_tree_at(&format!("{p}/Preset"))).unwrap_or_default()
    }

    pub(crate) fn surface_preset_name(&self, s: &StripInfo, scope: PresetScope) -> Option<String> {
        let (base, _) = self.preset_object(&s.id, scope)?;
        self.store.value(&format!("{base}/Preset")).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(|v| v.trim_start_matches('/').to_string())
    }

    pub(crate) fn surface_can_paste_plugin_settings(&self, s: &StripInfo, slot: u8) -> bool {
        let Some((base, _)) = self.preset_object(&s.id, PresetScope::Insert(slot)) else { return false };
        self.bool_at_path(&format!("{base}/CanPastePluginBarSettings")).unwrap_or(false)
    }

    /// The footer for an unlicensed plug-in, from its catalogue entry (`Authorized`, `Demoing`,
    /// `DemoAvailable`, `DemoDaysRemaining`): Rack's state strings, upper case.
    pub(crate) fn surface_plugin_footer(&self, effect_name: &str) -> Option<PluginFooter> {
        let base = self.plugin_entry(effect_name)?;
        let flag = |k: &str| self.bool_at_path(&format!("{base}/{k}"));
        if flag("Authorized").unwrap_or(true) {
            return None;
        }
        let days = self.store.value(&format!("{base}/DemoDaysRemaining")).and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
        let (status, state, action) = if flag("Demoing").unwrap_or(false) {
            (format!("DEMO ACTIVE - {days} DAYS"), 3, Some("BUY".to_string()))
        } else if flag("DemoAvailable").unwrap_or(false) {
            ("DEMO NOT STARTED".to_string(), 0, Some("START DEMO".to_string()))
        } else {
            ("DEMO EXPIRED".to_string(), 2, Some("BUY".to_string()))
        };
        Some(PluginFooter { status, state, action, plugin_path: Some(base) })
    }

    fn bool_at_path(&self, path: &str) -> Option<bool> {
        self.store.value(path).and_then(Value::as_bool)
    }

    /// `set_text` targets: a load (`Preset`) or a named file command. True when handled.
    pub(crate) fn preset_set_text(&mut self, id: &ControlId, text: &str) -> bool {
        match &id.control {
            Control::Preset(scope) => {
                if let Some((base, _)) = self.preset_object(&id.strip, *scope) {
                    let value = text.trim_start_matches('/');
                    if !value.is_empty() {
                        self.store.commit(&format!("{base}/Preset"), value);
                    }
                }
                true
            }
            Control::PresetCmd(scope, cmd) => {
                self.preset_command(&id.strip, *scope, cmd, Some(text));
                true
            }
            _ => false,
        }
    }

    /// Taps: the commands that need no name (Save, Delete, plug-in bar copy / paste). True when
    /// handled.
    pub(crate) fn apply_presets(&mut self, action: &Action) -> bool {
        let Action::Tap(id) = action else { return false };
        let Control::PresetCmd(scope, cmd) = &id.control else { return false };
        self.preset_command(&id.strip, *scope, cmd, None);
        true
    }

    fn preset_command(&mut self, strip: &str, scope: PresetScope, cmd: &PresetCmd, text: Option<&str>) {
        let Some((object, plugin)) = self.preset_object(strip, scope) else { return };
        let name = text.map(|t| t.trim().trim_matches('/').to_string()).filter(|t| !t.is_empty());
        let post = match (cmd, name) {
            (PresetCmd::SaveAs, Some(n)) => Some(("/SaveAsPreset", obj(&[("path", s(&object)), ("pluginName", s(&plugin)), ("presetName", s(&n))]))),
            (PresetCmd::Save, _) => {
                let current = self.store.value(&format!("{object}/Preset")).and_then(|v| v.as_str()).map(|v| v.trim_start_matches('/').to_string()).filter(|v| !v.is_empty());
                current.map(|n| ("/SaveAsPreset", obj(&[("path", s(&object)), ("pluginName", s(&plugin)), ("presetName", s(&n))])))
            }
            (PresetCmd::Rename(orig), Some(n)) => Some((
                "/MovePreset",
                obj(&[("isFolder", Json::Bool(false)), ("origPresetName", s(orig)), ("newPresetName", s(&renamed(orig, &n))), ("pluginName", s(&plugin))]),
            )),
            (PresetCmd::Delete(orig), _) => Some(("/DeletePreset", obj(&[("pluginName", s(&plugin)), ("presetName", s(orig))]))),
            (PresetCmd::NewFolder, Some(n)) => Some(("/NewFolder", obj(&[("folder", s(&n)), ("path", s(&format!("{object}/Preset")))]))),
            (PresetCmd::CopySettings, _) if matches!(scope, PresetScope::Insert(_)) => Some(("/PluginBarCopySettings", obj(&[("effectPath", s(&object))]))),
            (PresetCmd::PasteSettings, _) if matches!(scope, PresetScope::Insert(_)) => Some(("/PluginBarPasteSettings", obj(&[("effectPath", s(&object))]))),
            _ => None,
        };
        if let Some((path, payload)) = post {
            self.store.post(path, Some(&payload));
        }
    }

    /// The footer's START DEMO: `post /PluginStartDemo {"pluginPath"}` (Rack_PlugInFooter
    /// `DoStateClicked`).
    pub fn start_plugin_demo(&mut self, plugin_path: &str) {
        self.store.post("/PluginStartDemo", Some(&obj(&[("pluginPath", s(plugin_path))])));
    }
}
