//! Insert slots as Console draws them, merging UAD-2 inserts (engine, 4710) with native
//! plug-ins from the UA Mixer Helper (4720), plus the helper connection itself.
//!
//! Console's slot labels (reference shots of Console 1.3.1 on macOS):
//! - a UAD-2 plug-in with the Apollo online is labelled with the catalogue's short name that
//!   fits the slot (`/plugins/P/Name/values`, picked by [`rack::model::insert_label_for`], as
//!   `Rack_PlugInHeader::GetEffectNameForFont` does: "1176 Rev A" for `UAD UA 1176 Rev A`,
//!   "CL 1B mkII" for `UAD Tube-Tech CL 1B mk II`), upright;
//! - with the Apollo offline (`/devices/0/DeviceOnline` false) the label is the engine name
//!   without "UAD " ("Millennia NSEQ-2"), in italics;
//! - a native plug-in shows its `plugin_name` (no live sample yet: see the ua-engine README).

use rack::model::{ConsoleModel, InsertLabel, InsertState, StripInfo};
use ua_engine::{
    AuxStrip, DEFAULT_PORT, Endpoint, HELPER_PORT, HelperInfo, HelperStore, InputStrip,
    InsertPaths, Json, NativeInsert, NativeOwner, Value,
};

use crate::{EngineMixer, INSERT_SLOTS};

/// Where a slot's plug-in runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotSource {
    Empty,
    /// UAD-2 DSP, from the engine's `effects/K/EffectName` (4710).
    Uad2,
    /// Native (UADx), from the helper's supercore track `effects/effect_K` (4720).
    Native,
}

/// One insert slot with everything Console uses to draw it.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotInfo {
    /// What `MixerModel::inserts` returns (`name` stays the engine/catalogue name, the icon key).
    pub state: InsertState,
    pub source: SlotSource,
    /// The text Console draws in the slot.
    pub label: String,
    /// Console draws the label in italics (UAD-2 plug-in with the Apollo offline).
    pub italic: bool,
    /// The helper's record, for native slots.
    pub native: Option<NativeInsert>,
}

impl SlotInfo {
    pub fn empty() -> SlotInfo {
        SlotInfo {
            state: InsertState::Empty,
            source: SlotSource::Empty,
            label: String::new(),
            italic: false,
            native: None,
        }
    }

    pub fn is_native(&self) -> bool {
        self.source == SlotSource::Native
    }

    /// The label rack draws (None for an empty slot).
    pub fn insert_label(self) -> Option<InsertLabel> {
        (self.source != SlotSource::Empty).then_some(InsertLabel {
            text: self.label,
            italic: self.italic,
        })
    }
}

impl EngineMixer {
    /// Connects the read-only helper client (also done by [`EngineMixer::connect`] when the engine
    /// is on the default port: the helper is then `host:4720`).
    pub fn connect_helper(&mut self, endpoint: Endpoint) {
        let mut h = HelperStore::new();
        h.connect(endpoint);
        self.helper = Some(h);
    }

    /// Like [`EngineMixer::connect_helper`]; with `writes` the helper also takes Console's
    /// helper settings (NETWORK INTERFACE).
    pub fn connect_helper_writable(&mut self, endpoint: Endpoint, writes: bool) {
        let mut h = HelperStore::new();
        h.connect_writable(endpoint, writes);
        self.helper = Some(h);
    }

    /// The helper endpoint that goes with an engine endpoint (`host:4720` for `host:4710`).
    pub fn helper_endpoint(engine: &Endpoint) -> Option<Endpoint> {
        (engine.port == DEFAULT_PORT).then(|| Endpoint::new(engine.host.clone(), HELPER_PORT))
    }

    /// Uses an existing helper store (e.g. offline, from a capture).
    pub fn set_helper(&mut self, helper: HelperStore) {
        self.helper = Some(helper);
    }

    pub fn helper(&self) -> Option<&HelperStore> {
        self.helper.as_ref()
    }

    pub fn helper_mut(&mut self) -> Option<&mut HelperStore> {
        self.helper.as_mut()
    }

    pub fn helper_info(&self) -> Option<HelperInfo> {
        self.helper.as_ref().map(HelperStore::info)
    }

    fn device_online(&self, device: usize) -> bool {
        self.store
            .value(&format!("/devices/{device}/DeviceOnline"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// The catalogue's short names for an engine plug-in name (`Name/values`), None when the
    /// catalogue has no such plug-in.
    pub fn short_names(&self, effect_name: &str) -> Option<Vec<String>> {
        let tree = self.store.tree();
        tree.children("/plugins").iter().find_map(|p| {
            let meta = tree.property(&format!("/plugins/{p}/Name"))?;
            if meta.value.as_ref().and_then(Value::as_str) != Some(effect_name) {
                return None;
            }
            let values = meta.meta.values.as_ref().and_then(Json::as_array);
            Some(
                values
                    .into_iter()
                    .flatten()
                    .filter_map(|v| match v {
                        Json::Object(o) => o.get("value")?.as_str().map(str::to_owned),
                        Json::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
            )
        })
    }

    /// The catalogue's first short name for an engine plug-in name (`Name/values[0]`).
    pub fn short_name(&self, effect_name: &str) -> Option<String> {
        self.short_names(effect_name)?.into_iter().next()
    }

    /// Console's label and italics for a UAD-2 plug-in.
    pub fn plugin_label(&self, effect_name: &str) -> (String, bool) {
        self.plugin_label_on_device(effect_name, self.store.topology().device)
    }

    fn plugin_label_on_device(&self, effect_name: &str, device: usize) -> (String, bool) {
        if !self.device_online(device) {
            let long = effect_name.strip_prefix("UAD ").unwrap_or(effect_name);
            return (long.to_owned(), true);
        }
        let shorts = self.short_names(effect_name).unwrap_or_default();
        (rack::model::insert_label_for(effect_name, &shorts), false)
    }

    /// Lowercased names a helper track may carry for this input (display, engine and default
    /// names).
    fn input_names(&self, s: &InputStrip) -> Vec<String> {
        let display = self
            .str_at(&s.display_name_path())
            .unwrap_or(&s.name)
            .to_owned();
        [Some(display), Some(s.name.clone()), s.default_name.clone()]
            .into_iter()
            .flatten()
            .map(|n| n.trim().to_lowercase())
            .collect()
    }

    /// Lowercased names a helper bus may carry for this aux.
    fn aux_names(&self, a: &AuxStrip) -> Vec<String> {
        [
            self.str_at(&a.name_path()).map(str::to_owned),
            Some(a.name.clone()),
        ]
        .into_iter()
        .flatten()
        .map(|n| n.trim().to_lowercase())
        .collect()
    }

    /// The console model assigned to a supercore track (inputs) or bus (auxes) named like the
    /// strip: the `plugin_id` of its `effects/console` slot (`insert_type` `"console"`, the slot
    /// Rack reads as insert kind 3, index 0). Apollo strips on 4710 have no console slot, so
    /// without a matching helper track or bus this is None. `plugin_id` is not among the
    /// subscribed slot properties: it comes from the helper's full read and its `tracks` polls,
    /// so a model assigned to a bus shows after a reconnect.
    pub(crate) fn console_model_of(&self, names: &[String], owner: NativeOwner) -> Option<ConsoleModel> {
        let tree = self.helper.as_ref()?.tree();
        let coll = match owner {
            NativeOwner::Track => "tracks",
            NativeOwner::Bus => "buses",
        };
        tree.children("/devices").iter().find_map(|device| {
            let cp = format!("/devices/{device}/{coll}");
            tree.children(&cp).iter().find_map(|uid| {
                let base = format!("{cp}/{uid}");
                let name = tree.value(&format!("{base}/name")).and_then(Value::as_str)?;
                if !names.contains(&name.trim().to_lowercase()) {
                    return None;
                }
                let id = tree
                    .value(&format!("{base}/effects/console/plugin_id"))
                    .and_then(Value::as_str)?;
                ConsoleModel::from_plugin_id(id)
            })
        })
    }

    pub(crate) fn input_console_model(&self, s: &InputStrip) -> Option<ConsoleModel> {
        self.helper.as_ref()?;
        self.console_model_of(&self.input_names(s), NativeOwner::Track)
    }

    pub(crate) fn aux_console_model(&self, a: &AuxStrip) -> Option<ConsoleModel> {
        self.helper.as_ref()?;
        self.console_model_of(&self.aux_names(a), NativeOwner::Bus)
    }

    /// Native plug-ins on the helper track that matches this strip (by name, case-insensitive:
    /// the track ↔ Apollo input mapping has not been observed live).
    pub fn native_inserts(&self, strip: &StripInfo) -> Vec<NativeInsert> {
        let Some(h) = &self.helper else {
            return Vec::new();
        };
        let Some(s) = self.input(&strip.id) else {
            return Vec::new();
        };
        let mut names = self.input_names(s);
        names.push(strip.title.trim().to_lowercase());
        h.native_inserts()
            .into_iter()
            .filter(|n| {
                n.owner == NativeOwner::Track
                    && n.owner_name
                        .as_deref()
                        .is_some_and(|o| {
                            let owner = o.trim().to_lowercase();
                            names.contains(&owner) && !self.store.topology().all_inputs.iter()
                                .filter(|other| other.base != s.base)
                                .any(|other| self.input_names(other).contains(&owner))
                        })
            })
            .collect()
    }

    fn uad2_slot(&self, slot: &InsertPaths) -> Option<SlotInfo> {
        let name = self.str_at(&slot.effect_name()).filter(|n| !n.is_empty())?;
        let device = slot.base.split('/').nth(2).and_then(|s| s.parse().ok()).unwrap_or(self.store.topology().device);
        let (label, italic) = self.plugin_label_on_device(name, device);
        Some(SlotInfo {
            state: InsertState::Loaded {
                name: name.to_owned(),
                power: self.slot_power(slot),
            },
            source: SlotSource::Uad2,
            label,
            italic,
            native: None,
        })
    }

    /// The strip's insert slots (4), UAD-2 first and native plug-ins in slots the engine leaves
    /// empty.
    pub fn slots(&self, strip: &StripInfo) -> Vec<SlotInfo> {
        let paths: &[InsertPaths] = match self.input(&strip.id) {
            Some(s) => &s.inserts,
            None => self
                .aux(&strip.id)
                .map(|a| a.inserts.as_slice())
                .unwrap_or(&[]),
        };
        let native = self.native_inserts(strip);
        (0..INSERT_SLOTS)
            .map(|k| {
                if let Some(slot) = paths.get(k).and_then(|p| self.uad2_slot(p)) {
                    return slot;
                }
                match native.iter().find(|n| n.index == Some(k)) {
                    Some(n) => SlotInfo {
                        state: InsertState::Loaded {
                            name: n.plugin_name.clone(),
                            power: n.power && n.enabled,
                        },
                        source: SlotSource::Native,
                        label: n.plugin_name.clone(),
                        italic: false,
                        native: Some(n.clone()),
                    },
                    None => SlotInfo::empty(),
                }
            })
            .collect()
    }

    /// The Unison slot as Console draws it.
    pub fn unison_slot(&self, strip: &StripInfo) -> SlotInfo {
        self.input(&strip.id)
            .and_then(|s| s.preamp.as_ref())
            .and_then(|p| self.uad2_slot(&p.unison))
            .unwrap_or_else(SlotInfo::empty)
    }
}
