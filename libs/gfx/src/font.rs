//! Fonts: TrueType faces rendered at any size, with a shared glyph cache.
//!
//! At start-up the window server (from the system image) and every app
//! [`install`] the UI faces from `/System/Fonts`. Text is then rasterized on
//! demand by our own TrueType engine ([`crate::ttf`]) with quarter-pixel
//! positioning and pair kerning, and cached. Until a face is installed (early
//! boot, the kernel panic screen) a few sizes pre-rasterized at build time
//! stand in.

use crate::ttf::{raster, TrueType};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    Regular,
    SemiBold,
    Mono,
}

impl Face {
    const ALL: [Face; 3] = [Face::Regular, Face::SemiBold, Face::Mono];

    fn index(self) -> usize {
        self as usize
    }

    /// Where the face lives in the system image.
    pub fn file(self) -> &'static str {
        match self {
            Face::Regular => "/System/Fonts/Inter-Regular.ttf",
            Face::SemiBold => "/System/Fonts/Inter-SemiBold.ttf",
            Face::Mono => "/System/Fonts/JetBrainsMono-Regular.ttf",
        }
    }
}

// ------------------------------------------------------ build-time atlases

pub struct GlyphData {
    pub ch: u32,
    pub w: u16,
    pub h: u16,
    pub xmin: i16,
    pub ymin: i16,
    /// Advance width in 1/64 pixel.
    pub adv64: u32,
    pub off: u32,
}

pub struct FaceData {
    pub face: Face,
    pub size: u16,
    pub ascent: i16,
    pub descent: i16,
    pub line_height: i16,
    pub glyphs: &'static [GlyphData],
    pub bitmap: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/fonts.rs"));

fn atlas(face: Face, size: u16) -> &'static FaceData {
    ATLASES.iter().filter(|f| f.face == face).min_by_key(|f| (f.size as i32 - size as i32).abs()).unwrap_or(&ATLASES[0])
}

impl FaceData {
    fn glyph(&self, c: char) -> &GlyphData {
        let cp = c as u32;
        if (0x20..0x7F).contains(&cp) {
            let g = &self.glyphs[(cp - 0x20) as usize];
            if g.ch == cp {
                return g;
            }
        }
        self.glyphs.iter().find(|g| g.ch == cp).unwrap_or(&self.glyphs[('?' as u32 - 0x20) as usize])
    }
}

// --------------------------------------------------------- runtime faces

struct Installed {
    tt: TrueType<'static>,
    /// Glyph ids of ASCII characters, the common case.
    ascii: [u16; 128],
}

static FACES: [spin::Once<Installed>; 3] = [spin::Once::new(), spin::Once::new(), spin::Once::new()];

/// Makes a TrueType face available. Returns false if the data isn't a usable font.
pub fn install(face: Face, data: &'static [u8]) -> bool {
    let Some(tt) = TrueType::parse(data) else { return false };
    let mut ascii = [0u16; 128];
    for (c, slot) in ascii.iter_mut().enumerate() {
        *slot = tt.glyph_index(c as u8 as char);
    }
    FACES[face.index()].call_once(|| Installed { tt, ascii });
    true
}

pub fn installed(face: Face) -> bool {
    FACES[face.index()].get().is_some()
}

impl Installed {
    fn glyph(&self, c: char) -> u16 {
        match c as u32 {
            cp @ 0..=127 => self.ascii[cp as usize],
            _ => self.tt.glyph_index(c),
        }
    }

    fn scale(&self, units: i32, px: u16) -> i32 {
        // Rounded 26.6 fixed point.
        let v = units as i64 * px as i64 * 64;
        let upem = self.tt.units_per_em as i64;
        ((v + v.signum() * upem / 2) / upem) as i32
    }
}

/// Finds the face (with fallbacks for missing characters) that has `c`.
fn resolve(face: Face, c: char) -> Option<(&'static Installed, Face, u16)> {
    let primary = FACES[face.index()].get()?;
    let g = primary.glyph(c);
    if g != 0 {
        return Some((primary, face, g));
    }
    for f in Face::ALL {
        if let Some(i) = FACES[f.index()].get() {
            let g = i.glyph(c);
            if g != 0 {
                return Some((i, f, g));
            }
        }
    }
    Some((primary, face, 0))
}

// ------------------------------------------------------------ glyph cache

struct Cached {
    mask: raster::Mask,
    used: u64,
}

struct Cache {
    map: BTreeMap<u64, Cached>,
    bytes: usize,
    clock: u64,
}

const CACHE_BUDGET: usize = 2 << 20;

static CACHE: spin::Mutex<Cache> = spin::Mutex::new(Cache { map: BTreeMap::new(), bytes: 0, clock: 0 });

fn key(face: Face, px: u16, glyph: u16, quarter: u8) -> u64 {
    (face.index() as u64) << 40 | (px as u64) << 24 | (glyph as u64) << 8 | quarter as u64
}

impl Cache {
    fn get(&mut self, inst: &Installed, face: Face, px: u16, glyph: u16, quarter: u8) -> &raster::Mask {
        self.clock += 1;
        let k = key(face, px, glyph, quarter);
        if !self.map.contains_key(&k) {
            if self.bytes > CACHE_BUDGET {
                self.evict();
            }
            let outline = inst.tt.outline(glyph);
            let mask = raster::render(&outline, inst.tt.units_per_em, px as u32, quarter as i32 * 16384);
            self.bytes += mask.data.len() + 48;
            self.map.insert(k, Cached { mask, used: self.clock });
        }
        let e = self.map.get_mut(&k).unwrap();
        e.used = self.clock;
        &e.mask
    }

    /// Drops the least recently used half of the cache.
    fn evict(&mut self) {
        let mut ages: Vec<u64> = self.map.values().map(|e| e.used).collect();
        ages.sort_unstable();
        let cutoff = ages[ages.len() / 2];
        self.map.retain(|_, e| e.used > cutoff);
        self.bytes = self.map.values().map(|e| e.mask.data.len() + 48).sum();
    }
}

// ------------------------------------------------------------------ Font

/// A face at a pixel size. Cheap to copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Font {
    pub face: Face,
    pub size: u16,
    /// Distance from the baseline to the top of tall glyphs (pixels, positive).
    pub ascent: i16,
    /// Distance from the baseline to the bottom of descenders (pixels, negative).
    pub descent: i16,
    pub line_height: i16,
}

/// `face` at `size` pixels per em.
pub fn get(face: Face, size: u16) -> Font {
    let size = size.clamp(4, 512);
    match FACES[face.index()].get() {
        Some(i) => {
            let asc = i.scale(i.tt.ascender as i32, size);
            let desc = i.scale(i.tt.descender as i32, size);
            let gap = i.scale(i.tt.line_gap as i32, size);
            Font {
                face,
                size,
                ascent: ((asc + 32) >> 6) as i16,
                descent: ((desc - 32) >> 6) as i16,
                line_height: ((asc - desc + gap + 32) >> 6) as i16,
            }
        }
        None => {
            let a = atlas(face, size);
            Font { face, size: a.size, ascent: a.ascent, descent: a.descent, line_height: a.line_height }
        }
    }
}

/// One positioned glyph handed to a drawing callback.
pub struct GlyphView<'a> {
    /// Top-left of the mask in pixels (y down), relative to the text origin
    /// (x = pen start, y = baseline).
    pub x: i32,
    pub y: i32,
    pub w: u16,
    pub h: u16,
    pub coverage: &'a [u8],
}

impl Font {
    /// Walks the glyphs of `s`, calling `f` for every visible one. Returns the
    /// advance in 1/64 pixel.
    pub fn layout(&self, s: &str, mut f: impl FnMut(GlyphView)) -> i32 {
        if FACES[self.face.index()].get().is_some() {
            // Never block (the kernel panic screen draws text): fall back if busy.
            if let Some(mut cache) = CACHE.try_lock() {
                let mut pen = 0i32;
                let mut prev: Option<(Face, u16)> = None;
                for c in s.chars() {
                    let Some((inst, face, g)) = resolve(self.face, c) else { break };
                    if let Some((pf, pg)) = prev {
                        if pf == face && self.face != Face::Mono {
                            pen += inst.scale(inst.tt.kerning(pg, g), self.size);
                        }
                    }
                    let quarter = ((pen & 63) >> 4) as u8;
                    let mask = cache.get(inst, face, self.size, g, quarter);
                    if mask.width > 0 {
                        f(GlyphView {
                            x: (pen >> 6) + mask.x_min as i32,
                            y: -(mask.y_min as i32) - mask.height as i32,
                            w: mask.width,
                            h: mask.height,
                            coverage: &mask.data,
                        });
                    }
                    pen += inst.scale(inst.tt.advance(g) as i32, self.size);
                    prev = Some((face, g));
                }
                return pen;
            }
        }
        let a = atlas(self.face, self.size);
        let mut pen = 0i32;
        for c in s.chars() {
            let g = a.glyph(c);
            let off = g.off as usize;
            f(GlyphView {
                x: ((pen + 32) >> 6) + g.xmin as i32,
                y: -(g.ymin as i32) - g.h as i32,
                w: g.w,
                h: g.h,
                coverage: &a.bitmap[off..off + g.w as usize * g.h as usize],
            });
            pen += g.adv64 as i32;
        }
        pen
    }

    /// Calls `f(byte_index, pen)` before each character of `s` and returns the
    /// final pen position (1/64 pixel), exactly as [`Font::layout`] advances.
    fn walk(&self, s: &str, mut f: impl FnMut(usize, i32)) -> i32 {
        let mut pen = 0;
        if FACES[self.face.index()].get().is_some() {
            let mut prev: Option<(Face, u16)> = None;
            for (idx, c) in s.char_indices() {
                let Some((i, face, g)) = resolve(self.face, c) else { break };
                if let Some((pf, pg)) = prev {
                    if pf == face && self.face != Face::Mono {
                        pen += i.scale(i.tt.kerning(pg, g), self.size);
                    }
                }
                f(idx, pen);
                pen += i.scale(i.tt.advance(g) as i32, self.size);
                prev = Some((face, g));
            }
        } else {
            let a = atlas(self.face, self.size);
            for (idx, c) in s.char_indices() {
                f(idx, pen);
                pen += a.glyph(c).adv64 as i32;
            }
        }
        pen
    }

    /// Advance of `s` in 1/64 pixel, including kerning.
    pub fn width64(&self, s: &str) -> i32 {
        self.walk(s, |_, _| {})
    }

    /// Width of `s` in pixels.
    pub fn width(&self, s: &str) -> i32 {
        (self.width64(s) + 32) >> 6
    }

    /// Advance of a single character (useful for monospace grids).
    pub fn advance(&self, c: char) -> i32 {
        let mut buf = [0u8; 4];
        self.width(c.encode_utf8(&mut buf))
    }

    /// Pixel x of the caret before each character of `s` (indexed like
    /// `s.char_indices()`), plus one final stop after the last character.
    pub fn caret_stops(&self, s: &str) -> Vec<i32> {
        let mut stops = Vec::with_capacity(s.len() + 1);
        let end = self.walk(s, |_, pen| stops.push((pen + 32) >> 6));
        stops.push((end + 32) >> 6);
        stops
    }
}
