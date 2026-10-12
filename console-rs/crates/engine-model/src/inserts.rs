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
        self.store.set_now(&slot.effect_name(), name.to_owned());
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
            // Loading a channel preset belongs to the presets layer (rc/presets-plugwin).
            Control::PresetDrop(_) => true,
            _ => false,
        }
    }
}
