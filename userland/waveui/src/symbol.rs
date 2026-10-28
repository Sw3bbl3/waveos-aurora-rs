//! The built-in symbols, drawn as strokes on a 16-unit grid so they stay
//! crisp at any size and take the colour of the text around them.

use crate::view::Symbol;
use lumen::canvas::Canvas;
use lumen::geom::Rect;

/// An arc in whole pixels (the canvas takes 1/16 px).
#[allow(clippy::too_many_arguments)]
fn arc(cv: &mut Canvas, cx: i32, cy: i32, r: i32, w: i32, start: i32, sweep: i32, c: u32) {
    cv.stroke_arc(cx * 16, cy * 16, r * 16, w * 16, start, sweep, c);
}

pub(crate) fn draw(cv: &mut Canvas, s: Symbol, r: Rect, c: u32) {
    let u = |v: i32, len: i32| v * len / 16;
    let p = |x: i32, y: i32| (r.x + u(x, r.w), r.y + u(y, r.h));
    let w = ((r.w + 4) / 9).max(1);
    let line = |cv: &mut Canvas, pts: &[(i32, i32)]| {
        let pts: alloc::vec::Vec<(i32, i32)> = pts.iter().map(|&(x, y)| p(x, y)).collect();
        cv.polyline(&pts, w, c);
    };
    let circle = |cv: &mut Canvas, x: i32, y: i32, rad: i32| {
        let (cx, cy) = p(x, y);
        arc(cv, cx, cy, u(rad, r.w), w, 0, 1024, c);
    };
    let dot = |cv: &mut Canvas, x: i32, y: i32, rad: i32| {
        let (cx, cy) = p(x, y);
        cv.fill_circle(cx, cy, u(rad, r.w).max(1), c);
    };
    let rect = |cv: &mut Canvas, x: i32, y: i32, x2: i32, y2: i32, rad: i32| {
        let (a, b) = p(x, y);
        let (e, f) = p(x2, y2);
        let rr = Rect::new(a, b, e - a, f - b);
        for i in 0..w {
            cv.stroke_round_rect(rr.inset(i), (u(rad, r.w) - i).max(0), c);
        }
    };
    let fill = |cv: &mut Canvas, x: i32, y: i32, x2: i32, y2: i32, rad: i32| {
        let (a, b) = p(x, y);
        let (e, f) = p(x2, y2);
        cv.fill_round_rect(Rect::new(a, b, e - a, f - b), u(rad, r.w), c);
    };
    match s {
        Symbol::ChevronLeft => line(cv, &[(10, 3), (5, 8), (10, 13)]),
        Symbol::ChevronRight => line(cv, &[(6, 3), (11, 8), (6, 13)]),
        Symbol::ChevronDown => line(cv, &[(3, 6), (8, 11), (13, 6)]),
        Symbol::ChevronUp => line(cv, &[(3, 10), (8, 5), (13, 10)]),
        Symbol::Check => line(cv, &[(3, 8), (6, 12), (13, 4)]),
        Symbol::Plus => {
            line(cv, &[(8, 3), (8, 13)]);
            line(cv, &[(3, 8), (13, 8)]);
        }
        Symbol::Minus => line(cv, &[(3, 8), (13, 8)]),
        Symbol::Close => {
            line(cv, &[(4, 4), (12, 12)]);
            line(cv, &[(12, 4), (4, 12)]);
        }
        Symbol::Search => {
            circle(cv, 7, 7, 4);
            line(cv, &[(10, 10), (14, 14)]);
        }
        Symbol::More => {
            for x in [3, 8, 13] {
                dot(cv, x, 8, 1);
            }
        }
        Symbol::Star | Symbol::StarFill => {
            let pts = [(8, 1), (10, 6), (15, 6), (11, 9), (13, 15), (8, 11), (3, 15), (5, 9), (1, 6), (6, 6), (8, 1)];
            if s == Symbol::StarFill {
                let (cx, cy) = p(8, 9);
                cv.fill_circle(cx, cy, u(4, r.w), c);
            }
            line(cv, &pts);
        }
        Symbol::Info => {
            circle(cv, 8, 8, 7);
            dot(cv, 8, 5, 1);
            line(cv, &[(8, 7), (8, 12)]);
        }
        Symbol::Warning => {
            line(cv, &[(8, 2), (15, 14), (1, 14), (8, 2)]);
            line(cv, &[(8, 6), (8, 10)]);
            dot(cv, 8, 12, 1);
        }
        Symbol::Trash => {
            line(cv, &[(2, 4), (14, 4)]);
            line(cv, &[(6, 4), (6, 2), (10, 2), (10, 4)]);
            line(cv, &[(4, 4), (5, 14), (11, 14), (12, 4)]);
        }
        Symbol::Share => {
            line(cv, &[(5, 6), (3, 6), (3, 14), (13, 14), (13, 6), (11, 6)]);
            line(cv, &[(8, 10), (8, 1)]);
            line(cv, &[(5, 4), (8, 1), (11, 4)]);
        }
        Symbol::Refresh => {
            let (cx, cy) = p(8, 8);
            arc(cv, cx, cy, u(5, r.w), w, 900, 770, c);
            line(cv, &[(13, 2), (13, 6), (9, 6)]);
        }
        Symbol::Play => {
            let (x0, y0) = p(5, 3);
            let (x1, y1) = p(13, 8);
            let (_, y2) = p(5, 13);
            for y in y0..=y2 {
                let t = if y <= y1 { y - y0 } else { y2 - y };
                let span = t * (x1 - x0) / (y1 - y0).max(1);
                cv.fill_rect(Rect::new(x0, y, span.max(1), 1), c);
            }
        }
        Symbol::Pause => {
            fill(cv, 4, 3, 7, 13, 1);
            fill(cv, 9, 3, 12, 13, 1);
        }
        Symbol::Stop => fill(cv, 3, 3, 13, 13, 2),
        Symbol::Flag => {
            line(cv, &[(4, 15), (4, 2)]);
            line(cv, &[(4, 2), (13, 2), (11, 5), (13, 8), (4, 8)]);
        }
        Symbol::Gear => {
            circle(cv, 8, 8, 5);
            dot(cv, 8, 8, 2);
            for (x0, y0, x1, y1) in [(8, 1, 8, 3), (8, 13, 8, 15), (1, 8, 3, 8), (13, 8, 15, 8)] {
                line(cv, &[(x0, y0), (x1, y1)]);
            }
            for (x0, y0, x1, y1) in [(3, 3, 4, 4), (12, 12, 13, 13), (3, 13, 4, 12), (12, 4, 13, 3)] {
                line(cv, &[(x0, y0), (x1, y1)]);
            }
        }
        Symbol::Home => {
            line(cv, &[(1, 8), (8, 2), (15, 8)]);
            line(cv, &[(3, 7), (3, 14), (13, 14), (13, 7)]);
            line(cv, &[(7, 14), (7, 10), (9, 10), (9, 14)]);
        }
        Symbol::Grid => {
            for (x, y) in [(2, 2), (9, 2), (2, 9), (9, 9)] {
                rect(cv, x, y, x + 5, y + 5, 1);
            }
        }
        Symbol::List => {
            for y in [4, 8, 12] {
                dot(cv, 2, y, 1);
                line(cv, &[(5, y), (14, y)]);
            }
        }
        Symbol::Folder => {
            line(cv, &[(1, 13), (1, 3), (6, 3), (8, 5), (15, 5), (15, 13), (1, 13)]);
            line(cv, &[(1, 7), (15, 7)]);
        }
        Symbol::Document => {
            line(cv, &[(3, 1), (10, 1), (13, 4), (13, 15), (3, 15), (3, 1)]);
            line(cv, &[(10, 1), (10, 4), (13, 4)]);
            line(cv, &[(5, 8), (11, 8)]);
            line(cv, &[(5, 11), (11, 11)]);
        }
        Symbol::Globe => {
            circle(cv, 8, 8, 7);
            line(cv, &[(1, 8), (15, 8)]);
            let (cx, cy) = p(8, 8);
            arc(cv, cx - u(3, r.w), cy, u(7, r.w) * 3 / 4, w, 1024 - 150, 300, c);
            arc(cv, cx + u(3, r.w), cy, u(7, r.w) * 3 / 4, w, 512 - 150, 300, c);
        }
        Symbol::Lock => {
            fill(cv, 3, 7, 13, 15, 2);
            let (cx, cy) = p(8, 7);
            arc(cv, cx, cy, u(3, r.w), w, 512, 512, c);
            line(cv, &[(5, 7), (5, 6)]);
            line(cv, &[(11, 7), (11, 6)]);
        }
        Symbol::Person => {
            circle(cv, 8, 5, 3);
            let (cx, cy) = p(8, 15);
            arc(cv, cx, cy, u(6, r.w), w, 512, 512, c);
        }
        Symbol::Bell => {
            let (cx, cy) = p(8, 8);
            arc(cv, cx, cy, u(4, r.w), w, 512, 512, c);
            line(cv, &[(4, 8), (4, 11), (2, 13), (14, 13), (12, 11), (12, 8)]);
            line(cv, &[(7, 15), (9, 15)]);
        }
        Symbol::Moon => {
            let (cx, cy) = p(8, 8);
            arc(cv, cx, cy, u(6, r.w), w, 100, 700, c);
            arc(cv, cx + u(3, r.w), cy - u(2, r.w), u(5, r.w), w, 280, 500, c);
        }
        Symbol::Sun => {
            circle(cv, 8, 8, 3);
            for (x0, y0, x1, y1) in [
                (8, 1, 8, 3),
                (8, 13, 8, 15),
                (1, 8, 3, 8),
                (13, 8, 15, 8),
                (3, 3, 4, 4),
                (12, 12, 13, 13),
                (3, 13, 4, 12),
                (12, 4, 13, 3),
            ] {
                line(cv, &[(x0, y0), (x1, y1)]);
            }
        }
        Symbol::Wifi => {
            let (cx, cy) = p(8, 14);
            for rad in [4, 8, 12] {
                arc(cv, cx, cy, u(rad, r.w), w, 640, 256, c);
            }
            dot(cv, 8, 14, 1);
        }
        Symbol::Speaker => {
            line(cv, &[(2, 6), (5, 6), (9, 2), (9, 14), (5, 10), (2, 10), (2, 6)]);
            let (cx, cy) = p(9, 8);
            arc(cv, cx, cy, u(4, r.w), w, 1024 - 128, 256, c);
        }
        Symbol::Keyboard => {
            rect(cv, 1, 4, 15, 13, 2);
            for x in [4, 7, 10, 12] {
                dot(cv, x, 7, 0);
            }
            line(cv, &[(5, 10), (11, 10)]);
        }
        Symbol::Mouse => {
            rect(cv, 4, 1, 12, 15, 4);
            line(cv, &[(8, 3), (8, 6)]);
        }
        Symbol::Gamepad => {
            rect(cv, 1, 4, 15, 12, 4);
            line(cv, &[(3, 8), (7, 8)]);
            line(cv, &[(5, 6), (5, 10)]);
            dot(cv, 11, 7, 1);
            dot(cv, 13, 9, 1);
        }
        Symbol::Photo => {
            rect(cv, 2, 3, 14, 13, 1);
            line(cv, &[(2, 11), (6, 7), (9, 10), (10, 9), (14, 12)]);
        }
        Symbol::Terminal => {
            rect(cv, 1, 2, 15, 14, 2);
            line(cv, &[(4, 6), (7, 8), (4, 10)]);
            line(cv, &[(8, 11), (12, 11)]);
        }
        Symbol::Drive => {
            rect(cv, 1, 5, 15, 12, 2);
            dot(cv, 4, 8, 1);
            line(cv, &[(8, 8), (12, 8)]);
        }
        Symbol::Display => {
            rect(cv, 1, 2, 15, 11, 1);
            line(cv, &[(8, 11), (8, 14)]);
            line(cv, &[(5, 14), (11, 14)]);
        }
        Symbol::Battery => {
            rect(cv, 1, 4, 13, 12, 2);
            fill(cv, 3, 6, 9, 10, 1);
            line(cv, &[(15, 7), (15, 9)]);
        }
        Symbol::Clock | Symbol::Timer => {
            circle(cv, 8, if s == Symbol::Timer { 9 } else { 8 }, if s == Symbol::Timer { 6 } else { 7 });
            if s == Symbol::Timer {
                line(cv, &[(6, 1), (10, 1)]);
                line(cv, &[(8, 9), (8, 5)]);
            } else {
                line(cv, &[(8, 4), (8, 8), (11, 10)]);
            }
        }
        Symbol::Stopwatch => {
            circle(cv, 8, 9, 6);
            line(cv, &[(8, 1), (8, 3)]);
            line(cv, &[(8, 9), (11, 6)]);
        }
        Symbol::Download => {
            line(cv, &[(8, 1), (8, 11)]);
            line(cv, &[(4, 7), (8, 11), (12, 7)]);
            line(cv, &[(2, 14), (14, 14)]);
        }
        Symbol::Sidebar => {
            rect(cv, 1, 2, 15, 14, 2);
            line(cv, &[(6, 2), (6, 14)]);
        }
        Symbol::Pencil => {
            line(cv, &[(3, 13), (4, 9), (11, 2), (14, 5), (7, 12), (3, 13)]);
            line(cv, &[(9, 4), (12, 7)]);
        }
        Symbol::Eraser => {
            line(cv, &[(2, 10), (9, 3), (14, 8), (8, 14), (5, 14), (2, 11)]);
            line(cv, &[(5, 7), (11, 12)]);
        }
        Symbol::Circle => circle(cv, 8, 8, 6),
        Symbol::Square => rect(cv, 2, 2, 14, 14, 1),
        Symbol::Line => line(cv, &[(2, 14), (14, 2)]),
        Symbol::Text => {
            line(cv, &[(3, 4), (3, 2), (13, 2), (13, 4)]);
            line(cv, &[(8, 2), (8, 14)]);
            line(cv, &[(6, 14), (10, 14)]);
        }
        Symbol::Undo => {
            let (cx, cy) = p(9, 10);
            arc(cv, cx, cy, u(5, r.w), w, 768, 512, c);
            line(cv, &[(6, 2), (3, 5), (6, 8)]);
            line(cv, &[(3, 5), (9, 5)]);
        }
        Symbol::Redo => {
            let (cx, cy) = p(7, 10);
            arc(cv, cx, cy, u(5, r.w), w, 512 + 256 - 512, 512, c);
            line(cv, &[(10, 2), (13, 5), (10, 8)]);
            line(cv, &[(13, 5), (7, 5)]);
        }
        Symbol::Copy => {
            rect(cv, 5, 5, 14, 14, 2);
            line(cv, &[(3, 11), (2, 11), (2, 2), (11, 2), (11, 3)]);
        }
        Symbol::Eject => {
            line(cv, &[(2, 9), (8, 3), (14, 9), (2, 9)]);
            line(cv, &[(2, 13), (14, 13)]);
        }
        Symbol::Calendar => {
            rect(cv, 1, 3, 15, 15, 2);
            line(cv, &[(1, 7), (15, 7)]);
            line(cv, &[(5, 1), (5, 4)]);
            line(cv, &[(11, 1), (11, 4)]);
        }
    }
}
