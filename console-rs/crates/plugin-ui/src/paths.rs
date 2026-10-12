//! UA Mixer Engine paths for a plug-in slot and its parameters (verified read-only against engine
//! 12.0.0 by ConsoleLab). The engine adapter implementing [`crate::ParamSource`] uses these.

use std::fmt;

/// A plug-in slot node: an input insert `/devices/D/inputs/N/effects/K`, an aux insert
/// `/devices/D/auxs/N/effects/K`, or the Unison slot `/devices/D/inputs/N/preamps/0/effects/0`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SlotPath(pub String);

impl SlotPath {
    pub fn input_insert(device: u32, input: u32, k: u32) -> Self {
        Self(format!("/devices/{device}/inputs/{input}/effects/{k}"))
    }
    pub fn aux_insert(device: u32, aux: u32, k: u32) -> Self {
        Self(format!("/devices/{device}/auxs/{aux}/effects/{k}"))
    }
    pub fn unison(device: u32, input: u32) -> Self {
        Self(format!("/devices/{device}/inputs/{input}/preamps/0/effects/0"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_unison(&self) -> bool {
        self.0.contains("/preamps/")
    }
    /// Insert index (last path component); 0 for Unison.
    pub fn index(&self) -> u32 {
        self.0.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    pub fn effect_name(&self) -> String {
        format!("{}/EffectName", self.0)
    }
    pub fn power(&self) -> String {
        format!("{}/Power", self.0)
    }
    pub fn preset(&self) -> String {
        format!("{}/Preset", self.0)
    }
    /// Present only while a plug-in is loaded.
    pub fn parameters(&self) -> String {
        format!("{}/parameters", self.0)
    }
    pub fn vendor_specific(&self) -> String {
        format!("{}/VendorSpecific", self.0)
    }
    pub fn param(&self, k: u16) -> ParamPaths {
        ParamPaths { base: format!("{}/parameters/{k}", self.0) }
    }
}

impl fmt::Display for SlotPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `<slot>/parameters/<k>/…`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParamPaths {
    pub base: String,
}

impl ParamPaths {
    /// float 0…1; written as `set <this>/value v`.
    pub fn normalized(&self) -> String {
        format!("{}/NormalizedValue", self.base)
    }
    /// Display text ("2.80", "Off"), pushed by the engine.
    pub fn string(&self) -> String {
        format!("{}/StringValue", self.base)
    }
    /// Step label with a `values` list; only on stepped parameters.
    pub fn step(&self) -> String {
        format!("{}/StepValue", self.base)
    }
    pub fn name(&self) -> String {
        format!("{}/Name", self.base)
    }
    /// What Console subscribes per parameter while the window is open.
    pub fn subscribed(&self) -> [String; 3] {
        [self.normalized(), self.string(), self.step()]
    }
}

/// Global undo bracket: `set /UndoRecording/value true` … `false` around a gesture.
pub const UNDO_RECORDING: &str = "/UndoRecording";
