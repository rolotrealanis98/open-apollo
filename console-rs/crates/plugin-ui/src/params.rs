//! Where a plug-in window's values come from and where its edits go.
//!
//! The engine-backed implementation lives with `ua-engine`; see this crate's README for the
//! exact path mapping it must use. [`MockParams`] is an in-memory stand-in for renders and tests.

use crate::spec::PluginSpec;

/// One plug-in slot's parameters, by ordinal `k` (`<slot>/parameters/<k>`).
///
/// Values are Console normalized values, 0…1. Stepped parameters spread their `n` steps evenly
/// over 0…1 (step `i` = `i / (n − 1)`).
///
/// **Gestures.** A continuous edit (knob drag) is `begin_gesture`, any number of `set`, then
/// `end_gesture`, and must become one undo step. Gestures nest; only the outermost pair is
/// significant (the engine adapter brackets it with `/UndoRecording` true … false and coalesces
/// the `set`s in between). A discrete edit (switch, step, button) is [`commit`](Self::commit).
pub trait ParamSource {
    /// Number of parameters the slot publishes.
    fn count(&self) -> usize;

    /// Parameter ordinals, ascending. Defaults to `0..count()`.
    fn indices(&self) -> Vec<u16> {
        (0..self.count() as u16).collect()
    }

    /// `Name`; None when the slot has no such parameter.
    fn name(&self, k: u16) -> Option<String>;

    /// `NormalizedValue` 0…1; None when the slot does not publish parameter `k` (the panel then
    /// falls back to its own local value and the spec's default).
    fn normalized(&self, k: u16) -> Option<f64>;

    /// The plug-in's display text for the current value (`StringValue`: "2.80", "Off").
    fn display(&self, k: u16) -> Option<String>;

    /// `StepValue` labels in order; empty for continuous parameters.
    fn steps(&self, k: u16) -> Vec<String>;

    fn begin_gesture(&mut self);

    /// A value inside a gesture. Implementations clamp to 0…1.
    fn set(&mut self, k: u16, v: f64);

    fn end_gesture(&mut self);

    /// A discrete edit: its own undo step.
    fn commit(&mut self, k: u16, v: f64) {
        self.begin_gesture();
        self.set(k, v);
        self.end_gesture();
    }
}

/// Clamp to 0…1 (NaN → 0).
pub fn clamp01(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// Normalized value of step `i` of `n`.
pub fn step_value(i: usize, n: usize) -> f64 {
    if n > 1 { i.min(n - 1) as f64 / (n - 1) as f64 } else { 0.0 }
}

/// Step index of a normalized value with `n` steps.
pub fn step_index(v: f64, n: usize) -> usize {
    if n > 1 { ((clamp01(v) * (n - 1) as f64).round() as usize).min(n - 1) } else { 0 }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MockParam {
    pub name: String,
    pub value: f64,
    pub steps: Vec<String>,
}

/// What a [`MockParams`] was asked to do, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum MockEvent {
    Begin,
    Set(u16, f64),
    End,
}

/// In-memory parameters: values, names, steps, and a log of gestures and sets.
#[derive(Clone, Debug, Default)]
pub struct MockParams {
    pub params: Vec<MockParam>,
    pub events: Vec<MockEvent>,
    pub depth: u32,
}

impl MockParams {
    pub fn new(params: Vec<MockParam>) -> Self {
        Self { params, ..Default::default() }
    }

    /// Every spec parameter at its default, with the spec's names and steps. Ordinals missing
    /// from the spec are filled as unnamed continuous parameters.
    pub fn from_spec(spec: &PluginSpec) -> Self {
        let n = spec.parameters.iter().map(|p| p.index as usize + 1).max().unwrap_or(0);
        let params = (0..n as u16)
            .map(|k| match spec.parameter(k) {
                Some(p) => MockParam { name: p.name.clone(), value: p.default_value.unwrap_or(0.0), steps: p.steps.clone().unwrap_or_default() },
                None => MockParam { name: format!("Parameter {k}"), value: 0.0, steps: vec![] },
            })
            .collect();
        Self::new(params)
    }

    /// Sets every value directly (no events), e.g. all 0 like the LUNA reference renders.
    pub fn set_all(&mut self, v: f64) {
        for p in &mut self.params {
            p.value = clamp01(v);
        }
    }

    pub fn value(&self, k: u16) -> Option<f64> {
        self.params.get(k as usize).map(|p| p.value)
    }
}

impl ParamSource for MockParams {
    fn count(&self) -> usize {
        self.params.len()
    }
    fn name(&self, k: u16) -> Option<String> {
        self.params.get(k as usize).map(|p| p.name.clone())
    }
    fn normalized(&self, k: u16) -> Option<f64> {
        self.value(k)
    }
    /// Step label for stepped parameters, else the value as a percentage.
    fn display(&self, k: u16) -> Option<String> {
        let p = self.params.get(k as usize)?;
        if p.steps.len() > 1 {
            return Some(p.steps[step_index(p.value, p.steps.len())].clone());
        }
        Some(format!("{:.0}%", p.value * 100.0))
    }
    fn steps(&self, k: u16) -> Vec<String> {
        self.params.get(k as usize).map(|p| p.steps.clone()).unwrap_or_default()
    }
    fn begin_gesture(&mut self) {
        self.depth += 1;
        self.events.push(MockEvent::Begin);
    }
    fn set(&mut self, k: u16, v: f64) {
        let v = clamp01(v);
        if let Some(p) = self.params.get_mut(k as usize) {
            p.value = v;
            self.events.push(MockEvent::Set(k, v));
        }
    }
    fn end_gesture(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        self.events.push(MockEvent::End);
    }
}
