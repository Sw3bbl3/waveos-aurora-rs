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
