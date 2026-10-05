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
}
pub fn target(p: Placement, area: Rect, minimum: (i32, i32)) -> Option<Rect> {
    let r = match p {
        Placement::Floating => return None,
        Placement::Left => Rect::new(area.x, area.y, area.w / 2 - 4, area.h),
        Placement::Right => Rect::new(area.x + area.w / 2 + 4, area.y, area.w - area.w / 2 - 4, area.h),
        Placement::Maximized => area,
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
