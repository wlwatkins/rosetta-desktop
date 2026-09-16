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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_points_normalizes_any_drag_direction() {
        let expected = Rect::new(10, 20, 30, 40);
        // All four drag directions must describe the same rectangle.
        assert_eq!(Rect::from_points(10, 20, 40, 60), expected);
        assert_eq!(Rect::from_points(40, 60, 10, 20), expected);
        assert_eq!(Rect::from_points(40, 20, 10, 60), expected);
        assert_eq!(Rect::from_points(10, 60, 40, 20), expected);
    }

    #[test]
    fn edges_and_emptiness() {
        let r = Rect::new(5, 7, 10, 20);
        assert_eq!((r.right(), r.bottom()), (15, 27));
        assert!(!r.is_empty());
        assert!(Rect::new(0, 0, 0, 5).is_empty());
        assert!(Rect::new(0, 0, 5, 0).is_empty());
        assert!(Rect::new(0, 0, -3, 5).is_empty());
    }

    #[test]
    fn contains_excludes_the_far_edges() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(0, 0));
        assert!(r.contains(9, 9));
        // right() and bottom() are one past the last pixel.
        assert!(!r.contains(10, 5));
        assert!(!r.contains(5, 10));
        assert!(!r.contains(-1, 5));
    }

    #[test]
    fn negative_origins_work() {
        // The virtual desktop starts at a negative x on a left-hand monitor.
        let r = Rect::new(-1920, 0, 1920, 1080);
        assert_eq!(r.right(), 0);
        assert!(r.contains(-1000, 500));
        assert!(!r.contains(0, 500));
    }

    #[test]
    fn inflate_grows_on_every_side() {
        let r = Rect::new(10, 10, 20, 20).inflate(5);
        assert_eq!(r, Rect::new(5, 5, 30, 30));
        assert_eq!(Rect::new(10, 10, 20, 20).inflate(-2), Rect::new(12, 12, 16, 16));
    }

    #[test]
    fn offset_by_round_trips() {
        let r = Rect::new(3, 4, 10, 10);
        assert_eq!(r.offset_by(100, 200).offset_by(-100, -200), r);
    }

    #[test]
    fn intersection_of_overlapping_rects() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersection(&b), Rect::new(5, 5, 5, 5));
        assert!(a.intersects(&b));
    }

    #[test]
    fn disjoint_rects_do_not_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 5, 5);
        assert!(a.intersection(&b).is_empty());
        assert!(!a.intersects(&b));
        // Touching edges share no pixels.
        assert!(!a.intersects(&Rect::new(10, 0, 5, 10)));
    }

    #[test]
    fn iou_of_identical_rects_is_one() {
        let a = Rect::new(2, 3, 8, 9);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_of_disjoint_rects_is_zero() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.iou(&Rect::new(50, 50, 10, 10)), 0.0);
    }

    #[test]
    fn iou_of_half_overlap() {
        // 10x10 over 10x10 sharing a 5x10 strip: 50 / (100 + 100 - 50).
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 0, 10, 10);
        assert!((a.iou(&b) - (50.0 / 150.0)).abs() < 1e-6);
    }

    #[test]
    fn iou_survives_large_coordinates() {
        // area() is i64 precisely so a 4K-wide rect cannot overflow.
        let a = Rect::new(0, 0, 3840, 2160);
        assert_eq!(a.area(), 8_294_400);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
    }
}
