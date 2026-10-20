//! ConsoleLink's generated plug-in panel spec (`PluginSpec_*.json`, `schema` 1 or 2).
//!
//! Schema 2 is the release format (RUNTIME-V2.md): sprite fields hold recipe ids whose art the
//! importer regenerates from the user's own install, withheld texts are `stringRef` / `stepsRef`
//! ids, and `toolbar` / `label` / `shape` / `lamp` elements are drawn by our code from facts.
//!
//! Specs are produced by the plugin-spec pipeline from UA's LUNA GUI bundles, the Powered DLLs and
//! live editor captures, joined with a UA Console parameter capture (indices, names, steps,
//! defaults). They are read at runtime and never bundled. Every value is a Console normalized value
//! (0…1) indexed by the plug-in's parameter ordinal, which is the engine's `<slot>/parameters/<k>`.
//!
//! The types mirror the validated Swift runtime (`PluginSpec.swift`) field for field, including the
//! "Runtime additions" of the pipeline contract (`PIPELINE.md`): local UI states, touch states,
//! sprite-font and computed readouts, held/blink/segment looks, long-press/tap/repeat/swipe
//! gestures, snapshots, reveal/move/rotate/spin, graphs (EQ, polar, transfer, spectrum,
//! polylines, ranges, peak marker) and the GraphValue expression language. Every type is also
//! `Serialize`, so a decode → encode round trip shows which keys a spec carries that the runtime
//! does not read (`tests/coverage.rs`). Unknown keys (`provenance`, `notes`, `_images`, …) are
//! ignored, as Swift's `Decodable` does.
//!
//! Deliberate differences from Swift's decoder (Swift would refuse the whole spec):
//! - a table row's `frame` may be `true`/`false` (two UA 175-B/176 lamps) and decodes as 1/0;
//! - `parameters`, `elements` and a meter source's `calibration` may be absent (empty).

use std::collections::{BTreeMap, BTreeSet};

use houston::Rect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Parameter {
    pub index: u16,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<String>>,
    #[serde(default, rename = "default", skip_serializing_if = "Option::is_none")]
    pub default_value: Option<f64>,
    /// Schema 2: the number of steps when the labels are withheld (`steps: null`).
    #[serde(default, rename = "stepCount", skip_serializing_if = "Option::is_none")]
    pub step_count: Option<u32>,
    /// Schema 2: a texts-recipe id (`dll-string-table`) holding the withheld step labels.
    #[serde(default, rename = "stepsRef", skip_serializing_if = "Option::is_none")]
    pub steps_ref: Option<String>,
}

/// One row of a lookup table keyed by the values of the table's `params`, then `states`, then
/// `meters` (L1-nearest row wins).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub key: Vec<f64>,
    /// Frame index. A few generated tables (UA 175-B, UA 176) write two-frame lamps as
    /// `true`/`false`; those decode as 1/0.
    #[serde(default, deserialize_with = "lenient_frame", skip_serializing_if = "Option::is_none")]
    pub frame: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_normalized: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// L1 distance over the common prefix of two keys (Swift's `Row.distance`).
pub fn key_distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

impl Row {
    /// Nearest row to `key` (L1 distance); the first of equally near rows wins.
    pub fn nearest<'a>(rows: &'a [Row], key: &[f64]) -> Option<&'a Row> {
        let mut best: Option<(&Row, f64)> = None;
        for r in rows {
            let d = key_distance(&r.key, key);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((r, d));
            }
        }
        best.map(|(r, _)| r)
    }
}

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $text:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)*
            /// A value this runtime does not know (ignored, like Swift's string switch).
            Other,
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other => "other",
                }
            }

            pub fn parse(s: &str) -> Self {
                match s {
                    $($text => Self::$variant,)*
                    _ => Self::Other,
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Ok(Self::parse(&String::deserialize(d)?))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
    };
}

string_enum!(
    /// `frameSource.type`.
    FrameSourceKind { Param = "param", Table = "table", Meter = "meter", Blink = "blink" }
);

string_enum!(
    /// `action.type`.
    ActionKind {
        Set = "set",
        Nudge = "nudge",
        Toggle = "toggle",
        Default = "default",
        Table = "table",
        Multi = "multi",
        SetState = "setState",
        CycleState = "cycleState",
        Copy = "copy",
        Random = "random",
        Snapshot = "snapshot",
        Resend = "resend",
    }
);

string_enum!(
    /// `element.kind`. `toolbar`, `label`, `shape` and `lamp` are schema-2 runtime additions
    /// (RUNTIME-V2.md): elements drawn by our code from facts instead of captured art.
    ElementKind {
        Knob = "knob",
        Switch = "switch",
        Button = "button",
        Filmstrip = "filmstrip",
        Image = "image",
        Zone = "zone",
        Text = "text",
        Menu = "menu",
        Graph = "graph",
        Toolbar = "toolbar",
        Label = "label",
        Shape = "shape",
        Lamp = "lamp",
    }
);

string_enum!(
    /// `toolbar.family`: whose plug-in shell strip the toolbar draws (its colours and glyphs).
    ToolbarFamily { Brainworx = "brainworx", Softube = "softube" }
);

string_enum!(
    /// `toolbar.items[].type`.
    ToolbarItemKind { Icon = "icon", Switch = "switch", Bank = "bank", Knob = "knob", Gear = "gear", Rect = "rect" }
);

string_enum!(
    /// `shape.type`.
    ShapeKind {
        Rect = "rect",
        Ellipse = "ellipse",
        Line = "line",
        Polyline = "polyline",
        Polygon = "polygon",
        Arc = "arc",
        Gear = "gear",
        Hexagon = "hexagon",
    }
);

string_enum!(
    /// `graph.type`; anything but polar and transfer draws as an EQ graph (Swift's default case).
    GraphKind { Eq = "eq", Polar = "polar", Transfer = "transfer" }
);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrameSource {
    #[serde(rename = "type")]
    pub kind: FrameSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<u16>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<Vec<Row>>,
    /// LUNA meter id, a key of [`PluginSpec::meter_sources`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter: Option<String>,
    /// Local UI states appended to the table key after `params`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<String>>,
    /// Calibrated meter values (0…1) appended to the table key after `states`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meters: Option<Vec<String>>,
    /// `meter`: a two-state lamp, last frame while the calibrated value exceeds this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// `blink`: while `param` is on (> 0.5) alternate frame 0 / last frame with this period (s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<f64>,
    /// Base schema's meter channel. Informational: meter ids are already per channel, and the
    /// Swift runtime does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<i64>,
}

impl FrameSource {
    pub fn param(param: u16) -> Self {
        Self {
            kind: FrameSourceKind::Param,
            param: Some(param),
            params: None,
            table: None,
            meter: None,
            states: None,
            meters: None,
            threshold: None,
            period: None,
            channel: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Visibility {
    pub params: Vec<u16>,
    pub table: Vec<Row>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meters: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ActionRow {
    /// `table` keyed on the action's own parameter: its current value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<f64>,
    /// `table` with `keys`: the values of those parameters this row applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Vec<f64>>,
    pub value: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Action {
    #[serde(rename = "type")]
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<Vec<ActionRow>>,
    /// `multi`: every action in order (e.g. a jack that sets input and level).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<Action>>,
    /// `setState` / `cycleState`, snapshot `switch`: the local UI state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// `cycleState`: number of values the state cycles through (0…count−1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// `copy`: write `param`'s current value to each of these parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<u16>>,
    /// `table`: look the value up from these parameters' values (rows carry `key`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<u16>>,
    /// `snapshot`: `switch` | `store` | `recall`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots: Option<Vec<String>>,
    /// `snapshot` (default: every parameter) and `resend`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<u16>>,
    /// `nudge`: wrap past either end instead of clamping (cycle buttons). Not in the Swift
    /// runtime or PIPELINE.md (agent H patches); honoured here, see [`crate::panel`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<bool>,
}

impl Action {
    pub fn new(kind: ActionKind, param: u16) -> Self {
        Self {
            kind,
            param: Some(param),
            value: None,
            delta: None,
            table: None,
            actions: None,
            state: None,
            count: None,
            to: None,
            keys: None,
            op: None,
            slot: None,
            slots: None,
            params: None,
            wrap: None,
        }
    }

    /// Parameters this action (and its sub-actions) binds, as Swift's `unboundParameters`
    /// counts them: `param` and `multi` children only.
    pub fn params(&self, out: &mut BTreeSet<u16>) {
        if let Some(p) = self.param {
            out.insert(p);
        }
        for a in self.actions.iter().flatten() {
            a.params(out);
        }
    }
}

/// Readout drawn with a character filmstrip (LED/nixie digits) instead of a system font.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpriteFont {
    pub sprite: String,
    pub frames: u32,
    /// The character each frame shows, in frame order (e.g. " 0123456789-").
    pub charset: String,
    /// One character cell `[w, h]` in points.
    pub cell: Vec<f64>,
    /// `left` | `center` | `right` (default right).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// A cell-sized image drawn over the previous character for "." (a separate DP segment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimal_sprite: Option<String>,
}

/// Readout of whichever parameter a local state selects: `params[state]`, else the element's.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TextSelect {
    pub state: String,
    pub params: Vec<u16>,
}

/// Touching the element sets a local state; after `timeout` seconds without another touch it
/// returns to `fallback` (default: the state's spec default) unless the `hold` state is on.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TouchState {
    pub state: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
}

/// `text` / `menu` styles and readout sources.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    /// `#RRGGBB` or `#RRGGBBAA`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `left` | `center` | `right`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    /// `regular` | `medium` | `bold`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monospaced: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite_font: Option<SpriteFont>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<TextSelect>,
    /// Maximum lines (default 1; 0 = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<i64>,
    /// Static text (no parameter or meter). Schema 2: a short label (≤ 48 characters, one line);
    /// longer text comes from the user's install through `string_ref`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string: Option<String>,
    /// Schema 2: a texts-recipe id (`dll-string`) whose generated text is shown instead of
    /// `string` (empty until imported).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string_ref: Option<String>,
    /// Schema 2: the font family (a fact: "Helvetica", "Arial", "Ubuntu", …). Absent: Helvetica,
    /// or Menlo when `monospaced`. The renderer substitutes a metric-compatible face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// Schema 2: a single line wider than its frame `shrink`s its size (default, down to half) or
    /// is `squeeze`d horizontally (JUCE's fitted text: down to 70 %, then "...").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<String>,
    /// Schema 2: draw the face's italic (or a synthetic slant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// Schema 2: `top` | `center` (default) | `bottom`: where a block of lines sits in the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valign: Option<String>,
    /// With `value`: print `names[round(value)]` (empty out of range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
    /// Schema 2, with `value` in Hz: print the nearest equal-tempered note name ("A", "A#", …),
    /// tuned to A = this value in Hz (a tuner's reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<GraphValue>,
    /// Computed readout instead of the Console string, printed with `format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<GraphValue>,
    /// printf format, one %f/%g/%e conversion (default "%.0f").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Readout of a meter's conditioned raw value × `scale` + `offset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    /// Meter readouts in dB: `db` × log10(raw) × scale + offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<f64>,
    /// Clamp the printed meter value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Meter readouts: below `floor` (after db/scale/offset) print `floor_text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor_text: Option<String>,
}

/// Clip to `[left, right) × [top, bottom)` in element-local points (absent edges: the element's).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Reveal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<GraphValue>,
}

/// Shift the drawing by (x, y) points.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Move {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<GraphValue>,
}

/// Keep rotating at `rpm` while `while` is on.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Spin {
    pub rpm: f64,
    #[serde(default, rename = "while", skip_serializing_if = "Option::is_none")]
    pub while_on: Option<GraphValue>,
}

/// Alternate the element's computed frame with frame 0 while it is above 0.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Blink {
    pub period: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    pub id: String,
    pub kind: ElementKind,
    pub x: f32,
    pub y: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f32>,
    /// Sprite name: `Images/<sprite>@2x.png`, a vertical strip of `frames`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<u16>,
    /// `vertical` | `horizontal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Zones: `drag` (a drag handle over a filmstrip) | `swipe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_source: Option<FrameSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// Knobs: `vertical` (default) or `horizontal` drag (faders).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextStyle>,
    /// `graph` elements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<Graph>,
    /// Switches: `vertical` | `horizontal`: a tap selects the tapped segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touch_state: Option<TouchState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blink: Option<Blink>,
    /// Switches: while a touch is down draw `frame + pressed_frame_offset`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressed_frame_offset: Option<u32>,
    /// Zones, buttons, switches: the action of a long press (≥ 0.45 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_press_action: Option<Action>,
    /// Knobs: the action of a tap without drag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap_action: Option<Action>,
    /// Shown only while this value is ≥ 0.5 (combined with `visibility`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<GraphValue>,
    /// Buttons: act on press and repeat every `repeat` seconds while held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<f64>,
    /// Swipe zones: `horizontal` (default) | `vertical`, and the travel that steps (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<Reveal>,
    #[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
    pub move_by: Option<Move>,
    /// Degrees clockwise about the element centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spin: Option<Spin>,
    /// `toolbar` elements: a plug-in shell strip (RUNTIME-V2.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolbar: Option<Toolbar>,
    /// `shape` elements: a vector primitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    /// `lamp` elements: an LED or lamp lit by the element's frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lamp: Option<Lamp>,
    /// `label` / `shape`: text (or stroke) colour per frame, `#RRGGBB[AA]`; the frame comes from
    /// `frameSource`, else the `param`'s step (as a switch shows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<Vec<String>>,
    /// `label` / `shape`: fill per frame (a label's box behind its text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fills: Option<Vec<Fill>>,
}

/// A plug-in shell toolbar: a bar of `family` colour with icons (the user's DLL art, by recipe
/// id), parameter switches, a segmented bank selector, ring knobs and glyphs drawn by us.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Toolbar {
    pub family: ToolbarFamily,
    /// Bar fill (default: the family's, brainworx `#000000`, softube `#444444`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Knob ring track and value colours (default: the family's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// In drawing order; coordinates are element-local points.
    #[serde(default)]
    pub items: Vec<ToolbarItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolbarItem {
    #[serde(rename = "type")]
    pub kind: ToolbarItemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default)]
    pub w: f32,
    #[serde(default)]
    pub h: f32,
    /// A vertical filmstrip (recipe id): `icon` draws `frame`; `switch` the parameter's step;
    /// `bank` crops `cells` horizontal cells from frame 0 (unselected) and frame 1 (selected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<u16>,
    /// `switch`: frame 0 is the "on" look (a lit bypass icon).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invert: Option<bool>,
    /// `bank`: number of cells (default: the parameter's steps, else 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<u32>,
    /// `bank`: distance between cell origins in points when the shell spaces the cells wider than
    /// the art (default: `w / cells`, the art's own cells side by side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    /// Drawn when the sprite is not available: `power`, `undo`, `redo` (vector glyphs) or text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// `rect` fill, `gear` and fallback-glyph colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `knob`: the ring, item-local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<Ring>,
}

/// A toolbar knob: an open ring (gap at the bottom) whose value arc runs clockwise.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Ring {
    pub cx: f32,
    pub cy: f32,
    /// Centre-line radius and stroke width (points).
    pub r: f32,
    pub width: f32,
    /// Opening at the bottom in degrees (default 70).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f32>,
}

/// A colour (`#RRGGBB[AA]`) or a gradient.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Fill {
    Color(String),
    Gradient(Gradient),
}

/// Element-local gradient: `linear` `[x0, y0, x1, y1]` or `radial` `[cx, cy, r]`, with
/// `stops` `[[t, "#RRGGBB[AA]"], …]`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Gradient {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radial: Option<[f64; 3]>,
    pub stops: Vec<(f64, String)>,
}

/// A vector primitive in the element's frame (points).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    #[serde(rename = "type")]
    pub kind: ShapeKind,
    /// `rect`: corner radius.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// `line` / `polyline` / `polygon`: element-local points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Fill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<Vec<f64>>,
    /// `arc`: degrees clockwise from up (GraphValues, so an arc can follow a parameter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<GraphValue>,
    /// `gear`: teeth and the root radius as a fraction of the outer radius (default 8, 0.72).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teeth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner: Option<f64>,
    /// Shrink the frame by this many points on every side before drawing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inset: Option<f64>,
}

/// An LED or lamp: `off` → `on` by the element's level (frame / (frames − 1)), with an optional
/// glow, bezel and highlight.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Lamp {
    pub on: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<String>,
    /// `circle` (default) | `rect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// Glow beyond the lamp (points), drawn while lit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<f64>,
    /// A soft specular highlight in the upper left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bezel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bezel_width: Option<f64>,
}

impl Element {
    /// Rect in panel points.
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w.unwrap_or(0.0), self.h.unwrap_or(0.0))
    }

    /// `max(frames ?? 1, 1)`.
    pub fn frame_count(&self) -> u32 {
        self.frames.unwrap_or(1).max(1)
    }

    pub fn is_horizontal(&self) -> bool {
        self.orientation.as_deref() == Some("horizontal")
    }

    /// Kinds that bind a parameter or action for the "more" panel's bookkeeping.
    pub fn is_interactive_kind(&self) -> bool {
        matches!(
            self.kind,
            ElementKind::Knob | ElementKind::Switch | ElementKind::Button | ElementKind::Zone | ElementKind::Menu
        )
    }
}

/// How a LUNA meter id is fed from UA Console (`VendorSpecific` reads).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeterSource {
    /// Native plug-in variable id: `<slot>/VendorSpecific/value?floats=ID`; absent for `combine`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_specific_float: Option<i64>,
    /// `(raw, needle 0…1)` points, raw ascending.
    #[serde(default)]
    pub calibration: Vec<Vec<f64>>,
    /// Per-tick fraction toward the target (default 0.7); ≤ 0 or ≥ 1 follows immediately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f64>,
    /// Tagged ids are read with `?opcode=519&p0=ID&p1=count&p2=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opcode: Option<Opcode>,
    /// Hold the last non-zero raw value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_zero: Option<bool>,
    /// Calibrate |raw|.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute: Option<bool>,
    /// The variable is an Int32 whose bits arrive in the float slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integer: Option<bool>,
    /// `direction`: +1 while the raw value rises, −1 while it falls (see [`crate::meters`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive: Option<String>,
    /// Optional per-mode curves selected by a parameter (a METER switch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_param: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_calibrations: Option<Vec<ModeCalibration>>,
    /// A derived source: the max of other (calibrated) meters, then `calibration` if non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine: Option<MeterCombine>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MeterCombine {
    pub op: String,
    pub meters: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Opcode {
    pub op: i64,
    pub p0: i64,
    pub p1: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ModeCalibration {
    /// Normalized value of `mode_param` this curve applies to.
    pub value: f64,
    pub calibration: Vec<Vec<f64>>,
}

impl MeterSource {
    pub fn is_integer(&self) -> bool {
        self.integer == Some(true)
    }

    /// Piecewise-linear needle position for a raw value: the end points' needles outside the
    /// curve, the interpolation (clamped to 0…1) inside, 0 for no curve or NaN (Swift's
    /// `calibrate`; points with fewer than two numbers are skipped instead of trapping).
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

    /// The curve for the current value of `mode_param`: the nearest `mode_calibrations` entry
    /// when it is within 0.01, else the base curve.
    pub fn curve(&self, mode: Option<f64>) -> &[Vec<f64>] {
        if let (Some(mode), Some(_), Some(modes)) = (mode, self.mode_param, &self.mode_calibrations) {
            let mut best: Option<&ModeCalibration> = None;
            for m in modes {
                if best.is_none_or(|b| (m.value - mode).abs() < (b.value - mode).abs()) {
                    best = Some(m);
                }
            }
            if let Some(m) = best.filter(|m| (m.value - mode).abs() < 0.01) {
                return &m.calibration;
            }
        }
        &self.calibration
    }
}

// MARK: - Graph elements

/// A display drawn from parameter (and meter) values: EQ response, mic polar pattern, dynamics
/// transfer curve, analyser. Coordinates are element-local @1x points.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    #[serde(rename = "type")]
    pub kind: GraphKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// Sprite drawn at natural size under the curves (covers the frozen art).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlay: Option<String>,
    /// Curves, fills and traces only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<GraphRect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<GraphAxis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<GraphAxis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<GraphBand>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves: Option<Vec<GraphCurve>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handles: Option<Vec<GraphHandle>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polar: Option<GraphPolar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traces: Option<Vec<GraphTrace>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markers: Option<Vec<GraphMarker>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<GraphRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spectrum: Option<GraphSpectrum>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polylines: Option<Vec<GraphPolyline>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_marker: Option<GraphPeakMarker>,
}

impl Graph {
    pub fn band(&self, id: Option<&str>) -> Option<&GraphBand> {
        let id = id?;
        self.bands.iter().flatten().find(|b| b.id == id)
    }

    /// The values a handle drag writes: horizontal (frequency) and vertical (gain or `y`).
    pub fn drag_values<'a>(&'a self, h: &'a GraphHandle) -> (Option<&'a GraphValue>, Option<&'a GraphValue>) {
        let b = self.band(h.band.as_deref());
        let x = h.x.as_ref().or_else(|| b.and_then(|b| b.freq.as_ref()));
        let y = h.y.as_ref().or_else(|| {
            if h.y_mode.as_deref().unwrap_or("gain") == "gain" { b.and_then(|b| b.gain.as_ref()) } else { None }
        });
        (x, y)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A value range mapped linearly (or logarithmically) between two element-local positions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphAxis {
    pub min: f64,
    pub max: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<bool>,
    /// Position of `min` / `max` (x: default 0 / width, y: default height / 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
}

/// Normalized 0…1 → real units.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphLaw {
    /// `linear` | `log` | `power` | `bipolar` | `steps` | `table`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<f64>,
    /// `steps`: value per step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<f64>>,
    /// `table`: `[[normalized, value], …]` ascending; `interp: "log"` = geometric.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<Vec<f64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interp: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphRow {
    pub key: Vec<f64>,
    pub value: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphSelect {
    pub param: u16,
    pub cases: Vec<GraphValue>,
}

/// A number, a parameter through a law, a lookup keyed by several parameters, a meter, or a
/// combination (`sum`, `product`, `select`, `min`, `max`, `ratio`, `apply`). Decoded like Swift's
/// custom `init(from:)`: a bare number is a constant; `min`/`max` count only when they decode as
/// a list of values (`try?`), so a stray scalar `min` is ignored rather than fatal.
/// [`crate::graph`] evaluates it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphValue {
    pub constant: Option<f64>,
    pub param: Option<u16>,
    pub law: Option<GraphLaw>,
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    pub params: Option<Vec<u16>>,
    pub table: Option<Vec<GraphRow>>,
    pub sum: Option<Vec<GraphValue>>,
    pub product: Option<Vec<GraphValue>>,
    pub select: Option<Box<GraphSelect>>,
    pub meter: Option<String>,
    pub db: Option<f64>,
    pub min_of: Option<Vec<GraphValue>>,
    pub max_of: Option<Vec<GraphValue>>,
    pub ratio: Option<Vec<GraphValue>>,
    pub apply: Option<Vec<GraphValue>>,
}

#[derive(Deserialize)]
struct GraphValueFields {
    #[serde(default)]
    param: Option<u16>,
    #[serde(default)]
    law: Option<GraphLaw>,
    #[serde(default)]
    scale: Option<f64>,
    #[serde(default)]
    offset: Option<f64>,
    #[serde(default)]
    params: Option<Vec<u16>>,
    #[serde(default)]
    table: Option<Vec<GraphRow>>,
    #[serde(default)]
    sum: Option<Vec<GraphValue>>,
    #[serde(default)]
    product: Option<Vec<GraphValue>>,
    #[serde(default)]
    select: Option<GraphSelect>,
    #[serde(default)]
    meter: Option<String>,
    #[serde(default)]
    db: Option<f64>,
    #[serde(default)]
    ratio: Option<Vec<GraphValue>>,
    #[serde(default)]
    apply: Option<Vec<GraphValue>>,
    #[serde(default)]
    min: Option<serde_json::Value>,
    #[serde(default)]
    max: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GraphValueRepr {
    Number(f64),
    Object(Box<GraphValueFields>),
}

impl<'de> Deserialize<'de> for GraphValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // `try?`: a list that does not decode as values is ignored, not an error.
        let list = |v: Option<serde_json::Value>| v.and_then(|v| serde_json::from_value::<Vec<GraphValue>>(v).ok());
        Ok(match GraphValueRepr::deserialize(d)? {
            GraphValueRepr::Number(n) => GraphValue { constant: Some(n), ..Default::default() },
            GraphValueRepr::Object(f) => {
                let f = *f;
                GraphValue {
                    constant: None,
                    param: f.param,
                    law: f.law,
                    scale: f.scale,
                    offset: f.offset,
                    params: f.params,
                    table: f.table,
                    sum: f.sum,
                    product: f.product,
                    select: f.select.map(Box::new),
                    meter: f.meter,
                    db: f.db,
                    min_of: list(f.min),
                    max_of: list(f.max),
                    ratio: f.ratio,
                    apply: f.apply,
                }
            }
        })
    }
}

impl Serialize for GraphValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        if let Some(c) = self.constant {
            return s.serialize_f64(c);
        }
        let mut m = s.serialize_map(None)?;
        macro_rules! field {
            ($key:literal, $v:expr) => {
                if let Some(v) = &$v {
                    m.serialize_entry($key, v)?;
                }
            };
        }
        field!("param", self.param);
        field!("law", self.law);
        field!("scale", self.scale);
        field!("offset", self.offset);
        field!("params", self.params);
        field!("table", self.table);
        field!("sum", self.sum);
        field!("product", self.product);
        field!("select", self.select);
        field!("meter", self.meter);
        field!("db", self.db);
        field!("min", self.min_of);
        field!("max", self.max_of);
        field!("ratio", self.ratio);
        field!("apply", self.apply);
        m.end()
    }
}

impl GraphValue {
    pub fn constant(v: f64) -> Self {
        Self { constant: Some(v), ..Default::default() }
    }

    pub fn param(k: u16) -> Self {
        Self { param: Some(k), ..Default::default() }
    }

    /// Meter ids this value reads (recursively).
    pub fn meters(&self, out: &mut BTreeSet<String>) {
        if let Some(m) = &self.meter {
            out.insert(m.clone());
        }
        for list in
            [&self.sum, &self.product, &self.min_of, &self.max_of, &self.ratio, &self.apply].into_iter().flatten()
        {
            for v in list {
                v.meters(out);
            }
        }
        if let Some(sel) = &self.select {
            for v in &sel.cases {
                v.meters(out);
            }
        }
    }
}

/// One EQ section. Gains in dB, frequencies in Hz.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphBand {
    pub id: String,
    /// `bell` | `lowShelf` | `highShelf` | `lowShelf1` | `highShelf1` | `highPass` | `lowPass` |
    /// `notch` | `bandPass` | `gain` | `bypass` (flat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_param: Option<GraphFilterSelect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<GraphValue>,
    /// Octaves, used when `q` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth: Option<GraphValue>,
    /// Pass filters: order (1 = 6 dB/oct, 0 = flat), or `slope` in dB/oct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slope: Option<GraphValue>,
    /// ≥ 0.5 = in; absent = always in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<GraphValue>,
    /// `rbj` (default) | `peak` | `3dB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelf_q: Option<f64>,
    /// Shelves: 1 = first-order, 2 = second-order (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelf_order: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelf_shift: Option<f64>,
    /// Pass filters: `butterworth` (default) | `linkwitzRiley` | `rbj`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphFilterSelect {
    pub param: u16,
    pub values: Vec<String>,
}

/// A drawn response: the sum of `bands` (all when absent), or `groups` in parallel.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphCurve {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_to_gain: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// Fill baseline in dB (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_to: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_enabled: Option<bool>,
    /// dB added to the whole curve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<GraphValue>,
    /// Stroke only where the curve is more than this many dB away from 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_below: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
}

/// A band dot (or free point); draggable when `drag`. Transfer graphs: the point
/// (x, output of `trace` at x).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphHandle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<GraphValue>,
    /// `gain` (default) | `curve`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_mode: Option<String>,
    /// Keep inside the y range (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clamp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<f64>,
    /// `circle` (default) | `square`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touch_state: Option<TouchState>,
}

/// `[y, "#RRGGBBAA"]`: a vertical gradient stop at element-local y.
#[derive(Clone, Debug, PartialEq)]
pub struct GradientStop {
    pub y: f64,
    pub color: String,
}

impl<'de> Deserialize<'de> for GradientStop {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (y, color) = <(f64, String)>::deserialize(d)?;
        Ok(Self { y, color })
    }
}

impl Serialize for GradientStop {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (self.y, &self.color).serialize(s)
    }
}

/// Analyser bins `first .. first + count` (Console variable ids polled with opcode 519).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphSpectrum {
    pub first: i64,
    pub count: i64,
    pub hz0: f64,
    pub hz_step: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
    /// Explicit bin frequencies (Hz, display order) instead of `hz0 + i × hz_step`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hz: Option<Vec<f64>>,
    /// Bins stored highest frequency first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverse: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_gradient: Option<Vec<GradientStop>>,
    /// Catmull-Rom curve through the bin points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smooth: Option<bool>,
    /// `fp16x2`: two IEEE half floats per variable (low 16 bits first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PeakLabel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dx: Option<f64>,
    pub y: f64,
}

/// The loudest decoded spectrum point with hz in `[from, to]`: a vertical line and a label.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphPeakMarker {
    pub from: GraphValue,
    pub to: GraphValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<PeakLabel>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PolylinePoint {
    pub x: f64,
    pub y: GraphValue,
}

/// A curve through points at fixed x (axis units) whose y is any value (axis units).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphPolyline {
    pub points: Vec<PolylinePoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_gradient: Option<Vec<GradientStop>>,
    /// Fill down to this y (axis units; default the element's bottom).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_to: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smooth: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
}

/// A horizontal line from x = `from` to x = `to` (axis units) at element-local `y` (or `y_value`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphRange {
    pub from: GraphValue,
    pub to: GraphValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_value: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dots: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dot_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
}

/// Polar plot frame: `radius` is the full-amplitude ring.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphPolar {
    pub center: Vec<f64>,
    pub radius: f64,
    /// `linear` (default) | `db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_range: Option<f64>,
}

/// Polar: first-order pattern (or a lobe / arc). Transfer: a dynamics curve or a chain.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphTrace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_scale: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_offset: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<GraphValue>,
    /// `compressor` | `limiter` | `expander` | `gate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knee: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub makeup: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplitude: Option<GraphValue>,
    /// None = first-order pattern, `lobe`, `arc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobe_width: Option<f64>,
    /// Lobes: `horizontal` | `opposite`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_from: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc_to: Option<GraphValue>,
}

/// Polar: a small triangle on the rim at `angle` degrees (clockwise from up) pointing inwards.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct GraphMarker {
    pub angle: GraphValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flip: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<GraphValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
}

// MARK: - The spec

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSpec {
    pub schema: i64,
    /// Engine `EffectName`, e.g. "UAD dbx 160".
    pub effect_name: String,
    /// Substring matched against `EffectName` when there is no exact match.
    pub name_pattern: String,
    /// Panel size in points (sprites are @2x).
    pub size: Size,
    /// Editor size on a mono insert (state `channels` = 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono_size: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    /// In z-order (first is drawn first).
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meter_sources: Option<BTreeMap<String, MeterSource>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo: Option<Vec<String>>,
    /// Local UI state with defaults (pages, tabs, band selection, `channels`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<BTreeMap<String, f64>>,
    /// Parameters the original editor has no control for (verified); reachable from the
    /// "more" panel like every unbound parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panelless: Option<Vec<u16>>,
}

impl PluginSpec {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let spec = Self::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if spec.schema > Self::SCHEMA_MAX {
            eprintln!(
                "plugin-ui: {} is schema {} (this runtime reads up to {}); unknown fields are ignored",
                path.display(),
                spec.schema,
                Self::SCHEMA_MAX
            );
        }
        Ok(spec)
    }

    pub fn parameter(&self, index: u16) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.index == index)
    }

    /// Spec step labels for a parameter (only when it has more than one).
    pub fn step_labels(&self, index: u16) -> Option<&[String]> {
        self.parameter(index).and_then(|p| p.steps.as_deref()).filter(|s| s.len() > 1)
    }

    pub fn meter_source(&self, id: &str) -> Option<&MeterSource> {
        self.meter_sources.as_ref().and_then(|m| m.get(id))
    }

    /// Sprite names the spec draws: background, element sprites, graph underlays and sprite fonts.
    pub fn sprite_names(&self) -> BTreeSet<&str> {
        let mut names: BTreeSet<&str> = BTreeSet::new();
        if let Some(bg) = &self.background {
            names.insert(bg);
        }
        for e in &self.elements {
            if let Some(s) = &e.sprite {
                names.insert(s);
            }
            if let Some(u) = e.graph.as_ref().and_then(|g| g.underlay.as_deref()) {
                names.insert(u);
            }
            if let Some(f) = e.text.as_ref().and_then(|t| t.sprite_font.as_ref()) {
                names.insert(&f.sprite);
                if let Some(d) = &f.decimal_sprite {
                    names.insert(d);
                }
            }
            for item in e.toolbar.iter().flat_map(|t| &t.items) {
                if let Some(s) = &item.sprite {
                    names.insert(s);
                }
            }
        }
        names
    }

    /// Schema 2: texts-recipe ids the spec reads (`text.stringRef`, `parameters[].stepsRef`).
    pub fn text_refs(&self) -> BTreeSet<&str> {
        let mut refs: BTreeSet<&str> = BTreeSet::new();
        for e in &self.elements {
            if let Some(r) = e.text.as_ref().and_then(|t| t.string_ref.as_deref()) {
                refs.insert(r);
            }
        }
        for p in &self.parameters {
            if let Some(r) = &p.steps_ref {
                refs.insert(r);
            }
        }
        refs
    }

    /// Highest `schema` this runtime reads; newer specs load with a warning (unknown keys and
    /// element kinds are ignored, as before).
    pub const SCHEMA_MAX: i64 = 2;

    /// True for schema 1 (ConsoleLink's stage) and 2 (release format: recipe ids, text refs).
    pub fn is_known_schema(&self) -> bool {
        (1..=Self::SCHEMA_MAX).contains(&self.schema)
    }

    /// Parameters no panel element can change, offered in the "more" panel (Swift's
    /// `unboundParameters`): knob/switch/button/zone/menu params, their action, long-press and
    /// tap actions (with `multi` children), and the targets of draggable graph handles.
    pub fn unbound_parameters(&self) -> Vec<u16> {
        let mut bound = BTreeSet::new();
        for el in &self.elements {
            if el.is_interactive_kind() {
                if let Some(p) = el.param {
                    bound.insert(p);
                }
                for a in [&el.action, &el.long_press_action, &el.tap_action].into_iter().flatten() {
                    a.params(&mut bound);
                }
            }
            for item in el.toolbar.iter().flat_map(|t| &t.items) {
                if let (Some(p), ToolbarItemKind::Switch | ToolbarItemKind::Bank | ToolbarItemKind::Knob) =
                    (item.param, item.kind)
                {
                    bound.insert(p);
                }
            }
            if el.kind == ElementKind::Graph {
                let Some(g) = &el.graph else { continue };
                for h in g.handles.iter().flatten().filter(|h| h.drag == Some(true)) {
                    let (x, y) = g.drag_values(h);
                    for v in [x, y].into_iter().flatten() {
                        if let Some(p) = v.param {
                            bound.insert(p);
                        }
                    }
                }
            }
        }
        self.parameters.iter().map(|p| p.index).filter(|i| !bound.contains(i)).collect()
    }

    fn meters(&self) -> impl Iterator<Item = (&String, &MeterSource)> {
        self.meter_sources.iter().flatten()
    }

    /// Analyser blocks of graph elements, polled like opcode meters (`op` 519).
    pub fn spectrum_opcodes(&self) -> Vec<Opcode> {
        self.elements
            .iter()
            .filter_map(|e| e.graph.as_ref()?.spectrum.as_ref())
            .map(|s| Opcode { op: 519, p0: s.first, p1: s.count })
            .collect()
    }

    /// True when a graph draws an analyser (the latest opcode block is kept by variable id).
    pub fn has_spectrum(&self) -> bool {
        self.elements.iter().any(|e| e.graph.as_ref().is_some_and(|g| g.spectrum.is_some()))
    }

    /// One opcode request covering the opcode-capable meter sources and analyser blocks: the
    /// largest group sharing an opcode and tag (`p0 >> 24`), as a contiguous range
    /// `(op, p0, count)` (ConsoleLink polls a single opcode per plug-in). Swift breaks ties by
    /// dictionary order (unspecified); here the lowest `(op, tag)` of the tied groups wins.
    pub fn opcode_range(&self) -> Option<(i64, i64, i64)> {
        let mut groups: BTreeMap<(i64, i64), Vec<Opcode>> = BTreeMap::new();
        for o in self.meters().filter_map(|(_, m)| m.opcode).chain(self.spectrum_opcodes()) {
            groups.entry((o.op, o.p0 >> 24)).or_default().push(o);
        }
        let mut best: Option<&Vec<Opcode>> = None;
        for g in groups.values() {
            if best.is_none_or(|b| g.len() > b.len()) {
                best = Some(g);
            }
        }
        let group = best?;
        let lo = group.iter().map(|o| o.p0).min()?;
        let hi = group.iter().map(|o| o.p0 + o.p1.max(1)).max()?;
        Some((group[0].op, lo, hi - lo))
    }

    /// True when the source is served by [`opcode_range`](Self::opcode_range) rather than `floats=`.
    pub fn uses_opcode(&self, source: &MeterSource) -> bool {
        Self::served(source, self.opcode_range())
    }

    fn served(source: &MeterSource, range: Option<(i64, i64, i64)>) -> bool {
        match (source.opcode, range) {
            (Some(o), Some((op, p0, n))) => o.op == op && o.p0 >= p0 && o.p0 < p0 + n,
            _ => false,
        }
    }

    /// Float ids to poll with `floats=`, ascending (the order the reply lists them).
    pub fn poll_floats(&self) -> Vec<i64> {
        let range = self.opcode_range();
        let ids: BTreeSet<i64> = self
            .meters()
            .filter(|(_, m)| !Self::served(m, range))
            .filter_map(|(_, m)| m.vendor_specific_float)
            .collect();
        ids.into_iter().collect()
    }

    /// The editor size: `mono_size` on a mono insert (`channels` < 1.5) when the spec has one.
    pub fn display_size(&self, channels: Option<f64>) -> Size {
        match (channels, self.mono_size) {
            (Some(c), Some(m)) if c < 1.5 => m,
            _ => self.size,
        }
    }
}
