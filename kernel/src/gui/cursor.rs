//! The mouse pointer: the arrow, and the shapes a window can ask for over
//! it (`aurora_abi::cursor`).

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

/// An I-beam, over text.
const TEXT: [&str; 17] = [
    "XXX XXX", "X..X..X", "XXX.XXX", "  X.X  ", "  X.X  ", "  X.X  ", "  X.X  ", "  X.X  ", "  X.X  ", "  X.X  ",
    "  X.X  ", "  X.X  ", "  X.X  ", "  X.X  ", "XXX.XXX", "X..X..X", "XXX XXX",
];

/// A pointing hand, over links and buttons.
const HAND: [&str; 19] = [
    "     XX          ",
    "    X..X         ",
    "    X..X         ",
    "    X..X         ",
    "    X..XXX       ",
    "    X..X..XXX    ",
    "    X..X..X..XX  ",
    "XXX X..X..X..X.X ",
    "X..XX........X..X",
    "X...X...........X",
    " X..............X",
    "  X.............X",
    "  X............X ",
    "   X...........X ",
    "   X..........X  ",
    "    X.........X  ",
    "    X........X   ",
    "     X.......X   ",
    "     XXXXXXXXX   ",
];

const CROSS: [&str; 17] = [
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "XXXXXXXX.XXXXXXXX",
    ".................",
    "XXXXXXXX.XXXXXXXX",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
    "       X.X       ",
];

/// An hourglass, while busy.
const WAIT: [&str; 17] = [
    "XXXXXXXXXXX",
    "X.........X",
    "XXXXXXXXXXX",
    " X.......X ",
    " X.......X ",
    "  X.....X  ",
    "   X...X   ",
    "    X.X    ",
    "    X.X    ",
    "    X.X    ",
    "   X...X   ",
    "  X.....X  ",
    " X.......X ",
    " X.......X ",
    "XXXXXXXXXXX",
    "X.........X",
    "XXXXXXXXXXX",
];

/// A double arrow, left and right (drawn turned for up and down).
const RESIZE: [&str; 9] = [
    "    X         X    ",
    "   X.X       X.X   ",
    "  X..X       X..X  ",
    " X...XXXXXXXXX...X ",
    "X.................X",
    " X...XXXXXXXXX...X ",
    "  X..X       X..X  ",
    "   X.X       X.X   ",
    "    X         X    ",
];

/// Room around the hot spot that every shape fits in.
pub fn bounds(x: i32, y: i32) -> Rect {
    Rect::new(x - 12, y - 12, 34, 36)
}

/// Draws the pointer of shape `shape` with its hot spot at (`x`, `y`).
pub fn draw(cv: &mut Canvas, x: i32, y: i32, shape: u32) {
    use aurora_abi::cursor as c;
    match shape {
        c::TEXT => art(cv, &TEXT, x - 3, y - 8, false),
        c::POINTER => art(cv, &HAND, x - 5, y, false),
        c::CROSSHAIR => art(cv, &CROSS, x - 8, y - 8, false),
        c::WAIT => art(cv, &WAIT, x - 5, y - 8, false),
        c::NOT_ALLOWED => not_allowed(cv, x, y),
        c::RESIZE_EW => art(cv, &RESIZE, x - 9, y - 4, false),
        c::RESIZE_NS => art(cv, &RESIZE, x - 4, y - 9, true),
        c::MOVE => {
            art(cv, &RESIZE, x - 9, y - 4, false);
            art(cv, &RESIZE, x - 4, y - 9, true);
            // The two arrows' bodies meet in the middle.
            art(cv, &["."], x, y, false);
        }
        c::HIDDEN => {}
        _ => arrow(cv, x, y),
    }
}

/// Pixel art: `X` white outline, `.` black body, with a soft shadow; each
/// art pixel is a point (a square of device pixels on HiDPI screens).
/// `turned` swaps rows and columns.
fn art(cv: &mut Canvas, rows: &[&str], x: i32, y: i32, turned: bool) {
    for pass in 0..2 {
        for (r, line) in rows.iter().enumerate() {
            for (col, ch) in line.bytes().enumerate() {
                if ch == b' ' {
                    continue;
                }
                let (dx, dy) = if turned { (r as i32, col as i32) } else { (col as i32, r as i32) };
                let (px, py, color) = match pass {
                    0 => (x + dx + 1, y + dy + 1, 0x5000_0000),
                    _ => (x + dx, y + dy, if ch == b'X' { 0xFFFF_FFFF } else { 0xFF10_1014 }),
                };
                cv.fill_rect(Rect::new(px, py, 1, 1), color);
            }
        }
    }
}

/// A circle with a bar across it.
fn not_allowed(cv: &mut Canvas, x: i32, y: i32) {
    for pass in 0..2 {
        for dy in -8i32..=8 {
            for dx in -8i32..=8 {
                let d2 = dx * dx + dy * dy;
                let bar = (dx + dy).abs() <= 1 && d2 <= 25;
                let color = if (25..=49).contains(&d2) || bar {
                    0xFF10_1014
                } else if (16..25).contains(&d2) || (50..=64).contains(&d2) || ((dx + dy).abs() == 2 && d2 < 25) {
                    0xFFFF_FFFF
                } else {
                    continue;
                };
                let (px, py, color) =
                    if pass == 0 { (x + dx + 1, y + dy + 1, 0x5000_0000) } else { (x + dx, y + dy, color) };
                cv.fill_rect(Rect::new(px, py, 1, 1), color);
            }
        }
    }
}

fn arrow(cv: &mut Canvas, x: i32, y: i32) {
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
