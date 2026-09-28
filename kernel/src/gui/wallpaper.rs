//! Procedural wallpapers: a night sky with aurora ribbons over layered waves.
//! Pure integer math, resolution independent (the same code renders the
//! full-screen wallpaper and the thumbnails in Settings).

use super::canvas::{blend, mix, rgb};
use super::math::{sin, Rng};
use alloc::vec;
use alloc::vec::Vec;

pub const NAMES: [&str; 3] = ["Aurora", "Ocean", "Sunset"];

struct Ribbon {
    color: u32,
    /// Base height, per mille of screen height.
    base: i32,
    amp1: i32,
    freq1: i32,
    phase1: i32,
    amp2: i32,
    freq2: i32,
    up: i32,
    down: i32,
    strength: i32,
}

struct Palette {
    sky_top: u32,
    sky_bottom: u32,
    ribbons: [Ribbon; 3],
    waves: [u32; 3],
    crest: u32,
    stars: bool,
}

const fn ribbon(
    color: u32,
    base: i32,
    amp1: i32,
    freq1: i32,
    phase1: i32,
    amp2: i32,
    freq2: i32,
    up: i32,
    down: i32,
    strength: i32,
) -> Ribbon {
    Ribbon { color, base, amp1, freq1, phase1, amp2, freq2, up, down, strength }
}

static PALETTES: [Palette; 3] = [
    Palette {
        sky_top: rgb(0x05, 0x08, 0x1C),
        sky_bottom: rgb(0x14, 0x22, 0x4E),
        ribbons: [
            ribbon(rgb(0x3B, 0xF5, 0xA8), 400, 70, 900, 0, 30, 2300, 190, 26, 230),
            ribbon(rgb(0x2D, 0xD4, 0xBF), 320, 55, 1300, 300, 25, 3100, 150, 22, 170),
            ribbon(rgb(0x9B, 0x5C, 0xFF), 250, 60, 700, 650, 35, 1900, 170, 30, 190),
        ],
        waves: [rgb(0x12, 0x1E, 0x4A), rgb(0x0C, 0x16, 0x3A), rgb(0x07, 0x0E, 0x28)],
        crest: rgb(0x6E, 0xE7, 0xD0),
        stars: true,
    },
    Palette {
        sky_top: rgb(0x02, 0x16, 0x2C),
        sky_bottom: rgb(0x0A, 0x55, 0x78),
        ribbons: [
            ribbon(rgb(0x5E, 0xEA, 0xD4), 380, 60, 800, 100, 30, 2100, 200, 30, 170),
            ribbon(rgb(0x60, 0xA5, 0xFA), 300, 50, 1100, 500, 30, 2700, 160, 26, 160),
            ribbon(rgb(0xC4, 0xE8, 0xFF), 220, 40, 600, 800, 25, 1700, 120, 20, 90),
        ],
        waves: [rgb(0x0E, 0x5E, 0x86), rgb(0x08, 0x46, 0x6C), rgb(0x04, 0x2C, 0x4C)],
        crest: rgb(0xB8, 0xF4, 0xFF),
        stars: true,
    },
    Palette {
        sky_top: rgb(0x2A, 0x0F, 0x46),
        sky_bottom: rgb(0xF4, 0x7A, 0x60),
        ribbons: [
            ribbon(rgb(0xFD, 0xBA, 0x74), 430, 60, 800, 200, 25, 2200, 200, 30, 150),
            ribbon(rgb(0xF4, 0x72, 0xB6), 330, 55, 1000, 600, 30, 2600, 180, 26, 170),
            ribbon(rgb(0xFF, 0xE0, 0x8A), 250, 40, 600, 900, 20, 1800, 120, 20, 90),
        ],
        waves: [rgb(0x7A, 0x2E, 0x6E), rgb(0x55, 0x1E, 0x5A), rgb(0x33, 0x12, 0x40)],
        crest: rgb(0xFF, 0xC8, 0xA8),
        stars: false,
    },
];

fn add(dst: u32, src: u32, amount: i32) -> u32 {
    let a = amount.clamp(0, 255) as u32;
    let ch = |s: u32| (((dst >> s) & 0xFF) + (((src >> s) & 0xFF) * a >> 8)).min(255) << s;
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

/// Renders wallpaper `index` at `w`×`h`.
pub fn generate(index: u8, w: i32, h: i32) -> Vec<u32> {
    let p = &PALETTES[index as usize % PALETTES.len()];
    let mut px = vec![0u32; (w * h) as usize];
    // Normalised coordinates: everything is designed for 1280×800.
    let nx = |x: i32| x * 1280 / w.max(1);
    let sy = |v: i32| v * h / 800;

    // Sky gradient (slightly curved so the horizon glows).
    for y in 0..h {
        let t = y * 256 / h;
        let row = mix(p.sky_top, p.sky_bottom, t * t / 256);
        px[(y * w) as usize..((y + 1) * w) as usize].fill(row);
    }

    // Stars.
    if p.stars {
        let mut rng = Rng(0x5EED_0A0B_0C0D_0E0F);
        let count = w * h / 3000;
        for _ in 0..count {
            let x = rng.range(w as u32);
            let y = rng.range((h * 6 / 10).max(1) as u32);
            let b = 60 + rng.range(196);
            let i = (y * w + x) as usize;
            px[i] = add(px[i], 0xFFFF_FFFF, b);
            if b > 220 && x + 1 < w && y + 1 < h {
                for j in [i + 1, i + w as usize] {
                    px[j] = add(px[j], 0xFFFF_FFFF, b / 3);
                }
            }
        }
    }

    // Aurora ribbons: bright lower edge, long soft curtain rising above it.
    for r in &p.ribbons {
        let mut rng = Rng(0xA0A0 ^ r.phase1 as u64 ^ (r.freq1 as u64) << 20);
        let mut streak = 0i32;
        for x in 0..w {
            let xn = nx(x);
            let center = sy(r.base * 800 / 1000)
                + sy(r.amp1) * sin(xn * r.freq1 / 1000 + r.phase1) / 16384
                + sy(r.amp2) * sin(xn * r.freq2 / 1000 + r.phase1 * 3) / 16384;
            // Vertical "curtain" streaks: slow sine plus a little per-column noise.
            streak = (streak * 3 + rng.range(90)) / 4;
            let rays = 150 + 70 * sin(xn * 13 / 2 + r.phase1) / 16384 + streak / 2;
            let up = sy(r.up) * (220 + sin(xn * 3 + r.phase1) / 200) / 256;
            let down = sy(r.down).max(1);
            let top = (center - up).max(0);
            let bottom = (center + down).min(h - 1);
            for y in top..=bottom {
                let dy = y - center;
                let t = if dy < 0 { -dy * 256 / up.max(1) } else { dy * 256 / down };
                let fall = (256 - t).max(0);
                let fall = if dy < 0 { fall * fall / 256 } else { fall * fall * fall / 65536 };
                let intensity = fall * rays / 256 * r.strength / 256;
                let i = (y * w + x) as usize;
                px[i] = add(px[i], r.color, intensity);
            }
        }
    }

    // Layered waves along the bottom, with anti-aliased crests.
    let layers = [(220, 26, 700, 0), (150, 22, 1000, 400), (80, 18, 1400, 800)];
    for (li, &(height, amp, freq, phase)) in layers.iter().enumerate() {
        let color = p.waves[li];
        let hl = sy(height).max(1);
        for x in 0..w {
            let xn = nx(x);
            // Edge position in 1/256 px for sub-pixel accuracy.
            let edge256 = (h - sy(height)) * 256
                + sy(amp) * sin(xn * freq / 1000 + phase) / 64
                + sy(amp / 3) * sin(xn * freq * 5 / 2000 + phase / 2) / 64;
            let edge256 = edge256.clamp(0, h * 256);
            let edge = edge256 >> 8;
            let frac = (256 - (edge256 & 0xFF)) as u32; // coverage of the edge pixel
            for y in edge..h {
                let i = (y * w + x) as usize;
                let depth = ((y - edge) * 256 / hl).min(256);
                let c = mix(color, 0xFF02_0410, depth / 2);
                let a = if y == edge { 235 * frac / 256 } else { 235 };
                px[i] = blend(px[i], c, a);
            }
            // Soft crest glow just above the edge.
            for k in 1..4 {
                let y = edge - k;
                if y >= 0 {
                    let i = (y * w + x) as usize;
                    let a = [0, 34, 16, 6][k as usize] * (3 - li as u32) / 3;
                    px[i] = blend(px[i], p.crest, a);
                }
            }
            if edge < h {
                let i = (edge * w + x) as usize;
                px[i] = blend(px[i], p.crest, 60 * (3 - li as u32) / 3 * frac / 256);
            }
        }
    }
    px
}

/// Separable box blur (two passes per axis ≈ triangle filter).
pub fn blur(src: &[u32], w: i32, h: i32, radius: i32) -> Vec<u32> {
    let mut a = src.to_vec();
    let mut b = vec![0u32; src.len()];
    for _ in 0..2 {
        box_pass(&a, &mut b, w, h, radius, true);
        box_pass(&b, &mut a, w, h, radius, false);
    }
    a
}

fn box_pass(src: &[u32], dst: &mut [u32], w: i32, h: i32, r: i32, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let idx = |line: i32, i: i32| -> usize {
        let i = i.clamp(0, len - 1);
        if horizontal {
            (line * w + i) as usize
        } else {
            (i * w + line) as usize
        }
    };
    let n = (2 * r + 1) as u32;
    for line in 0..lines {
        let (mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32);
        for i in -r..=r {
            let c = src[idx(line, i)];
            sr += (c >> 16) & 0xFF;
            sg += (c >> 8) & 0xFF;
            sb += c & 0xFF;
        }
        for i in 0..len {
            dst[idx(line, i)] = 0xFF00_0000 | (sr / n) << 16 | (sg / n) << 8 | sb / n;
            let out = src[idx(line, i - r)];
            let inn = src[idx(line, i + r + 1)];
            sr = sr + ((inn >> 16) & 0xFF) - ((out >> 16) & 0xFF);
            sg = sg + ((inn >> 8) & 0xFF) - ((out >> 8) & 0xFF);
            sb = sb + (inn & 0xFF) - (out & 0xFF);
        }
    }
}
