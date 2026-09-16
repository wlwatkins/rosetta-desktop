/// A rectangle in physical (device) pixels. Screen coordinates are virtual-desktop
/// relative, so `x`/`y` may be negative on multi-monitor setups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Build from two drag endpoints, normalizing so w/h are non-negative.
    pub fn from_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self {
            x: x0.min(x1),
            y: y0.min(y1),
            w: (x1 - x0).abs(),
            h: (y1 - y0).abs(),
        }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// Translate a rect expressed in this rect's local space into screen space.
    pub fn offset_by(&self, dx: i32, dy: i32) -> Self {
        Self { x: self.x + dx, y: self.y + dy, ..*self }
    }

    pub fn inflate(&self, d: i32) -> Self {
        Self { x: self.x - d, y: self.y - d, w: self.w + 2 * d, h: self.h + 2 * d }
    }

    pub fn area(&self) -> i64 {
        (self.w.max(0) as i64) * (self.h.max(0) as i64)
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        Self { x, y, w: (r - x).max(0), h: (b - y).max(0) }
    }

    pub fn intersects(&self, other: &Self) -> bool {
        !self.intersection(other).is_empty()
    }

    /// Intersection over union, used to decide whether two OCR boxes from
    /// consecutive scans describe the same line of text.
    pub fn iou(&self, other: &Self) -> f32 {
        let inter = self.intersection(other).area();
        if inter == 0 {
            return 0.0;
        }
        let union = self.area() + other.area() - inter;
        if union <= 0 {
            return 0.0;
        }
        inter as f32 / union as f32
    }
}
