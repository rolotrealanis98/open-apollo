//! Insert operations as Console 1.3.1 sends them (`Rack_Inserts` slot menu 0x18143c320,
//! `MxClnt_PlugInClipboard` 0x181703480 / 0x18170d9b0, `MoveInsert` client 0x180a97b70,
//! `Rack_Overview_Preamp::HandleButtonPopup` 0x1815215c0; engine commands in
//! `protocol/v131/MAP.md`):
//!
//! | UI | Wire |
//! |---|---|
//! | browser double click, assign mode, drop of a browser row | `set <slot>/EffectName/value "<name>"`, no undo bracket and no query (1.2.x traces; the engine records `AssignInsert` itself) |
//! | slot menu Remove, REMOVE modifier | bracketed `set <slot>/EffectName/value ""` (traces) |
//! | slot menu Copy, COPY modifier on a loaded slot | `post /CopyInsert {"effectPath": <slot>}`; the client keeps the name for `Paste "<name>"` |
//! | slot menu Paste, COPY modifier on an empty slot | `post /PasteInsert {"effectPaths": [<slot>]}` (`CMxPasteInsertAction`, undoable) |
//! | preamp menu Unison Copy / Paste Settings | `post /PluginBarCopySettings` / `/PluginBarPasteSettings {"effectPath": <unison>}`; Paste enabled by the effect's `CanPastePluginBarSettings` |
//! | drag a loaded insert onto a slot | `post /MoveInsert {"copy": <bool>, "srcPath", "dstPath"}` (`CMxMoveInsertAction`, undoable) |
//!
//! Console never sends `ShuffleInserts`, `MoveChannel`, `CopyChannel` or `PasteChannel` from its
//! mixer (Rack.dll has no Console caller for them).

use rack::input::{Control, ControlId};
use ua_engine::{InsertPaths, Json};

use crate::{EngineMixer, UNISON_SLOT};

fn native_input_root(slot: &str) -> Option<&str> {
    let (input, _) = slot.rsplit_once("/effects/")?;
    Some(input.strip_suffix("/preamps/0").unwrap_or(input))
}

impl EngineMixer {
    /// The catalogue's `/plugins/P/Unison` for the entry named `name`.
    pub(crate) fn surface_plugin_is_unison(&self, name: &str) -> Option<bool> {
        self.store
            .tree()
            .indices("/plugins")
            .into_iter()
            .find(|p| self.str_at(&format!("/plugins/{p}/Name")) == Some(name))
            .and_then(|p| self.bool_at(&format!("/plugins/{p}/Unison")))
    }

    /// Browser assign / drop: `EffectName` ← `name`, unbracketed like Console.
    pub(crate) fn assign_insert(&mut self, slot: &InsertPaths, name: &str) {
        self.native_load_notice = self.native_assignment_refusal(slot, name);
        if self.native_load_notice.is_none() {
            self.store.clear_error();
            self.store.set_now(&slot.effect_name(), name.to_owned());
        }
    }

    /// An advertised native host is a restricted loader, not the entire licensed catalogue.
    /// Older engines omit this extension and retain their original assignment behavior.
    pub(crate) fn native_assignment_refusal(&self, slot: &InsertPaths, name: &str) -> Option<String> {
        if name.is_empty() || self.bool_at("/NativePluginHosting") != Some(true) {
            return None;
        }
        let plugin = self.store.tree().indices("/plugins").into_iter()
            .find(|p| self.str_at(&format!("/plugins/{p}/Name")) == Some(name));
        let Some(p) = plugin else { return Some("No native DSP loader for this plug-in.".into()) };
        let base = format!("/plugins/{p}");
        if self.bool_at(&format!("{base}/NativeLoadSupported")) != Some(true) {
            return Some("No native DSP loader for this plug-in.".into());
        }
        let mono = self.str_at(&format!("{base}/NativeMonoSlots"));
        let stereo = self.str_at(&format!("{base}/NativeStereoSlots"));
        let layout_advertised = mono.is_some() || stereo.is_some();
        let input = native_input_root(&slot.base);
        let linked = input.is_some_and(|path| self.bool_at(&format!("{path}/Stereo")) == Some(true));
        if layout_advertised && linked && input.and_then(|path| path.rsplit('/').next())
            .and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| n % 2 == 1)
        {
            return Some("Assign the stereo insert to the first channel of the linked pair.".into());
        }
        let slots = if layout_advertised {
            if linked { stereo.unwrap_or("") } else { mono.unwrap_or("") }
        } else {
            self.str_at(&format!("{base}/NativeLoadSlots")).unwrap_or("")
        };
        let here = |list: &str| list.split(',').any(|s| s == slot.base);
        if !here(slots) {
            return Some(if layout_advertised {
                // The bundle's other layout: link or unlink the input, or pick another bundle.
                let (want, other, other_slots, verb) = if linked {
                    ("stereo", "mono", mono.unwrap_or(""), "unlink")
                } else {
                    ("mono", "stereo", stereo.unwrap_or(""), "link")
                };
                if slots.is_empty() && !other_slots.is_empty() {
                    format!("{name}: {other} only — {verb} the input or use a {want} bundle.")
                } else if here(other_slots) {
                    format!("{name}: no native {want} loader on this input — {verb} it to load the {other} bundle.")
                } else {
                    format!("{name}: no native {want} loader for this slot.")
                }
            } else {
                self.str_at(&format!("{base}/NativeLoadHint")).unwrap_or("This slot is not supported by the native loader.").into()
            });
        }
        let chain_limit = self.store.value("/NativeMaxInserts").and_then(|v| v.as_i64())
            .filter(|n| (1..=4).contains(n));
        let chain_flag = self.bool_at("/NativeChainSupported");
        let chains = chain_flag == Some(true) && chain_limit.is_some();
        if chain_flag.is_some() || self.store.value("/NativeMaxInserts").is_some() {
            let limit = if chains { chain_limit.unwrap() as usize } else { 1 };
            if slot.index >= limit {
                return Some(format!("The native loader supports {limit} {} per input chain.", if limit == 1 { "insert" } else { "inserts" }));
            }
        }
        let required = self.store.value(&format!("{base}/NativeSampleRate"));
        if required.is_some() && required != self.store.value("/SampleRate") {
            return Some("Sample rate differs from the native loader; restart with the supported rate.".into());
        }
        let maximum = self.store.value("/NativeMaxInstances").and_then(|v| v.as_i64())
            .filter(|n| *n > 0).unwrap_or(1) as usize;
        let replacement = self.bool_at("/NativeReplacementSupported") == Some(true);
        // A slot appears in several catalogue entries; count each occupied path once.
        let mut occupied = std::collections::BTreeSet::new();
        for p in self.store.tree().indices("/plugins") {
            for field in ["NativeLoadSlots", "NativeMonoSlots", "NativeStereoSlots"] {
                let paths = self.str_at(&format!("/plugins/{p}/{field}")).unwrap_or("");
                for path in paths.split(',').filter(|p| !p.is_empty()) {
                    if self.str_at(&format!("{path}/EffectName")).is_some_and(|n| !n.is_empty()) {
                        occupied.insert(path.to_owned());
                    }
                }
            }
        }
        if !replacement && occupied.contains(&slot.base) {
            return Some("Remove the current native insert before loading another.".into());
        }
        let channels = self.native_slot_channels(&slot.base);
        for path in occupied.iter().filter(|path| **path != slot.base) {
            let same_chain = chains && native_input_root(path) == input;
            if !same_chain && self.native_slot_channels(path).iter().any(|channel| channels.contains(channel)) {
                return Some("Another native insert already uses this input or linked pair.".into());
            }
        }
        // A chain hosts several logical effects but consumes one physical instance.
        // Keep the historical slot count until both chain capabilities are advertised.
        let owner = |path: &str| -> String {
            if chains { native_input_root(path).unwrap_or(path).into() }
            else { path.into() }
        };
        let owners: std::collections::BTreeSet<_> = occupied.iter().map(|path| owner(path)).collect();
        let other_instances = owners.len() - usize::from(owners.contains(&owner(&slot.base)));
        if other_instances >= maximum {
            return Some(if chains {
                format!("The native loader supports {maximum} simultaneous input {}; remove one before loading another.", if maximum == 1 { "chain" } else { "chains" })
            } else if maximum == 1 {
                "Remove the current native insert before loading another.".into()
            } else {
                format!("The native loader supports {maximum} simultaneous inserts; remove one before loading another.")
            });
        }
        None
    }

    /// Physical channels owned by a currently advertised input-slot layout.
    fn native_slot_channels(&self, slot: &str) -> Vec<String> {
        let Some(input) = native_input_root(slot) else { return vec![slot.into()] };
        let Some((bank, index)) = input.rsplit_once('/') else { return vec![input.into()] };
        let Ok(index) = index.parse::<u32>() else { return vec![input.into()] };
        if self.bool_at(&format!("{input}/Stereo")) == Some(true) {
            let first = index & !1;
            vec![format!("{bank}/{first}"), format!("{bank}/{}", first + 1)]
        } else {
            vec![input.into()]
        }
    }

    /// Slot Copy: the engine keeps the plug-in and its settings; we keep its name.
    pub(crate) fn copy_insert(&mut self, slot: &InsertPaths, name: String) {
        let args: Json = [("effectPath", Json::String(slot.base.clone()))].into_iter().collect();
        self.store.post("/CopyInsert", Some(&args));
        self.clipboard = Some(name);
    }

    /// Whether the copied plug-in can be pasted into `slot_index` (`MxClnt_PlugInClipboard`
    /// 0x181702160): something was copied, and a Unison slot only takes Unison plug-ins.
    pub(crate) fn can_paste(&self, slot_index: u8) -> bool {
        match &self.clipboard {
            None => false,
            Some(name) => slot_index != UNISON_SLOT || self.surface_plugin_is_unison(name) != Some(false),
        }
    }

    /// Slot Paste: `post /PasteInsert {"effectPaths": [slot]}`.
    pub(crate) fn paste_insert(&mut self, slot: &InsertPaths, slot_index: u8) {
        if !self.can_paste(slot_index) {
            return;
        }
        let args: Json = [("effectPaths", Json::Array(vec![Json::String(slot.base.clone())]))].into_iter().collect();
        self.store.post("/PasteInsert", Some(&args));
    }

    /// Unison Copy / Paste Settings (the plug-in bar's settings clipboard, engine-side).
    pub(crate) fn plugin_bar_settings(&mut self, slot: &InsertPaths, paste: bool) {
        let args: Json = [("effectPath", Json::String(slot.base.clone()))].into_iter().collect();
        self.store.post(if paste { "/PluginBarPasteSettings" } else { "/PluginBarCopySettings" }, Some(&args));
    }

    /// Drag of a loaded insert onto another slot.
    pub(crate) fn move_insert(&mut self, src: &InsertPaths, dst: &InsertPaths, copy: bool) {
        if src.base == dst.base {
            return;
        }
        // A refusal shown now is this move's (the slot's drop notice reads it).
        self.native_load_notice = None;
        self.store.clear_error();
        let args: Json = [("copy", Json::Bool(copy)), ("dstPath", Json::String(dst.base.clone())), ("srcPath", Json::String(src.base.clone()))].into_iter().collect();
        self.store.post("/MoveInsert", Some(&args));
    }

    /// `Control::InsertMove` / `Control::PresetDrop` taps from `rack::dnd`.
    pub(crate) fn apply_drop(&mut self, id: &ControlId) -> bool {
        match &id.control {
            Control::InsertMove { slot, from, from_slot, copy } => {
                let src = self.insert_slot(&ControlId { strip: from.clone(), control: Control::Insert(*from_slot) });
                let dst = self.insert_slot(&ControlId { strip: id.strip.clone(), control: Control::Insert(*slot) });
                if let (Some(s), Some(d)) = (src, dst) {
                    self.move_insert(&s, &d, *copy);
                }
                true
            }
            // A channel preset dropped on a strip loads there, as a tap in a targeted PRESETS
            // tab does: `set <strip>/Preset/value "[folder/]name"`.
            Control::PresetDrop(path) => {
                let load = ControlId { strip: id.strip.clone(), control: Control::Preset(rack::presets::PresetScope::Channel) };
                self.preset_set_text(&load, path);
                true
            }
            _ => false,
        }
    }
}
