//! Mapping the logical Console window (points, `WindowLayout::size()`) onto the surface
//! (pixels): "fit" scales it to the surface, letterboxed; "1:1" draws a point as one logical
//! pixel. Zoom multiplies either; whenever the content is larger than the surface it pans.

use houston::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Fit,
    OneToOne,
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Fit => "fit",
            ViewMode::OneToOne => "1to1",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fit" => Some(ViewMode::Fit),
            "1to1" | "1:1" => Some(ViewMode::OneToOne),
            _ => None,
        }
    }
}

pub const ZOOM_MIN: f32 = 0.25;
pub const ZOOM_MAX: f32 = 4.0;
const ZOOM_STEPS: [f32; 15] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 3.0, 4.0,
];

pub fn zoom_in(z: f32) -> f32 {
    ZOOM_STEPS
        .iter()
        .copied()
        .find(|&s| s > z + 1e-3)
        .unwrap_or(ZOOM_MAX)
}

pub fn zoom_out(z: f32) -> f32 {
    ZOOM_STEPS
        .iter()
        .rev()
        .copied()
        .find(|&s| s < z - 1e-3)
        .unwrap_or(ZOOM_MIN)
}

/// Points → pixels: `px = pt * scale + offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub scale: f32,
    pub offset: (f32, f32),
}

impl Transform {
    pub fn to_logical(&self, px: (f64, f64)) -> (f32, f32) {
        (
            ((px.0 as f32) - self.offset.0) / self.scale,
            ((px.1 as f32) - self.offset.1) / self.scale,
        )
    }

    /// The part of the logical window visible on a `surface`-sized target.
    pub fn visible(&self, logical: (f32, f32), surface: (u32, u32)) -> Rect {
        let (x0, y0) = self.to_logical((0.0, 0.0));
        let (x1, y1) = self.to_logical((surface.0 as f64, surface.1 as f64));
        let (x0, y0) = (x0.max(0.0), y0.max(0.0));
        Rect::new(
            x0,
            y0,
            (x1.min(logical.0) - x0).max(0.0),
            (y1.min(logical.1) - y0).max(0.0),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub mode: ViewMode,
    pub zoom: f32,
    /// Content origin in pixels, used on axes where the content overflows the surface.
    pan: (f32, f32),
}

impl View {
    pub fn new(mode: ViewMode, zoom: f32) -> Self {
        Self {
            mode,
            zoom: zoom.clamp(ZOOM_MIN, ZOOM_MAX),
            pan: (0.0, 0.0),
        }
    }

    pub fn transform(
        &self,
        logical: (f32, f32),
        surface: (u32, u32),
        scale_factor: f64,
    ) -> Transform {
        let (sw, sh) = (surface.0.max(1) as f32, surface.1.max(1) as f32);
        let (lw, lh) = (logical.0.max(1.0), logical.1.max(1.0));
        let base = match self.mode {
            ViewMode::Fit => (sw / lw).min(sh / lh),
            ViewMode::OneToOne => scale_factor as f32,
        };
        let scale = base * self.zoom;
        let axis = |s: f32, content: f32, pan: f32| {
            if content <= s {
                ((s - content) / 2.0).round()
            } else {
                pan.clamp(s - content, 0.0).round()
            }
        };
        Transform {
            scale,
            offset: (
                axis(sw, lw * scale, self.pan.0),
                axis(sh, lh * scale, self.pan.1),
            ),
        }
    }

    pub fn pan_by(
        &mut self,
        dx: f32,
        dy: f32,
        logical: (f32, f32),
        surface: (u32, u32),
        scale_factor: f64,
    ) {
        let t = self.transform(logical, surface, scale_factor);
        self.pan = (t.offset.0 + dx, t.offset.1 + dy);
        self.pan = self.transform(logical, surface, scale_factor).offset;
    }

    /// Sets the zoom keeping the logical point under `anchor` (pixels) in place.
    pub fn set_zoom(
        &mut self,
        zoom: f32,
        anchor: (f64, f64),
        logical: (f32, f32),
        surface: (u32, u32),
        scale_factor: f64,
    ) {
        let before = self.transform(logical, surface, scale_factor);
        let p = before.to_logical(anchor);
        self.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        let s = self.transform(logical, surface, scale_factor).scale;
        self.pan = (anchor.0 as f32 - p.0 * s, anchor.1 as f32 - p.1 * s);
        self.pan = self.transform(logical, surface, scale_factor).offset;
    }

    pub fn set_mode(&mut self, mode: ViewMode) {
        self.mode = mode;
        self.pan = (0.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGICAL: (f32, f32) = (1920.0, 1050.0);

    #[test]
    fn fit_letterboxes() {
        let v = View::new(ViewMode::Fit, 1.0);
        let t = v.transform(LOGICAL, (1920, 1400), 1.0);
        assert_eq!(t.scale, 1.0);
        assert_eq!(t.offset, (0.0, 175.0));
        assert_eq!(t.to_logical((960.0, 175.0 + 525.0)), (960.0, 525.0));
        let t = v.transform(LOGICAL, (3840, 2100), 2.0);
        assert_eq!((t.scale, t.offset), (2.0, (0.0, 0.0)));
        let mut v2 = v;
        v2.pan_by(-50.0, -50.0, LOGICAL, (1000, 600), 1.0);
        assert_eq!(
            v2.transform(LOGICAL, (1000, 600), 1.0),
            v.transform(LOGICAL, (1000, 600), 1.0),
            "fit never pans"
        );
    }

    #[test]
    fn one_to_one_pans_and_clamps() {
        let mut v = View::new(ViewMode::OneToOne, 1.0);
        let surface = (1000, 2000);
        let t = v.transform(LOGICAL, surface, 1.5);
        assert_eq!(t.scale, 1.5);
        assert_eq!(t.offset, (0.0, 213.0)); // y fits (1575 px) → centred
        v.pan_by(-500.0, -50.0, LOGICAL, surface, 1.5);
        assert_eq!(v.transform(LOGICAL, surface, 1.5).offset, (-500.0, 213.0));
        v.pan_by(-1e6, 0.0, LOGICAL, surface, 1.5);
        assert_eq!(v.transform(LOGICAL, surface, 1.5).offset.0, 1000.0 - 2880.0);
        let vis = v.transform(LOGICAL, surface, 1.5).visible(LOGICAL, surface);
        assert!((vis.max_x() - 1920.0).abs() < 1e-3 && vis.y == 0.0);
    }

    #[test]
    fn zoom_keeps_anchor() {
        let mut v = View::new(ViewMode::OneToOne, 1.0);
        let surface = (800, 600);
        v.pan_by(-300.0, -200.0, LOGICAL, surface, 1.0);
        let anchor = (400.0, 300.0);
        let p = v.transform(LOGICAL, surface, 1.0).to_logical(anchor);
        v.set_zoom(zoom_in(1.0), anchor, LOGICAL, surface, 1.0);
        let q = v.transform(LOGICAL, surface, 1.0).to_logical(anchor);
        assert!(
            (p.0 - q.0).abs() < 1.0 && (p.1 - q.1).abs() < 1.0,
            "{p:?} {q:?}"
        );
        assert_eq!(zoom_in(1.0), 1.1);
        assert_eq!(zoom_out(1.0), 0.9);
        assert_eq!(zoom_out(0.25), ZOOM_MIN);
        assert_eq!(zoom_in(4.0), ZOOM_MAX);
    }
}
