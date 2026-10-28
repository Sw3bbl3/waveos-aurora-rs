//! The mouse pointer.

use super::canvas::Canvas;
use super::geom::Rect;

const ARROW: [&str; 19] = [
    "X           ",
    "XX          ",
    "X.X         ",
    "X..X        ",
    "X...X       ",
    "X....X      ",
    "X.....X     ",
    "X......X    ",
    "X.......X   ",
    "X........X  ",
    "X.........X ",
    "X......XXXXX",
    "X...X..X    ",
    "X..XX..X    ",
    "X.X  X..X   ",
    "XX   X..X   ",
    "X     X..X  ",
    "      X..X  ",
    "       XX   ",
];

pub const W: i32 = 14;
pub const H: i32 = 21;

pub fn bounds(x: i32, y: i32) -> Rect {
    Rect::new(x - 1, y - 1, W + 2, H + 2)
}

pub fn draw(cv: &mut Canvas, x: i32, y: i32) {
    if cv.scale > 1 {
        return draw_vector(cv, x, y);
    }
    // Soft shadow first, then white outline and black body.
    for pass in 0..2 {
        for (row, line) in ARROW.iter().enumerate() {
            for (col, ch) in line.bytes().enumerate() {
                if ch == b' ' {
                    continue;
                }
                let (px, py, c) = match pass {
                    0 => (x + col as i32 + 1, y + row as i32 + 1, 0x5000_0000),
                    _ => (x + col as i32, y + row as i32, if ch == b'X' { 0xFFFF_FFFF } else { 0xFF10_1014 }),
                };
                cv.fill_rect(Rect::new(px, py, 1, 1), c);
            }
        }
    }
}

/// The arrow's outline and body, in 1/16 point from its tip.
const OUTER: [(i32, i32); 7] = [(0, 0), (0, 272), (67, 211), (112, 317), (160, 298), (117, 197), (203, 197)];
const INNER: [(i32, i32); 7] = [(19, 46), (19, 227), (74, 178), (122, 290), (139, 282), (91, 174), (158, 174)];

/// The arrow drawn at the screen's full resolution (HiDPI).
fn draw_vector(cv: &mut Canvas, x: i32, y: i32) {
    fill_polygon(cv, &OUTER, x * 16 + 16, y * 16 + 20, 0x5000_0000);
    fill_polygon(cv, &OUTER, x * 16, y * 16, 0xFFFF_FFFF);
    fill_polygon(cv, &INNER, x * 16, y * 16, 0xFF10_1014);
}

/// Fills a polygon (vertices in 1/16 point, offset by `ox`, `oy`) with
/// 4×4 samples per device pixel.
fn fill_polygon(cv: &mut Canvas, pts: &[(i32, i32)], ox: i32, oy: i32, c: u32) {
    let k = cv.scale;
    // Device pixels, in 1/16 pixel.
    let v: alloc::vec::Vec<(i64, i64)> =
        pts.iter().map(|&(px, py)| (((ox + px) * k) as i64, ((oy + py) * k) as i64)).collect();
    let (x0, x1) = (v.iter().map(|p| p.0).min().unwrap() >> 4, (v.iter().map(|p| p.0).max().unwrap() >> 4) + 1);
    let (y0, y1) = (v.iter().map(|p| p.1).min().unwrap() >> 4, (v.iter().map(|p| p.1).max().unwrap() >> 4) + 1);
    let area = Rect::new(x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32).intersect(&cv.pclip());
    let inside = |sx: i64, sy: i64| {
        let mut odd = false;
        let mut j = v.len() - 1;
        for i in 0..v.len() {
            let ((xi, yi), (xj, yj)) = (v[i], v[j]);
            if (yi > sy) != (yj > sy) && sx < xi + (xj - xi) * (sy - yi) / (yj - yi) {
                odd = !odd;
            }
            j = i;
        }
        odd
    };
    let alpha = c >> 24;
    for py in area.y..area.bottom() {
        for px in area.x..area.right() {
            let mut n = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    n += inside(px as i64 * 16 + 2 + sx * 4, py as i64 * 16 + 2 + sy * 4) as u32;
                }
            }
            if n > 0 {
                let i = (py * cv.width + px) as usize;
                cv.buf[i] = super::canvas::blend(cv.buf[i], c, alpha * n / 16);
            }
        }
    }
}
