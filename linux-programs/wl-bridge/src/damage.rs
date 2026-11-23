//! Conservative bounded damage rectangles, in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}
impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w: w.max(0), h: h.max(0) }
    }
    pub fn right(self) -> i32 {
        self.x.saturating_add(self.w)
    }
    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.h)
    }
    pub fn empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn intersect(self, b: Self) -> Self {
        let (x, y) = (self.x.max(b.x), self.y.max(b.y));
        Self::new(x, y, self.right().min(b.right()).saturating_sub(x), self.bottom().min(b.bottom()).saturating_sub(y))
    }
    pub fn union(self, b: Self) -> Self {
        if self.empty() {
            return b;
        }
        if b.empty() {
            return self;
        }
        let (x, y) = (self.x.min(b.x), self.y.min(b.y));
        Self::new(x, y, self.right().max(b.right()).saturating_sub(x), self.bottom().max(b.bottom()).saturating_sub(y))
    }
    pub fn scaled(self, k: i32) -> Self {
        Self::new(
            self.x.saturating_mul(k),
            self.y.saturating_mul(k),
            self.w.saturating_mul(k),
            self.h.saturating_mul(k),
        )
    }
    pub fn translated(self, x: i32, y: i32) -> Self {
        Self::new(self.x.saturating_add(x), self.y.saturating_add(y), self.w, self.h)
    }
    pub fn points(self, k: i32) -> Self {
        let k = k.max(1);
        let (x, y) = (self.x.div_euclid(k), self.y.div_euclid(k));
        Self::new(
            x,
            y,
            (self.right().saturating_add(k - 1)).div_euclid(k) - x,
            (self.bottom().saturating_add(k - 1)).div_euclid(k) - y,
        )
    }
}
pub fn add(target: &mut Option<Rect>, r: Rect) {
    if !r.empty() {
        *target = Some(target.map_or(r, |a| a.union(r)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clip_overflow_and_rounding() {
        let bounds = Rect::new(0, 0, 20, 20);
        assert_eq!(Rect::new(-5, -5, 10, 10).intersect(bounds), Rect::new(0, 0, 5, 5));
        assert!(Rect::new(i32::MAX, 1, i32::MAX, 2).intersect(bounds).empty());
        assert_eq!(Rect::new(3, 5, 2, 2).points(2), Rect::new(1, 2, 2, 2));
        let mut r = None;
        add(&mut r, Rect::new(1, 2, 3, 4));
        add(&mut r, Rect::new(5, 6, 2, 2));
        assert_eq!(r, Some(Rect::new(1, 2, 6, 6)));
    }
}
