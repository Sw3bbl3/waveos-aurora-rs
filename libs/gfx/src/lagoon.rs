//! Lagoon's geometry: spreading windows out so each can be seen whole,
//! and moving between them with the arrow keys.
//!
//! [`spread`] lays pictures of windows out in rows inside an area, each
//! keeping its shape, as large as the area allows but never larger than
//! the window itself, with room under every picture for its name. The
//! number of rows is whichever makes the pictures largest. The order is
//! the caller's (reading order of where the windows are, so the layout
//! resembles the desktop).

use crate::geom::Rect;
use alloc::vec::Vec;

/// Thousandths, for aspect ratios.
const K: i64 = 1000;

/// Places windows of `sizes` (width, height) inside `area`, `gap` apart,
/// leaving `label` points under each for its name. The result is in the
/// order of `sizes`: picture rectangles (the label goes below each).
pub fn spread(sizes: &[(i32, i32)], area: Rect, gap: i32, label: i32) -> Vec<Rect> {
    let n = sizes.len();
    if n == 0 || area.w <= 0 || area.h <= 0 {
        return alloc::vec![Rect::default(); n];
    }
    let sizes: Vec<(i64, i64)> = sizes.iter().map(|&(w, h)| (w.max(1) as i64, h.max(1) as i64)).collect();
    let aspect: Vec<i64> = sizes.iter().map(|&(w, h)| w * K / h).collect();
    // (Extreme shapes count as merely very wide or tall when rows are made.)
    let weight: Vec<i64> = aspect.iter().map(|a| (*a).clamp(K / 5, K * 5)).collect();
    let mut best: Option<(i64, Vec<Rect>)> = None;
    for rows in 1..=n {
        let split = rows_of(&weight, rows);
        // The tallest row height that fits every row's width and all rows' height.
        let count = split.len() as i64;
        let mut height = (area.h as i64 - gap as i64 * (count - 1)) / count - label as i64;
        for row in &split {
            let room = area.w as i64 - gap as i64 * (row.len() as i64 - 1);
            // Widths are rounded down, so a height that fits by aspect fits.
            let sum: i64 = row.iter().map(|&i| aspect[i]).sum();
            height = height.min(room * K / sum.max(1));
        }
        if height < 8 {
            continue;
        }
        // Never larger than the window itself.
        let tile = |i: usize| -> (i32, i32) {
            let (sw, sh) = sizes[i];
            let h = height.min(sh);
            let w = if h == sh { sw } else { (h * sw / sh).max(1) };
            (w as i32, h as i32)
        };
        let score: i64 = (0..n)
            .map(|i| {
                let (w, h) = tile(i);
                w as i64 * h as i64
            })
            .sum();
        if best.as_ref().is_some_and(|(s, _)| *s >= score) {
            continue;
        }
        // Rows as tall as their tallest picture, centred as a block.
        let heights: Vec<i32> = split.iter().map(|row| row.iter().map(|&i| tile(i).1).max().unwrap_or(0)).collect();
        let block: i32 = heights.iter().map(|h| h + label).sum::<i32>() + gap * (count as i32 - 1);
        let mut y = area.y + (area.h - block) / 2;
        let mut out = alloc::vec![Rect::default(); n];
        for (row, rh) in split.iter().zip(&heights) {
            let width: i32 = row.iter().map(|&i| tile(i).0).sum::<i32>() + gap * (row.len() as i32 - 1);
            let mut x = area.x + (area.w - width) / 2;
            for &i in row {
                let (w, h) = tile(i);
                out[i] = Rect::new(x, y + (rh - h) / 2, w, h);
                x += w + gap;
            }
            y += rh + label + gap;
        }
        best = Some((score, out));
    }
    best.map(|(_, r)| r).unwrap_or_else(|| alloc::vec![Rect::default(); n])
}

/// Splits items (by their aspect ratios, in order) into about `rows` rows
/// of similar total width. Rows may come out fewer, never empty.
fn rows_of(aspect: &[i64], rows: usize) -> Vec<Vec<usize>> {
    let total: i64 = aspect.iter().sum::<i64>().max(1);
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut before = 0i64;
    let mut last = usize::MAX;
    for (i, &a) in aspect.iter().enumerate() {
        // The row this item's middle falls in.
        let row = (((before + a / 2) * rows as i64 / total) as usize).min(rows - 1);
        if row != last {
            out.push(Vec::new());
            last = row;
        }
        out.last_mut().unwrap().push(i);
        before += a;
    }
    out
}

/// Indices of `rects` in reading order: top to bottom in bands of
/// `band` points, then left to right.
pub fn reading_order(rects: &[Rect], band: i32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rects.len()).collect();
    let band = band.max(1);
    order.sort_by_key(|&i| {
        let (cx, cy) = rects[i].center();
        (cy.div_euclid(band), cx, i)
    });
    order
}

/// The rectangle to go to from `rects[from]` with an arrow key (`dx`,
/// `dy` one of the four directions): the nearest one lying that way,
/// distance along the way counting less than distance across it.
pub fn neighbour(rects: &[Rect], from: usize, (dx, dy): (i32, i32)) -> Option<usize> {
    let (fx, fy) = rects.get(from)?.center();
    rects
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != from)
        .filter_map(|(i, r)| {
            let (cx, cy) = r.center();
            let (along, across) = ((cx - fx) * dx + (cy - fy) * dy, ((cx - fx) * dy).abs() + ((cy - fy) * dx).abs());
            (along > 0).then_some((along as i64 + 2 * across as i64, i))
        })
        .min()
        .map(|(_, i)| i)
}

/// `a` to `b` by `t` thousandths (past either end too, for a spring's overshoot).
pub fn lerp_rect(a: Rect, b: Rect, t: i32) -> Rect {
    let l = |p: i32, q: i32| p + ((q - p) as i64 * t as i64 / 1000) as i32;
    Rect::new(l(a.x, b.x), l(a.y, b.y), l(a.w, b.w).max(1), l(a.h, b.h).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disjoint(r: &[Rect]) -> bool {
        r.iter().enumerate().all(|(i, a)| r[i + 1..].iter().all(|b| !a.intersects(b)))
    }

    #[test]
    fn one_window_is_centred_and_never_enlarged() {
        let area = Rect::new(0, 0, 1000, 700);
        let r = spread(&[(400, 300)], area, 20, 30);
        assert_eq!((r[0].w, r[0].h), (400, 300));
        assert_eq!(r[0].x, 300);
        // A huge window shrinks to fit, keeping its shape.
        let r = spread(&[(4000, 2000)], area, 20, 30);
        assert!(r[0].w <= 1000 && r[0].h + 30 <= 700, "{:?}", r[0]);
        assert!((r[0].w * 1000 / r[0].h - 2000).abs() < 20);
    }

    #[test]
    fn many_windows_fit_without_overlapping_and_keep_their_shapes() {
        let area = Rect::new(40, 100, 1200, 600);
        for n in 1..=12 {
            let sizes: Vec<(i32, i32)> = (0..n).map(|i| (600 + (i % 3) * 200, 400 + (i % 2) * 150)).collect();
            let r = spread(&sizes, area, 24, 28);
            assert!(disjoint(&r), "{n}: {r:?}");
            for (s, t) in sizes.iter().zip(&r) {
                assert!(
                    area.contains(t.x, t.y) && t.right() <= area.right() && t.bottom() + 28 <= area.bottom(),
                    "{n}: {t:?}"
                );
                let (want, got) = (s.0 as i64 * 1000 / s.1 as i64, t.w as i64 * 1000 / t.h as i64);
                assert!((want - got).abs() * 100 < want * 4, "{n}: shape {s:?} became {t:?}");
            }
        }
        // Eight windows in a wide area take more than one row and more than one column.
        let r = spread(&[(800, 600); 8], area, 24, 28);
        let rows = r.iter().map(|t| t.y).collect::<alloc::collections::BTreeSet<_>>().len();
        assert!((2..=4).contains(&rows), "{rows} rows");
    }

    #[test]
    fn rows_are_never_empty() {
        for rows in 1..6 {
            let split = rows_of(&[3000, 500, 500, 500, 2000], rows);
            assert!(split.iter().all(|r| !r.is_empty()));
            assert_eq!(split.iter().map(Vec::len).sum::<usize>(), 5);
        }
    }

    #[test]
    fn arrows_go_to_the_nearest_window_that_way() {
        let grid: Vec<Rect> = (0..6).map(|i| Rect::new((i % 3) * 300, (i / 3) * 250, 200, 150)).collect();
        assert_eq!(neighbour(&grid, 0, (1, 0)), Some(1));
        assert_eq!(neighbour(&grid, 1, (0, 1)), Some(4));
        assert_eq!(neighbour(&grid, 4, (-1, 0)), Some(3));
        assert_eq!(neighbour(&grid, 0, (-1, 0)), None);
        assert_eq!(neighbour(&grid, 5, (0, -1)), Some(2));
        assert_eq!(reading_order(&[grid[4], grid[0], grid[2]], 100), alloc::vec![1, 2, 0]);
    }
}
