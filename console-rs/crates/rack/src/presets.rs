//! Channel-strip and plug-in presets, the way UAD Console 1.3.1 handles them (UA Mixer Engine
//! 12.0.0, `protocol/v131/MAP.md` and the engine decompile):
//!
//! * **Lists.** A strip's `Preset` property carries the Channel Strip preset tree as `values`
//!   (`CMxBase::GetChannelStripPresetInfo`: folders and `.uadchannel` files under the Channel
//!   Strip preset folder, factory entries `locked`). A loaded insert's `Preset` does the same
//!   for its plug-in's `.fxp` presets.
//! * **Load.** `set <strip>/Preset/value "[folder/]name"` (`CMxBase::SetChannelStripPreset`
//!   loads `<folder>/<value>.uadchannel`) or `set <effect>/Preset/value "[folder/]name"`
//!   (`CMxEffect::setPreset`, `.fxp`). The engine records it as `CMxAssignPreset`, an undo step.
//! * **Files.** `post /SaveAsPreset {path, pluginName, presetName}`,
//!   `/MovePreset {isFolder, origPresetName, newPresetName, pluginName}` (rename),
//!   `/DeletePreset {pluginName, presetName}`, `/NewFolder {folder, path: "<object>/Preset"}`.
//!   `pluginName` is `"Channel Strip"` or the plug-in's catalogue name.
//! * **Plug-in bar.** `post /PluginBarCopySettings {effectPath}` and
//!   `/PluginBarPasteSettings {effectPath}`; Paste is enabled by the effect's
//!   `CanPastePluginBarSettings`.
//!
//! The browser's PRESETS tab lists the tree; "Channel Presets..." / "Presets..." target it at a
//! strip or insert (Rack's `channel_preset_assign_browser`), where a tap loads the preset and
//! the operations row saves, renames or deletes.

use crate::model::PresetNode;

/// What a preset applies to: the strip itself (Channel Strip presets) or one insert slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresetScope {
    Channel,
    /// An insert slot (`255` = the Unison slot).
    Insert(u8),
}

/// A preset file operation on the browser's target (`ControlId::strip` = the strip).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PresetCmd {
    /// SAVE AS...: `set_text` carries `[folder/]name`.
    SaveAs,
    /// SAVE over the current preset (`SaveAsPreset` with its name).
    Save,
    /// RENAME the `[folder/]name` preset: `set_text` carries the new name (same folder).
    Rename(std::sync::Arc<str>),
    /// DELETE the `[folder/]name` preset.
    Delete(std::sync::Arc<str>),
    /// NEW FOLDER: `set_text` carries the folder name.
    NewFolder,
    /// The plug-in window's COPY / PASTE (plug-in bar settings), inserts only.
    CopySettings,
    PasteSettings,
}

impl PresetCmd {
    /// Needs a name typed first (opens a text field).
    pub fn needs_text(&self) -> bool {
        matches!(self, PresetCmd::SaveAs | PresetCmd::Rename(_) | PresetCmd::NewFolder)
    }
}

/// A row of the PRESETS list: a top-level node, or a child of an open top-level folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresetRow {
    pub top: u16,
    /// `None` for the top-level node itself.
    pub child: Option<u16>,
}

impl PresetRow {
    /// The `(top, child)` pair a hit carries (`u16::MAX` = no child).
    pub fn encode(self) -> (u16, u16) {
        (self.top, self.child.unwrap_or(u16::MAX))
    }

    pub fn decode(top: u16, child: u16) -> Self {
        Self { top, child: (child != u16::MAX).then_some(child) }
    }

    pub fn node<'a>(&self, tree: &'a [PresetNode]) -> Option<&'a PresetNode> {
        let top = tree.get(self.top as usize)?;
        match self.child {
            None => Some(top),
            Some(c) => top.children.as_ref()?.get(c as usize),
        }
    }

    /// The engine's `[folder/]name` for this row's file (None for a folder).
    pub fn value(&self, tree: &[PresetNode]) -> Option<String> {
        let node = self.node(tree)?;
        if node.children.is_some() {
            return None;
        }
        Some(match self.child {
            None => node.name.clone(),
            Some(_) => format!("{}/{}", tree[self.top as usize].name, node.name),
        })
    }
}

/// Every file in the tree as `[folder/]name`, in list order (the plug-in window's ‹ › step
/// through this).
pub fn flatten(tree: &[PresetNode]) -> Vec<String> {
    let mut out = Vec::new();
    for top in tree {
        match &top.children {
            None => out.push(top.name.clone()),
            Some(kids) => {
                for k in kids {
                    if k.children.is_none() {
                        out.push(format!("{}/{}", top.name, k.name));
                    }
                }
            }
        }
    }
    out
}

/// The tree filtered by a search string (case-insensitive substring of the file name; folders
/// keep their matching files and drop when none match).
pub fn filter(tree: &[PresetNode], search: &str) -> Vec<PresetNode> {
    let q = search.trim().to_lowercase();
    if q.is_empty() {
        return tree.to_vec();
    }
    tree.iter()
        .filter_map(|n| match &n.children {
            None => n.name.to_lowercase().contains(&q).then(|| n.clone()),
            Some(kids) => {
                let kept: Vec<_> = kids.iter().filter(|k| k.name.to_lowercase().contains(&q)).cloned().collect();
                (!kept.is_empty() || n.name.to_lowercase().contains(&q)).then(|| PresetNode { children: Some(if kept.is_empty() { kids.clone() } else { kept }), ..n.clone() })
            }
        })
        .collect()
}

/// Steps from `current` (a `[folder/]name`, or empty) by `delta` through `list`, wrapping like
/// Console's preset arrows. None for an empty list.
pub fn step(list: &[String], current: &str, delta: i32) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let n = list.len() as i32;
    let current = current.trim_start_matches('/');
    let i = match list.iter().position(|p| p == current) {
        Some(i) => (i as i32 + delta).rem_euclid(n),
        None if delta < 0 => n - 1,
        None => 0,
    };
    list.get(i as usize).cloned()
}

/// A rename keeps the preset's folder: `Drums/Kick` renamed `Big Kick` → `Drums/Big Kick`.
pub fn renamed(orig: &str, new_name: &str) -> String {
    let new_name = new_name.trim().trim_matches('/');
    match orig.rsplit_once('/') {
        Some((folder, _)) if !new_name.contains('/') => format!("{folder}/{new_name}"),
        _ => new_name.to_string(),
    }
}

// MARK: the PRESETS tab's behaviour (ViewState)

use std::sync::Arc;

use crate::input::{Action, Control, ControlId};
use crate::model::MixerModel;
use crate::surfaces::{BrowserTab, Effect, TextEdit, TextTarget, ViewState};

fn find_strip(model: &dyn MixerModel, id: &str) -> Option<crate::model::StripInfo> {
    model.inputs().into_iter().chain(model.auxes()).chain(model.talkback()).find(|s| &*s.id == id)
}

impl ViewState {
    /// "Channel Presets..." (strip label, strip section of the insert menu) or an insert's
    /// "Presets...": the browser's PRESETS tab, loading into and saving from that target.
    pub fn begin_preset_assign(&mut self, strip: Arc<str>, scope: PresetScope) {
        self.show_browser = true;
        self.compact = false;
        self.browser.assign_to = None;
        self.browser.tab = BrowserTab::Presets;
        self.browser.preset_target = Some((strip, scope));
        self.browser.preset_selected = None;
        self.browser.presets_scroll = 0.0;
    }

    /// The tree the PRESETS tab lists: the target insert's plug-in presets, else the Channel
    /// Strip presets; filtered by the search field.
    pub fn preset_tree(&self, model: &dyn MixerModel) -> Vec<PresetNode> {
        let tree = match &self.browser.preset_target {
            Some((strip, PresetScope::Insert(slot))) => find_strip(model, strip).map(|s| model.plugin_presets(&s, *slot)).unwrap_or_default(),
            _ => model.channel_presets(),
        };
        filter(&tree, &self.browser.search)
    }

    /// A file row: selects it and, with a target, loads it there. The target stays, so SAVE AS
    /// and RENAME keep working on it (Console's assign browser stays open until closed).
    pub(crate) fn press_preset_file(&mut self, top: u16, child: u16, model: &dyn MixerModel) -> Vec<Effect> {
        let row = PresetRow::decode(top, child);
        let tree = self.preset_tree(model);
        let Some(value) = row.value(&tree) else { return Vec::new() };
        self.browser.preset_selected = Some((top, child));
        match &self.browser.preset_target {
            Some((strip, scope)) => vec![Effect::SetText(ControlId { strip: strip.clone(), control: Control::Preset(*scope) }, value)],
            // Without a target Console assigns by drag (`preset_drop`); a tap selects.
            None => Vec::new(),
        }
    }

    /// The operations row (DELETE / RENAME / SAVE AS... / NEW FOLDER): text-taking commands open
    /// a name field; the rest reach the model.
    pub(crate) fn press_preset_cmd(&mut self, strip: &Arc<str>, scope: PresetScope, cmd: PresetCmd, _model: &dyn MixerModel) -> Vec<Effect> {
        if cmd.needs_text() {
            let text = match &cmd {
                PresetCmd::Rename(orig) => orig.rsplit('/').next().unwrap_or(orig).to_string(),
                _ => String::new(),
            };
            self.text_edit = Some(TextEdit { target: TextTarget::Preset(strip.clone(), scope, cmd), text });
            return Vec::new();
        }
        if matches!(cmd, PresetCmd::Delete(_)) {
            self.browser.preset_selected = None;
        }
        vec![Effect::Model(Action::Tap(ControlId { strip: strip.clone(), control: Control::PresetCmd(scope, cmd) }))]
    }

    pub(crate) fn commit_preset_text(&mut self, strip: Arc<str>, scope: PresetScope, cmd: PresetCmd, text: String) -> Vec<Effect> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Vec::new();
        }
        vec![Effect::SetText(ControlId { strip, control: Control::PresetCmd(scope, cmd) }, text)]
    }

    /// For a drop of a PRESETS row onto a strip (drag and drop): the load it means there.
    pub fn preset_drop(&self, top: u16, child: u16, strip: Arc<str>, model: &dyn MixerModel) -> Option<Effect> {
        let scope = match &self.browser.preset_target {
            Some((_, s @ PresetScope::Insert(_))) => *s,
            _ => PresetScope::Channel,
        };
        let value = PresetRow::decode(top, child).value(&self.preset_tree(model))?;
        Some(Effect::SetText(ControlId { strip, control: Control::Preset(scope) }, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Vec<PresetNode> {
        vec![
            PresetNode::folder("Bass", vec![PresetNode::file("A"), PresetNode::file("B")]),
            PresetNode::file("Loose"),
            PresetNode::folder("Drums", vec![PresetNode::file("Kick")]),
        ]
    }

    #[test]
    fn rows_and_values() {
        let t = tree();
        assert_eq!(PresetRow::decode(0, 1).value(&t).as_deref(), Some("Bass/B"));
        assert_eq!(PresetRow::decode(1, u16::MAX).value(&t).as_deref(), Some("Loose"));
        assert_eq!(PresetRow::decode(0, u16::MAX).value(&t), None, "a folder is not a preset");
        assert_eq!(flatten(&t), ["Bass/A", "Bass/B", "Loose", "Drums/Kick"]);
    }

    #[test]
    fn stepping_wraps() {
        let l = flatten(&tree());
        assert_eq!(step(&l, "Bass/B", 1).as_deref(), Some("Loose"));
        assert_eq!(step(&l, "Drums/Kick", 1).as_deref(), Some("Bass/A"));
        assert_eq!(step(&l, "/Bass/A", -1).as_deref(), Some("Drums/Kick"), "a leading / is ignored, as setPreset does");
        assert_eq!(step(&l, "", 1).as_deref(), Some("Bass/A"));
        assert_eq!(step(&l, "", -1).as_deref(), Some("Drums/Kick"));
        assert_eq!(step(&[], "", 1), None);
    }

    #[test]
    fn search_and_rename() {
        let f = filter(&tree(), "kick");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].children.as_ref().unwrap().len(), 1);
        assert_eq!(renamed("Drums/Kick", "Big Kick"), "Drums/Big Kick");
        assert_eq!(renamed("Loose", "Tight"), "Tight");
    }
}
