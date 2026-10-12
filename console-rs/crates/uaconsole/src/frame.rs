//! One frame: the mixer window, overlays and the stats HUD as a single draw list, plus timing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use houston::{Rect, Rgba};
use rack::{Align, Assets, DrawList, Hit, MixerModel, ScrollRegion, Ui, ViewState, WindowLayout, build_window_with};

use crate::overlay::OverlayStack;

pub struct Frame {
    pub list: DrawList,
    /// The mixer's hit regions (overlay panels hit-test their own events).
    pub hits: Vec<Hit>,
    /// Wheel-scrollable areas of the mixer (browser list, cells column).
    pub scrolls: Vec<ScrollRegion>,
    pub layout: WindowLayout,
}

/// The window layout `build_window_with` will produce for `view`, without building it.
pub fn layout_for(model: &dyn MixerModel, view: &ViewState) -> WindowLayout {
    WindowLayout::for_model(model, view)
}

/// Stats text drawn at the top-left of the visible `area`, `px_per_pt` keeps it at a fixed
/// pixel size whatever the zoom.
pub struct Hud<'a> {
    pub text: &'a str,
    pub px_per_pt: f32,
}

/// Builds the window, then the overlays above it (centred in `area`, the visible part of the
/// window in points), then the HUD.
pub fn build_frame(
    assets: &Assets,
    model: &dyn MixerModel,
    view: &ViewState,
    overlays: &mut OverlayStack,
    area: Rect,
    hud: Option<Hud>,
) -> Frame {
    let mut ui = Ui::new(assets);
    let layout = build_window_with(&mut ui, model, view);
    overlays.build(&mut ui, area);
    if let Some(hud) = hud {
        draw_hud(&mut ui, hud, area);
    }
    Frame {
        list: ui.list,
        hits: ui.hits,
        scrolls: ui.scrolls,
        layout,
    }
}

fn draw_hud(ui: &mut Ui, hud: Hud, area: Rect) {
    let px = 1.0 / hud.px_per_pt.max(0.01);
    let size = 12.0 * px;
    let lines = hud.text.lines().count().max(1) as f32;
    let chars = hud
        .text
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0) as f32;
    let (pad, line_h) = (6.0 * px, (12.0 * 1.15f32).ceil() * px);
    let r = Rect::new(
        area.x + 8.0 * px,
        area.y + 8.0 * px,
        chars * size * 0.56 + 2.0 * pad,
        lines * line_h + 2.0 * pad,
    );
    ui.clip(None);
    ui.fill(
        r,
        Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.72,
        },
    );
    let style = Ui::plain_style("Helvetica", size, false, Rgba::rgb8(150, 240, 160));
    ui.text(hud.text, style, r.inset(pad, pad, pad, pad), Align::Left);
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// (average, p99, max) of `samples`.
pub fn summary(samples: &[f64]) -> (f64, f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut s = samples.to_vec();
    s.sort_by(f64::total_cmp);
    let avg = s.iter().sum::<f64>() / s.len() as f64;
    let p99 = s[((s.len() as f64 * 0.99).ceil() as usize).clamp(1, s.len()) - 1];
    (avg, p99, s[s.len() - 1])
}

/// Rolling frame timings for the HUD.
#[derive(Default)]
pub struct FrameTimes {
    presented: VecDeque<Instant>,
    pub build_ms: f64,
    pub render_ms: f64,
    build: VecDeque<f64>,
    render: VecDeque<f64>,
}

const KEEP: usize = 120;

impl FrameTimes {
    pub fn built(&mut self, d: Duration) {
        self.build_ms = ms(d);
        push(&mut self.build, self.build_ms);
    }

    pub fn rendered(&mut self, now: Instant, d: Duration) {
        self.render_ms = ms(d);
        push(&mut self.render, self.render_ms);
        self.presented.push_back(now);
        while self.presented.len() > KEEP {
            self.presented.pop_front();
        }
    }

    /// Frames presented during the last second.
    pub fn fps(&self, now: Instant) -> usize {
        self.presented
            .iter()
            .filter(|&&t| now.duration_since(t) <= Duration::from_secs(1))
            .count()
    }

    pub fn build_p99(&self) -> f64 {
        summary(&self.build.iter().copied().collect::<Vec<_>>()).1
    }

    pub fn render_p99(&self) -> f64 {
        summary(&self.render.iter().copied().collect::<Vec<_>>()).1
    }
}

fn push(q: &mut VecDeque<f64>, v: f64) {
    q.push_back(v);
    while q.len() > KEEP {
        q.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(summary(&v), (50.5, 99.0, 100.0));
        assert_eq!(summary(&[3.0]), (3.0, 3.0, 3.0));
        assert_eq!(summary(&[]), (0.0, 0.0, 0.0));
    }
}
