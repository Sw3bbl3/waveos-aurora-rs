//! Automatic desktop arrangement in logical points, independent of display scale.
use core::cmp::Ordering;
pub const TILE_W: i32 = 112;
pub const TILE_H: i32 = 108;
#[derive(Clone, Copy)]
pub struct Grid {
    pub rows: usize,
    pub columns: usize,
    pub width: i32,
    pub top: i32,
}
impl Grid {
    pub fn new(width: i32, height: i32, menu_height: i32) -> Self {
        Self {
            rows: ((height - menu_height - 150) / TILE_H).max(1) as usize,
            columns: ((width - 24) / TILE_W).max(1) as usize,
            width,
            top: menu_height + 10,
        }
    }
    pub fn capacity(self) -> usize {
        self.rows * self.columns
    }
    pub fn position(self, slot: usize) -> (i32, i32) {
        (self.width - 16 - TILE_W * (slot / self.rows + 1) as i32, self.top + TILE_H * (slot % self.rows) as i32)
    }
    pub fn max_offset(self, count: usize) -> usize {
        count.saturating_sub(self.capacity()).div_ceil(self.rows) * self.rows
    }
    pub fn reveal(self, index: usize, offset: usize) -> usize {
        if index < offset {
            index / self.rows * self.rows
        } else if index >= offset + self.capacity() {
            (index / self.rows + 1) * self.rows - self.capacity()
        } else {
            offset
        }
    }
}
pub fn compare(sort: u8, a: (&str, bool, u64), b: (&str, bool, u64)) -> Ordering {
    let order = match sort {
        1 => b.1.cmp(&a.1).then_with(|| extension(a.0).cmp(extension(b.0))),
        2 => b.2.cmp(&a.2),
        _ => Ordering::Equal,
    };
    order.then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())).then_with(|| a.0.cmp(b.0))
}
fn extension(name: &str) -> &str {
    name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn columns_fill_from_upper_right() {
        let g = Grid::new(1280, 800, 54);
        assert_eq!(g.position(0), (1152, 64));
        assert_eq!(g.position(1), (1152, 172));
        assert_eq!(g.position(g.rows), (1040, 64));
        for i in 0..g.capacity() {
            let (x, y) = g.position(i);
            assert!(x >= 0 && y >= 54 && y + TILE_H <= 660);
        }
    }
    #[test]
    fn overflow_and_reveal() {
        for (w, h) in [(640, 480), (1280, 800), (1920, 1080)] {
            let g = Grid::new(w, h, 54);
            assert_eq!(g.max_offset(g.capacity()), 0);
            let n = g.capacity() * 3 + 1;
            let off = g.reveal(n - 1, 0);
            assert!(n > off && n - 1 < off + g.capacity());
            assert!(off <= g.max_offset(n));
            assert_eq!(g.reveal(0, off), 0);
        }
    }
    #[test]
    fn names_kind_and_modified() {
        assert_eq!(compare(0, ("alpha", false, 1), ("Beta", false, 2)), Ordering::Less);
        assert_eq!(compare(1, ("Folder", true, 0), ("a.png", false, 0)), Ordering::Less);
        assert_eq!(compare(2, ("a", false, 9), ("b", false, 2)), Ordering::Less);
        assert_eq!(compare(2, ("a", false, 2), ("b", false, 2)), Ordering::Less);
    }
}
