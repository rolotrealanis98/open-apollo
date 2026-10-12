//! ConsoleLink's generated plug-in panel spec (`PluginSpec_*.json`, `schema` 1).
//!
//! Specs are produced by ConsoleLink's `tools/plugin-specs` from UA's LUNA GUI bundle (geometry,
//! sprites, frame counts, click behaviour) joined with a UA Console parameter capture (indices,
//! names, steps, defaults). They are read at runtime and never bundled. Every value is a Console
//! normalized value (0…1) indexed by the plug-in's parameter ordinal, which is the engine's
//! `<slot>/parameters/<k>`. Port of ConsoleLab's `PluginSpec.swift`; unknown keys are ignored.

use std::collections::{BTreeMap, BTreeSet};

use houston::Rect;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Parameter {
    pub index: u16,
    pub name: String,
    #[serde(default)]
    pub steps: Option<Vec<String>>,
    #[serde(default, rename = "default")]
    pub default_value: Option<f64>,
}

/// One row of a lookup table keyed by the normalized values of the table's `params`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub key: Vec<f64>,
    /// Frame index. A few generated tables (UA 175-B, UA 176) write two-frame lamps as
    /// `true`/`false`; those decode as 1/0, as ConsoleLab does.
    #[serde(default, deserialize_with = "lenient_frame")]
    pub frame: Option<i32>,
    #[serde(default)]
    pub frame_normalized: Option<f64>,
    #[serde(default)]
    pub opacity: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FrameValue {
    Int(i64),
    Float(f64),
    Bool(bool),
}

fn lenient_frame<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i32>, D::Error> {
    Ok(Option::<FrameValue>::deserialize(d)?.map(|v| match v {
        FrameValue::Int(i) => i as i32,
        FrameValue::Float(f) => f.round() as i32,
        FrameValue::Bool(b) => b as i32,
    }))
}

impl Row {
    /// Nearest row to `key` (L1 distance); every lookup table uses it.
    pub fn nearest<'a>(rows: &'a [Row], key: &[f64]) -> Option<&'a Row> {
        let dist = |r: &Row| r.key.iter().zip(key).map(|(a, b)| (a - b).abs()).sum::<f64>();
        rows.iter().min_by(|a, b| dist(a).total_cmp(&dist(b)))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FrameSourceKind {
    Param,
    Table,
    Meter,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct FrameSource {
    #[serde(rename = "type")]
    pub kind: FrameSourceKind,
    #[serde(default)]
    pub param: Option<u16>,
    #[serde(default)]
    pub params: Option<Vec<u16>>,
    #[serde(default)]
    pub table: Option<Vec<Row>>,
    /// LUNA meter id, a key of [`PluginSpec::meter_sources`].
    #[serde(default)]
    pub meter: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Visibility {
    pub params: Vec<u16>,
    pub table: Vec<Row>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct ActionRow {
    pub current: f64,
    pub value: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActionKind {
    Set,
    Nudge,
    Toggle,
    Default,
    Table,
    Multi,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Action {
    #[serde(rename = "type")]
    pub kind: ActionKind,
    #[serde(default)]
    pub param: Option<u16>,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub delta: Option<f64>,
    #[serde(default)]
    pub table: Option<Vec<ActionRow>>,
    /// `multi`: every action in order (e.g. a jack that sets input and level).
    #[serde(default)]
    pub actions: Option<Vec<Action>>,
}

impl Action {
    pub fn new(kind: ActionKind, param: u16) -> Self {
        Self { kind, param: Some(param), value: None, delta: None, table: None, actions: None }
    }

    /// Parameters this action (and its sub-actions) writes.
    pub fn params(&self, out: &mut BTreeSet<u16>) {
        if let Some(p) = self.param {
            out.insert(p);
        }
        for a in self.actions.iter().flatten() {
            a.params(out);
        }
    }
}

/// Value readout style (`text` and `menu` elements).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TextStyle {
    #[serde(default)]
    pub size: Option<f64>,
    /// `#RRGGBB` or `#RRGGBBAA`.
    #[serde(default)]
    pub color: Option<String>,
    /// `left` | `center` | `right`.
    #[serde(default)]
    pub align: Option<String>,
    /// `regular` | `medium` | `bold`.
    #[serde(default)]
    pub weight: Option<String>,
    #[serde(default)]
    pub monospaced: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ElementKind {
    Knob,
    Switch,
    Button,
    Filmstrip,
    Image,
    Zone,
    Text,
    Menu,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    pub id: String,
    pub kind: ElementKind,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub w: Option<f32>,
    #[serde(default)]
    pub h: Option<f32>,
    /// Sprite name: `Images/<sprite>@2x.png` (or `<sprite>.png`), a vertical strip of `frames`.
    #[serde(default)]
    pub sprite: Option<String>,
    #[serde(default)]
    pub frames: Option<u32>,
    #[serde(default)]
    pub param: Option<u16>,
    /// `vertical` | `horizontal` (and `-reverse` variants).
    #[serde(default)]
    pub orientation: Option<String>,
    #[serde(default)]
    pub reverse: Option<bool>,
    #[serde(default)]
    pub opacity: Option<f64>,
    /// `drag` on zones laid over a filmstrip.
    #[serde(default)]
    pub gesture: Option<String>,
    #[serde(default)]
    pub frame_source: Option<FrameSource>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub action: Option<Action>,
    /// Knobs: `horizontal` drag (faders); vertical by default.
    #[serde(default)]
    pub drag: Option<String>,
    #[serde(default)]
    pub text: Option<TextStyle>,
}

impl Element {
    /// Rect in panel points.
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w.unwrap_or(0.0), self.h.unwrap_or(0.0))
    }

    pub fn frame_count(&self) -> u32 {
        self.frames.unwrap_or(1).max(1)
    }

    pub fn is_horizontal(&self) -> bool {
        self.orientation.as_deref().is_some_and(|o| o.starts_with("horizontal"))
    }
}

/// How a LUNA meter id is fed from UA Console (`VendorSpecific` reads).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeterSource {
    /// Native plug-in variable id: `<slot>/VendorSpecific/value?floats=ID`.
    pub vendor_specific_float: i64,
    /// `(raw, needle 0…1)` points, raw ascending.
    #[serde(default)]
    pub calibration: Vec<Vec<f64>>,
    #[serde(default)]
    pub smoothing: Option<f64>,
    /// Tagged ids (`tag << 24 | index`) are read with `?opcode=519&p0=ID&p1=count&p2=1`.
    #[serde(default)]
    pub opcode: Option<Opcode>,
    /// Optional per-mode curves selected by a parameter (a METER switch).
    #[serde(default)]
    pub mode_param: Option<u16>,
    #[serde(default)]
    pub mode_calibrations: Option<Vec<ModeCalibration>>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub struct Opcode {
    pub op: i64,
    pub p0: i64,
    pub p1: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ModeCalibration {
    /// Normalized value of `mode_param` this curve applies to.
    pub value: f64,
    pub calibration: Vec<Vec<f64>>,
}

impl MeterSource {
    /// Piecewise-linear needle position for a raw value (clamped to the curve's ends).
    pub fn calibrate(raw: f64, points: &[Vec<f64>]) -> f64 {
        let pts: Vec<&Vec<f64>> = points.iter().filter(|p| p.len() >= 2).collect();
        let (Some(first), Some(last)) = (pts.first(), pts.last()) else { return 0.0 };
        if raw <= first[0] {
            return first[1];
        }
        if raw >= last[0] {
            return last[1];
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if raw >= a[0] && raw <= b[0] {
                let t = if b[0] == a[0] { 0.0 } else { (raw - a[0]) / (b[0] - a[0]) };
                return (a[1] + t * (b[1] - a[1])).clamp(0.0, 1.0);
            }
        }
        0.0
    }

    /// The curve for the current mode value (an exact `mode_calibrations` entry), else the base.
    pub fn curve(&self, mode: Option<f64>) -> &[Vec<f64>] {
        if let (Some(mode), Some(modes)) = (mode, &self.mode_calibrations) {
            if let Some(m) = modes.iter().min_by(|a, b| (a.value - mode).abs().total_cmp(&(b.value - mode).abs())) {
                if (m.value - mode).abs() < 0.01 {
                    return &m.calibration;
                }
            }
        }
        &self.calibration
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSpec {
    pub schema: i64,
    /// Engine `EffectName`, e.g. "UAD dbx 160".
    pub effect_name: String,
    /// Substring matched against `EffectName` when there is no exact match.
    pub name_pattern: String,
    /// Panel size in points (sprites are @2x).
    pub size: Size,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    /// In z-order (first is drawn first).
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default)]
    pub meter_sources: Option<BTreeMap<String, MeterSource>>,
    #[serde(default)]
    pub todo: Option<Vec<String>>,
}

impl PluginSpec {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parameter(&self, index: u16) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.index == index)
    }

    /// Spec step labels for a parameter (only when it has more than one).
    pub fn step_labels(&self, index: u16) -> Option<&[String]> {
        self.parameter(index).and_then(|p| p.steps.as_deref()).filter(|s| s.len() > 1)
    }

    /// Sprite names the spec draws (background included).
    pub fn sprite_names(&self) -> BTreeSet<&str> {
        let mut names: BTreeSet<&str> = self.elements.iter().filter_map(|e| e.sprite.as_deref()).collect();
        if let Some(bg) = &self.background {
            names.insert(bg);
        }
        names
    }

    /// Parameters no panel element can change (offer them in the parameter list).
    pub fn unbound_parameters(&self) -> Vec<u16> {
        let mut bound = BTreeSet::new();
        for el in &self.elements {
            if matches!(el.kind, ElementKind::Knob | ElementKind::Switch | ElementKind::Button | ElementKind::Zone | ElementKind::Menu) {
                if let Some(p) = el.param {
                    bound.insert(p);
                }
                if let Some(a) = &el.action {
                    a.params(&mut bound);
                }
            }
        }
        self.parameters.iter().map(|p| p.index).filter(|i| !bound.contains(i)).collect()
    }

    fn meters(&self) -> impl Iterator<Item = (&String, &MeterSource)> {
        self.meter_sources.iter().flatten()
    }

    /// One opcode request covering the opcode-capable meter sources: the largest group sharing
    /// an opcode and tag, as a contiguous range `(op, p0, count)` (as ConsoleLink polls it).
    pub fn opcode_range(&self) -> Option<(i64, i64, i64)> {
        let mut groups: BTreeMap<(i64, i64), Vec<Opcode>> = BTreeMap::new();
        for (_, m) in self.meters() {
            if let Some(o) = m.opcode {
                groups.entry((o.op, o.p0 >> 24)).or_default().push(o);
            }
        }
        let group = groups.into_values().max_by_key(|g| g.len())?;
        let lo = group.iter().map(|o| o.p0).min()?;
        let hi = group.iter().map(|o| o.p0 + o.p1.max(1)).max()?;
        Some((group[0].op, lo, hi - lo))
    }

    /// True when the source is served by [`opcode_range`](Self::opcode_range) rather than `floats=`.
    pub fn uses_opcode(&self, source: &MeterSource) -> bool {
        match (source.opcode, self.opcode_range()) {
            (Some(o), Some((op, p0, n))) => o.op == op && o.p0 >= p0 && o.p0 < p0 + n,
            _ => false,
        }
    }

    /// Float ids to poll with `floats=`, ascending (the order the reply lists them).
    pub fn poll_floats(&self) -> Vec<i64> {
        let ids: BTreeSet<i64> = self.meters().filter(|(_, m)| !self.uses_opcode(m)).map(|(_, m)| m.vendor_specific_float).collect();
        ids.into_iter().collect()
    }
}
