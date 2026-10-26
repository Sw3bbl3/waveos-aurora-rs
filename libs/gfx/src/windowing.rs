//! Host-tested window placement and stable MRU selection, independent of input/rendering.
use crate::geom::Rect;
use alloc::vec::Vec;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    #[default]
    Floating,
    Left,
    Right,
    Maximized,
    /// A Moorings zone: x, y, width and height in twelfths of the work area.
    Zone(u8, u8, u8, u8),
}

/// The six Moorings layouts offered while dragging a window, each a set of
/// zones (x, y, w, h in twelfths): halves, two thirds and a third (both
/// ways), thirds, quarters, and the whole area.
pub const MOORINGS: [&[(u8, u8, u8, u8)]; 6] = [
    &[(0, 0, 6, 12), (6, 0, 6, 12)],
    &[(0, 0, 8, 12), (8, 0, 4, 12)],
    &[(0, 0, 4, 12), (4, 0, 8, 12)],
    &[(0, 0, 4, 12), (4, 0, 4, 12), (8, 0, 4, 12)],
    &[(0, 0, 6, 6), (6, 0, 6, 6), (0, 6, 6, 6), (6, 6, 6, 6)],
    &[(0, 0, 12, 12)],
];

/// The placement for a Moorings zone (halves and the whole area keep their own names).
pub fn zone_placement(z: (u8, u8, u8, u8)) -> Placement {
    match z {
        (0, 0, 6, 12) => Placement::Left,
        (6, 0, 6, 12) => Placement::Right,
        (0, 0, 12, 12) => Placement::Maximized,
        (x, y, w, h) => Placement::Zone(x, y, w, h),
    }
}

/// A zone of `area`, leaving an 8-point gap between neighbouring zones.
fn zone(area: Rect, x: u8, y: u8, w: u8, h: u8) -> Rect {
    let (x, y, w, h) = (x.min(12) as i32, y.min(12) as i32, w.clamp(1, 12) as i32, h.clamp(1, 12) as i32);
    let mut left = area.x + area.w * x / 12;
    let mut right = area.x + area.w * (x + w).min(12) / 12;
    let mut top = area.y + area.h * y / 12;
    let mut bottom = area.y + area.h * (y + h).min(12) / 12;
    if x > 0 {
        left += 4;
    }
    if x + w < 12 {
        right -= 4;
    }
    if y > 0 {
        top += 4;
    }
    if y + h < 12 {
        bottom -= 4;
    }
    Rect::new(left, top, right - left, bottom - top)
}

pub fn target(p: Placement, area: Rect, minimum: (i32, i32)) -> Option<Rect> {
    let r = match p {
        Placement::Floating => return None,
        Placement::Left => zone(area, 0, 0, 6, 12),
        Placement::Right => zone(area, 6, 0, 6, 12),
        Placement::Maximized => area,
        Placement::Zone(x, y, w, h) => zone(area, x, y, w, h),
    };
    (r.w >= minimum.0 && r.h >= minimum.1).then_some(r)
}
pub fn clamp(r: Rect, area: Rect) -> Rect {
    let w = r.w.min(area.w);
    let h = r.h.min(area.h);
    Rect::new(r.x.clamp(area.x, area.right() - w), r.y.clamp(area.y, area.bottom() - h), w, h)
}
#[derive(Default)]
pub struct Switcher {
    pub ids: Vec<u32>,
    pub selected: usize,
}
impl Switcher {
    pub fn new(ids: Vec<u32>, reverse: bool) -> Self {
        let selected = if ids.len() < 2 {
            0
        } else if reverse {
            ids.len() - 1
        } else {
            1
        };
        Self { ids, selected }
    }
    pub fn step(&mut self, reverse: bool) {
        if !self.ids.is_empty() {
            self.selected = (self.selected + if reverse { self.ids.len() - 1 } else { 1 }) % self.ids.len();
        }
    }
    pub fn remove(&mut self, id: u32) {
        let selected = self.current();
        self.ids.retain(|v| *v != id);
        self.selected = selected
            .and_then(|s| self.ids.iter().position(|v| *v == s))
            .unwrap_or(self.selected.min(self.ids.len().saturating_sub(1)));
    }
    pub fn current(&self) -> Option<u32> {
        self.ids.get(self.selected).copied()
    }
}
/// Conservative dependency check: padded material bounds must be untouched.
pub fn safe_damage(damage: &[Rect], materials: &[Rect], padding: i32) -> bool {
    damage.iter().all(|r| materials.iter().all(|m| !r.intersects(&m.inset(-padding))))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placements_fit_and_reject_small_targets() {
        let a = Rect::new(0, 30, 1280, 690);
        let l = target(Placement::Left, a, (320, 200)).unwrap();
        let r = target(Placement::Right, a, (320, 200)).unwrap();
        assert_eq!(r.x - l.right(), 8);
        assert_eq!(r.right(), a.right());
        assert!(target(Placement::Left, a, (700, 200)).is_none());
        assert_eq!(clamp(Rect::new(2000, 1000, 800, 600), a), Rect::new(480, 120, 800, 600));
    }
    #[test]
    fn moorings_zones_tile_the_area() {
        let a = Rect::new(8, 62, 1264, 730);
        for layout in MOORINGS {
            let rects: alloc::vec::Vec<Rect> =
                layout.iter().map(|&z| target(zone_placement(z), a, (1, 1)).unwrap()).collect();
            // Every zone is inside the area and no two overlap.
            for (i, r) in rects.iter().enumerate() {
                assert_eq!(r.intersect(&a), *r);
                for o in &rects[i + 1..] {
                    assert!(!r.intersects(o), "{r:?} overlaps {o:?}");
                }
            }
            // Together they reach every edge.
            assert_eq!(rects.iter().map(|r| r.x).min(), Some(a.x));
            assert_eq!(rects.iter().map(|r| r.right()).max(), Some(a.right()));
            assert_eq!(rects.iter().map(|r| r.bottom()).max(), Some(a.bottom()));
        }
        assert_eq!(zone_placement((0, 0, 6, 12)), Placement::Left);
    }

    #[test]
    fn switch_order_and_closed_selection() {
        let mut s = Switcher::new(alloc::vec![3, 1, 2], false);
        assert_eq!(s.current(), Some(1));
        s.step(true);
        assert_eq!(s.current(), Some(3));
        s.remove(3);
        assert_eq!(s.current(), Some(1));
        s.remove(1);
        s.remove(2);
        assert_eq!(s.current(), None);
    }
    #[test]
    fn blur_dependencies_expand_beyond_surface() {
        let m = [Rect::new(0, 0, 100, 30)];
        assert!(!safe_damage(&[Rect::new(20, 40, 5, 5)], &m, 40));
        assert!(safe_damage(&[Rect::new(20, 80, 5, 5)], &m, 40));
    }
    #[test]
    fn localized_reconstruction_matches_full_reference() {
        use crate::{canvas::Canvas, material::MaterialCache};
        fn draw(pixels: &mut [u32], clip: Rect, value: u32) {
            let mut cache = MaterialCache::default();
            let mut cv = Canvas::new(pixels, 160, 160);
            cv.clip = clip;
            cv.materials = Some(&mut cache);
            cv.fill_rect(Rect::new(0, 0, 160, 160), 0xff102040);
            cv.frosted(Rect::new(0, 0, 160, 24), 6, 0x80607080);
            cv.fill_round_rect(Rect::new(12, 65, 136, 85), 8, 0xffeeeeee);
            cv.fill_rect(Rect::new(30, 90, 45, 20), value);
        }
        let full = Rect::new(0, 0, 160, 160);
        let damage = Rect::new(30, 90, 45, 20);
        let mut partial = alloc::vec![0;160*160];
        draw(&mut partial, full, 0xff123456);
        assert!(safe_damage(&[damage], &[Rect::new(0, 0, 160, 24)], 40));
        draw(&mut partial, damage, 0xffabcdef);
        let mut reference = alloc::vec![0;160*160];
        draw(&mut reference, full, 0xffabcdef);
        assert_eq!(partial, reference);
    }
}
