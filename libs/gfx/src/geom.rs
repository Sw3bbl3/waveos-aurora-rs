//! Integer rectangles.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }
    /// The parts of `self` outside `o`: up to four bands (above, below,
    /// left, right), empty ones included.
    pub fn subtract(&self, o: &Rect) -> [Rect; 4] {
        let i = self.intersect(o);
        if i.is_empty() {
            return [*self, Rect::default(), Rect::default(), Rect::default()];
        }
        [
            Rect::new(self.x, self.y, self.w, i.y - self.y),
            Rect::new(self.x, i.bottom(), self.w, self.bottom() - i.bottom()),
            Rect::new(self.x, i.y, i.x - self.x, i.h),
            Rect::new(i.right(), i.y, self.right() - i.right(), i.h),
        ]
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }
    /// Insets by `dx` on the left and right and `dy` on the top and bottom.
    pub fn inset_xy(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w - 2 * dx, self.h - 2 * dy)
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    pub fn area(&self) -> i64 {
        self.w.max(0) as i64 * self.h.max(0) as i64
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::Rect;

    fn area(parts: &[Rect]) -> i64 {
        parts.iter().map(|p| p.area()).sum()
    }

    #[test]
    fn subtract_leaves_what_is_outside() {
        let a = Rect::new(0, 0, 100, 80);
        // A hole in the middle: four bands around it.
        let parts = a.subtract(&Rect::new(20, 10, 30, 40));
        assert_eq!(area(&parts), 100 * 80 - 30 * 40);
        for (i, p) in parts.iter().enumerate() {
            assert!(!p.intersects(&Rect::new(20, 10, 30, 40)));
            for q in &parts[i + 1..] {
                assert!(!p.intersects(q));
            }
        }
        // Disjoint: unchanged. Covering: nothing.
        assert_eq!(area(&a.subtract(&Rect::new(200, 0, 10, 10))), a.area());
        assert_eq!(area(&a.subtract(&Rect::new(-5, -5, 200, 200))), 0);
        // A sidebar on the left edge.
        let parts = a.subtract(&Rect::new(0, 0, 25, 80));
        assert_eq!(parts.iter().filter(|p| !p.is_empty()).count(), 1);
        assert_eq!(area(&parts), 75 * 80);
    }
}
