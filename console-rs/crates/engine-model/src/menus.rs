//! Menu state and menu commands (`rack::menu`) against the engine tree. Every write goes
//! through the store's write gate like the controls' (read-only connections move nothing).

use std::sync::Arc;

use rack::menu::{Command, MenuCmd, MenuModelState, PluginOp, PluginScope, StripMenuState};
use rack::model::StripInfo;
use ua_engine::{InsertPaths, Json};

use crate::EngineMixer;

impl EngineMixer {
    pub(crate) fn surface_strip_menu_state(&self, s: &StripInfo) -> StripMenuState {
        let Some(input) = self.input(&s.id) else { return StripMenuState::default() };
        StripMenuState {
            solo_safe: self.bool_at(&input.solo_safe()).unwrap_or(false),
            linked: input.stereo_linkable.then(|| self.bool_at(&input.stereo()).unwrap_or(false)),
            hidden: self.bool_at(&input.channel_hidden()).unwrap_or(false),
            hi_z: input.preamp.as_ref().filter(|p| p.has_hi_z).and_then(|p| self.bool_at(&p.hi_z())).unwrap_or(false),
            sr_convert: s.shows_sr_convert.then(|| self.bool_at(&input.sr_convert()).unwrap_or(false)),
        }
    }

    pub(crate) fn surface_menu_state(&self) -> MenuModelState {
        let (undo, redo) = self.undo_redo_targets();
        MenuModelState {
            can_undo: undo.is_some(),
            can_redo: redo.is_some(),
            post_fader_metering: self.bool_at("/PostFaderMetering"),
            clipboard: self.clipboard.clone(),
        }
    }

    /// `/UndoRedo` values `{value, type: undo | current | redo, enabled}`: the nearest enabled
    /// undo step before the current one and the nearest redo step after it.
    fn undo_redo_targets(&self) -> (Option<Json>, Option<Json>) {
        let Some(Json::Array(list)) = self.store.meta("/UndoRedo").and_then(|m| m.values.clone()) else { return (None, None) };
        let mut undo = None;
        let mut redo = None;
        for item in &list {
            let Json::Object(o) = item else { continue };
            if o.get("enabled").and_then(Json::as_bool) == Some(false) {
                continue;
            }
            match o.get("type").and_then(Json::as_str) {
                Some("undo") => undo = o.get("value").cloned(),
                Some("redo") if redo.is_none() => redo = o.get("value").cloned(),
                _ => {}
            }
        }
        (undo, redo)
    }

    fn strip_slots(&self, strip: &str) -> Vec<InsertPaths> {
        if let Some(s) = self.input(strip) {
            return s.inserts.clone();
        }
        self.aux(strip).map(|a| a.inserts.clone()).unwrap_or_default()
    }

    fn slots_in(&self, scope: PluginScope) -> Vec<InsertPaths> {
        let t = self.store.topology();
        let mut out = Vec::new();
        if matches!(scope, PluginScope::All | PluginScope::Inserts) {
            out.extend(t.inputs.iter().flat_map(|s| s.inserts.iter().cloned()));
        }
        if matches!(scope, PluginScope::All | PluginScope::Aux) {
            out.extend(t.auxes.iter().flat_map(|a| a.inserts.iter().cloned()));
        }
        if matches!(scope, PluginScope::All | PluginScope::Unison) {
            out.extend(t.inputs.iter().filter_map(|s| s.preamp.as_ref().map(|p| p.unison.clone())));
        }
        out
    }

    fn plugin_op(&mut self, slots: &[InsertPaths], op: PluginOp) {
        for slot in slots {
            let loaded = self.str_at(&slot.effect_name()).is_some_and(|n| !n.is_empty());
            if !loaded {
                continue;
            }
            match op {
                PluginOp::Remove => self.store.commit(&slot.effect_name(), String::new()),
                PluginOp::Enable | PluginOp::Disable => {
                    let on = op == PluginOp::Enable;
                    if self.bool_at(&slot.power()) != Some(on) {
                        self.store.commit(&slot.power(), on);
                    }
                }
            }
        }
    }

    /// A model-scope `MenuCmd` on `strip`.
    pub(crate) fn apply_menu(&mut self, strip: &Arc<str>, cmd: &MenuCmd) {
        match cmd {
            MenuCmd::SoloSafe | MenuCmd::Link | MenuCmd::Hide | MenuCmd::SrConvert(_) => {
                let Some(s) = self.input(strip) else { return };
                let path = match cmd {
                    MenuCmd::SoloSafe => s.solo_safe(),
                    MenuCmd::Link if s.stereo_linkable => s.stereo(),
                    MenuCmd::Hide => s.channel_hidden(),
                    MenuCmd::SrConvert(_) => s.sr_convert(),
                    _ => return,
                };
                match cmd {
                    MenuCmd::SrConvert(on) => self.store.commit(&path, *on),
                    _ => self.toggle(&path, false),
                }
            }
            MenuCmd::PostFaderMetering(post) => self.store.commit("/PostFaderMetering", *post),
            MenuCmd::InsertPaste(k) => {
                let Some(name) = self.clipboard.clone() else { return };
                let id = rack::ControlId { strip: strip.clone(), control: rack::Control::Insert(*k) };
                if let Some(slot) = self.insert_slot(&id) {
                    self.store.commit(&slot.effect_name(), name);
                }
            }
            MenuCmd::StripInserts(op) => {
                let slots = self.strip_slots(strip);
                self.plugin_op(&slots, *op);
            }
            MenuCmd::App(c) => {
                if let Some((op, scope)) = c.plugin_op() {
                    let slots = self.slots_in(scope);
                    self.plugin_op(&slots, op);
                    return;
                }
                let (undo, redo) = self.undo_redo_targets();
                let target = match c {
                    Command::Undo => undo,
                    Command::Redo => redo,
                    _ => None,
                };
                if let Some(v) = target.and_then(|v| v.as_i64()) {
                    self.store.commit("/UndoRedo", v);
                }
            }
            // No engine path: Console clears its own meter holds (the helper has `clear_clips`).
            MenuCmd::ClearMeter | MenuCmd::ClearAllMeters => {}
            _ => {}
        }
    }
}
