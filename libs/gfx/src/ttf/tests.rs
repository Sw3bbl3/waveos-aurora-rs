//! Cross-checks the TrueType engine against fontdue (a mature rasterizer).

use super::{raster, TrueType};

const FONTS: [&str; 3] = ["Inter-Regular.ttf", "Inter-SemiBold.ttf", "JetBrainsMono-Regular.ttf"];
const TEXT: &str = "AaBbGgJjQqRrWwYy0123456789&@%$#?!éüñ“”—…→";

fn load(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../assets/fonts/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn metrics_and_advances_match() {
    for name in FONTS {
        let data = load(name);
        let tt = TrueType::parse(&data).expect("parse");
        let fd = fontdue::Font::from_bytes(&data[..], fontdue::FontSettings::default()).unwrap();
        assert_eq!(tt.num_glyphs as usize, fd.glyph_count() as usize, "{name}: glyph count");
        for c in TEXT.chars() {
            let g = tt.glyph_index(c);
            assert_eq!(g, fd.lookup_glyph_index(c), "{name}: glyph for {c:?}");
            let ours = tt.advance(g) as f32 * 20.0 / tt.units_per_em as f32;
            let theirs = fd.metrics(c, 20.0).advance_width;
            assert!((ours - theirs).abs() < 0.01, "{name}: advance of {c:?}: {ours} vs {theirs}");
        }
        let lm = fd.horizontal_line_metrics(20.0).unwrap();
        let asc = tt.ascender as f32 * 20.0 / tt.units_per_em as f32;
        assert!((asc - lm.ascent).abs() < 0.01, "{name}: ascent {asc} vs {}", lm.ascent);
    }
}

/// Coverage of pixel (x, y) (y up, relative to the baseline) of a mask.
fn at(m: &raster::Mask, x: i32, y: i32) -> i32 {
    let (lx, ly) = (x - m.x_min as i32, (m.y_min as i32 + m.height as i32 - 1) - y);
    if lx < 0 || ly < 0 || lx >= m.width as i32 || ly >= m.height as i32 {
        0
    } else {
        m.data[(ly * m.width as i32 + lx) as usize] as i32
    }
}

/// Exact-area coverage must equal the average of a 4× finer rendering.
#[test]
fn coverage_is_exact_area() {
    for name in FONTS {
        let data = load(name);
        let tt = TrueType::parse(&data).unwrap();
        for px in [11u32, 14, 20, 40] {
            for c in TEXT.chars() {
                let o = tt.outline(tt.glyph_index(c));
                let m = raster::render(&o, tt.units_per_em, px, 0);
                let big = raster::render(&o, tt.units_per_em, px * 4, 0);
                let (mut diff, mut total, mut max) = (0i64, 0i64, 0);
                for y in m.y_min as i32 - 1..=m.y_min as i32 + m.height as i32 {
                    for x in m.x_min as i32 - 1..=m.x_min as i32 + m.width as i32 {
                        let mut sum = 0;
                        for sy in 0..4 {
                            for sx in 0..4 {
                                sum += at(&big, 4 * x + sx, 4 * y + sy);
                            }
                        }
                        let d = (at(&m, x, y) - sum / 16).abs();
                        diff += d as i64;
                        total += at(&m, x, y) as i64;
                        max = max.max(d);
                    }
                }
                let rel = diff as f64 / total.max(1) as f64;
                assert!(max <= 12 && rel < 0.02, "{name} {px}px {c:?}: max {max}, relative {:.2}%", rel * 100.0);
            }
        }
    }
}

/// And it looks like fontdue's (placement identical, coverage within a few %).
#[test]
fn rasters_match_fontdue() {
    for name in FONTS {
        let data = load(name);
        let tt = TrueType::parse(&data).unwrap();
        let fd = fontdue::Font::from_bytes(&data[..], fontdue::FontSettings::default()).unwrap();
        for px in [11u32, 14, 20, 40, 96] {
            for c in TEXT.chars() {
                let ours = raster::render(&tt.outline(tt.glyph_index(c)), tt.units_per_em, px, 0);
                let (m, data) = fd.rasterize(c, px as f32);
                let theirs = raster::Mask {
                    width: m.width as u16,
                    height: m.height as u16,
                    x_min: m.xmin as i16,
                    y_min: m.ymin as i16,
                    data,
                };
                // Same placement, give or take a pixel of box rounding.
                assert!(
                    (ours.x_min - theirs.x_min).abs() <= 1
                        && (ours.y_min - theirs.y_min).abs() <= 1
                        && (ours.width as i32 - theirs.width as i32).abs() <= 1
                        && (ours.height as i32 - theirs.height as i32).abs() <= 1,
                    "{name} {px}px {c:?}: placement"
                );
                let (mut diff, mut total) = (0i64, 0i64);
                for y in theirs.y_min as i32 - 2..theirs.y_min as i32 + theirs.height as i32 + 2 {
                    for x in theirs.x_min as i32 - 2..theirs.x_min as i32 + theirs.width as i32 + 2 {
                        diff += (at(&ours, x, y) - at(&theirs, x, y)).abs() as i64;
                        total += at(&theirs, x, y) as i64;
                    }
                }
                let rel = diff as f64 / total.max(1) as f64;
                assert!(rel < 0.10, "{name} {px}px {c:?}: coverage differs by {:.1}%", rel * 100.0);
            }
        }
    }
}

#[test]
fn kerning_is_read() {
    let data = load("Inter-Regular.ttf");
    let tt = TrueType::parse(&data).unwrap();
    let (a, v) = (tt.glyph_index('A'), tt.glyph_index('V'));
    let (t, o) = (tt.glyph_index('T'), tt.glyph_index('o'));
    assert!(tt.kerning(a, v) < 0, "AV should kern tighter: {}", tt.kerning(a, v));
    assert!(tt.kerning(t, o) < 0, "To should kern tighter: {}", tt.kerning(t, o));
}
