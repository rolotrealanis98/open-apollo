/// Rectangle in logical points (or image pixels where noted).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn max_x(&self) -> f32 {
        self.x + self.w
    }
    pub fn max_y(&self) -> f32 {
        self.y + self.h
    }
    pub fn offset(&self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn inset(&self, l: f32, t: f32, r: f32, b: f32) -> Self {
        Self::new(self.x + l, self.y + t, (self.w - l - r).max(0.0), (self.h - t - b).max(0.0))
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.max_x() && y < self.max_y()
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    /// A `w`×`h` rect centred in this one.
    pub fn centered(&self, w: f32, h: f32) -> Self {
        Self::new(self.x + (self.w - w) / 2.0, self.y + (self.h - h) / 2.0, w, h)
    }
}

/// `l,t,r,b` insets (image pixels for 9-slice).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Insets {
    pub const fn uniform(v: f32) -> Self {
        Self { l: v, t: v, r: v, b: v }
    }
}

/// Straight-alpha colour, components 0…1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

    pub const fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Parses `"R,G,B"` or `"R,G,B,A"` (RGB 0–255, A 0–1).
    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<f32> = text.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>().ok()?;
        match parts.as_slice() {
            [r, g, b] => Some(Self { r: r / 255.0, g: g / 255.0, b: b / 255.0, a: 1.0 }),
            [r, g, b, a] => Some(Self { r: r / 255.0, g: g / 255.0, b: b / 255.0, a: *a }),
            _ => None,
        }
    }
}
