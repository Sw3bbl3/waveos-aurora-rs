//! WAVEOS pointer family. All shapes use logical hotspots and smooth vector
//! geometry at every display scale. Activity is driven by real shell/client state.
use super::canvas::Canvas;
use super::geom::Rect;
use aurora_abi::cursor as c;

const INK: u32 = 0xFF202630;
const PAPER: u32 = 0xFFFFFFFF;

pub fn bounds(x: i32, y: i32) -> Rect {
    Rect::new(x - 17, y - 17, 49, 51)
}
pub fn animated(shape: u32) -> bool {
    matches!(shape, c::WAIT | c::PROGRESS)
}
pub fn resize_shape(edges: u8) -> u32 {
    match edges {
        5 | 10 => c::RESIZE_NWSE,
        6 | 9 => c::RESIZE_NESW,
        1 | 2 => c::RESIZE_EW,
        4 | 8 => c::RESIZE_NS,
        _ => c::ARROW,
    }
}

/// A rounded stroke with a readable light outline and a soft offset shadow.
fn strokes(cv: &mut Canvas, x: i32, y: i32, paths: &[&[(i32, i32)]]) {
    for (offset, width, color) in [(1, 5, 0x40000000), (0, 4, PAPER), (0, 2, INK)] {
        for path in paths {
            for pair in path.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                cv.line(x + a.0 + offset, y + a.1 + offset, x + b.0 + offset, y + b.1 + offset, width, color);
            }
            for &(px, py) in *path {
                cv.fill_circle(x + px + offset, y + py + offset, width / 2, color);
            }
        }
    }
}

fn arrow(cv: &mut Canvas, x: i32, y: i32) {
    const OUTER: [(i32, i32); 9] =
        [(0, 4), (0, 258), (16, 265), (76, 205), (122, 309), (166, 290), (121, 190), (205, 190), (207, 173)];
    const INNER: [(i32, i32); 7] = [(20, 47), (20, 226), (82, 170), (133, 282), (145, 276), (96, 169), (171, 169)];
    fill_polygon(cv, &OUTER, x * 16 + 12, y * 16 + 20, 0x50000000);
    fill_polygon(cv, &OUTER, x * 16, y * 16, PAPER);
    fill_polygon(cv, &INNER, x * 16, y * 16, INK);
}

fn hand(cv: &mut Canvas, x: i32, y: i32, closed: bool, pointing: bool) {
    // Round-ended fingers make the family read cleanly even at 1x.
    let palm = Rect::new(x - 5, y + 7, 15, 12);
    cv.shadow(palm, 5, 3, 1, 65);
    cv.fill_round_rect(palm.inset(-1), 5, PAPER);
    cv.fill_round_rect(palm, 4, INK);
    let tips = if closed {
        [6, 5, 5, 7]
    } else if pointing {
        [0, 7, 9, 11]
    } else {
        [2, 0, 1, 4]
    };
    for (n, top) in tips.into_iter().enumerate() {
        let fx = x - 3 + n as i32 * 4;
        cv.line(fx, y + top, fx, y + 12, 4, PAPER);
        cv.fill_circle(fx, y + top, 2, PAPER);
        cv.line(fx, y + top + 1, fx, y + 12, 2, INK);
        cv.fill_circle(fx, y + top + 1, 1, INK);
    }
    strokes(cv, x, y, &[&[(-6, 13), (-9, 8), (-11, 9), (-7, 17)]]);
}

fn activity(cv: &mut Canvas, x: i32, y: i32, now: u64) {
    let frame = if super::theme::reduce_motion() { 0 } else { (now / 80 % 8) as usize };
    let dots = [(0, -6), (4, -4), (6, 0), (4, 4), (0, 6), (-4, 4), (-6, 0), (-4, -4)];
    cv.fill_circle(x, y, 9, PAPER);
    cv.fill_circle(x, y, 8, INK);
    for (i, (dx, dy)) in dots.into_iter().enumerate() {
        let alpha = (70 + ((i + 8 - frame) % 8) * 24) as u8;
        cv.fill_circle(x + dx, y + dy, 1, super::canvas::with_alpha(0xFF90C8FF, alpha));
    }
}

struct Cached {
    shape: u32,
    scale: i32,
    frame: u64,
    pixels: alloc::vec::Vec<u32>,
}
static CACHE: spin::Mutex<alloc::vec::Vec<Cached>> = spin::Mutex::new(alloc::vec::Vec::new());

/// The display shows the pointer on its cursor plane: the compositor
/// neither draws it into the picture nor repaints where it moved.
static HARDWARE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn set_hardware(on: bool) {
    HARDWARE.store(on, core::sync::atomic::Ordering::Relaxed);
}

pub fn hardware() -> bool {
    HARDWARE.load(core::sync::atomic::Ordering::Relaxed)
}

/// The activity frame `shape` shows at `now` (0 for still shapes).
pub fn frame(shape: u32, now: u64) -> u64 {
    if animated(shape) && !super::theme::reduce_motion() {
        now / 80 % 8
    } else {
        0
    }
}

/// A cursor-plane image: 0xAARRGGBB pixels (straight alpha), its size,
/// and the hot spot inside it.
pub struct Image {
    pub pixels: alloc::vec::Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub hot: (i32, i32),
}

/// The pointer `shape` (activity frame `frame`) at `scale` as a
/// cursor-plane image of at most `max`×`max` pixels. `None` if the shape
/// is larger than the plane at this scale (the compositor then draws it).
pub fn image(shape: u32, frame: u64, scale: i32, max: u32) -> Option<Image> {
    let (w, h) = (49 * scale, 51 * scale);
    let mut full = alloc::vec![0u32; (w * h) as usize];
    let mut cv = Canvas::new(&mut full, w, h).with_scale(scale);
    draw_vector(&mut cv, 17, 17, shape, frame * 80);
    // Crop to what was drawn.
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if full[(y * w + x) as usize] >> 24 != 0 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x1 <= x0 {
        // Nothing to show (hidden): one transparent pixel.
        return Some(Image { pixels: alloc::vec![0], width: 1, height: 1, hot: (0, 0) });
    }
    let (cw, ch) = ((x1 - x0) as u32, (y1 - y0) as u32);
    if cw > max || ch > max {
        return None;
    }
    let mut out = alloc::vec![0u32; (cw * ch) as usize];
    for y in 0..ch as i32 {
        let src = ((y0 + y) * w + x0) as usize;
        out[(y as u32 * cw) as usize..((y as u32 + 1) * cw) as usize].copy_from_slice(&full[src..src + cw as usize]);
    }
    Some(Image { pixels: out, width: cw, height: ch, hot: (17 * scale - x0, 17 * scale - y0) })
}

/// Rasterize each vector once per scale and activity frame, then move a small
/// transparent bitmap. Pointer motion never rebuilds vector geometry.
/// (Nothing when the display shows the pointer on its cursor plane.)
pub fn draw(cv: &mut Canvas, x: i32, y: i32, shape: u32, now: u64) {
    if shape == c::HIDDEN || hardware() {
        return;
    }
    let frame = frame(shape, now);
    let scale = cv.scale;
    let mut cache = CACHE.lock();
    let at = cache.iter().position(|p| p.shape == shape && p.scale == scale && p.frame == frame);
    let at = match at {
        Some(i) => i,
        None => {
            let mut pixels = alloc::vec![0; (49 * scale * 51 * scale) as usize];
            let mut canvas = Canvas::new(&mut pixels, 49 * scale, 51 * scale).with_scale(scale);
            draw_vector(&mut canvas, 17, 17, shape, frame * 80);
            // Current supported scales need at most 60 entries, including both
            // activity animations. Bound the cache if more scales are added.
            if cache.len() >= 64 {
                cache.remove(0);
            }
            let i = cache.len();
            cache.push(Cached { shape, scale, frame, pixels });
            i
        }
    };
    cv.blit_alpha(&cache[at].pixels, (49 * scale) as u32, (51 * scale) as u32, bounds(x, y), 0);
}

fn draw_vector(cv: &mut Canvas, x: i32, y: i32, shape: u32, now: u64) {
    match shape {
        c::HIDDEN => {}
        c::TEXT => strokes(cv, x, y, &[&[(-3, -8), (3, -8)], &[(0, -8), (0, 8)], &[(-3, 8), (3, 8)]]),
        c::POINTER => hand(cv, x, y, false, true),
        c::GRAB => hand(cv, x, y - 8, false, false),
        c::GRABBING => hand(cv, x, y - 8, true, false),
        c::CROSSHAIR => strokes(cv, x, y, &[&[(-8, 0), (8, 0)], &[(0, -8), (0, 8)]]),
        c::RESIZE_EW => {
            strokes(cv, x, y, &[&[(-9, 0), (9, 0)], &[(-5, -4), (-9, 0), (-5, 4)], &[(5, -4), (9, 0), (5, 4)]])
        }
        c::RESIZE_NS => {
            strokes(cv, x, y, &[&[(0, -9), (0, 9)], &[(-4, -5), (0, -9), (4, -5)], &[(-4, 5), (0, 9), (4, 5)]])
        }
        c::RESIZE_NWSE => {
            strokes(cv, x, y, &[&[(-7, -7), (7, 7)], &[(-7, -2), (-7, -7), (-2, -7)], &[(2, 7), (7, 7), (7, 2)]])
        }
        c::RESIZE_NESW => {
            strokes(cv, x, y, &[&[(-7, 7), (7, -7)], &[(-7, 2), (-7, 7), (-2, 7)], &[(2, -7), (7, -7), (7, -2)]])
        }
        c::MOVE => strokes(
            cv,
            x,
            y,
            &[
                &[(-9, 0), (9, 0)],
                &[(0, -9), (0, 9)],
                &[(-6, -3), (-9, 0), (-6, 3)],
                &[(6, -3), (9, 0), (6, 3)],
                &[(-3, -6), (0, -9), (3, -6)],
                &[(-3, 6), (0, 9), (3, 6)],
            ],
        ),
        c::NOT_ALLOWED => {
            cv.fill_circle(x, y, 10, PAPER);
            cv.fill_circle(x, y, 8, INK);
            cv.fill_circle(x, y, 5, PAPER);
            cv.line(x - 5, y + 5, x + 5, y - 5, 3, INK);
        }
        c::WAIT => activity(cv, x, y, now),
        c::PROGRESS => {
            arrow(cv, x, y);
            activity(cv, x + 19, y + 17, now);
        }
        c::COPY => {
            arrow(cv, x, y);
            cv.fill_round_rect(Rect::new(x + 10, y + 13, 14, 14), 4, PAPER);
            cv.fill_round_rect(Rect::new(x + 11, y + 14, 12, 12), 3, 0xFF267B68);
            cv.line(x + 17, y + 17, x + 17, y + 23, 2, PAPER);
            cv.line(x + 14, y + 20, x + 20, y + 20, 2, PAPER);
        }
        _ => arrow(cv, x, y),
    }
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
