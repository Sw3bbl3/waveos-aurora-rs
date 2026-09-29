//! Pre-rasterized fonts (generated at build time by `kernel/build.rs`).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    Regular,
    SemiBold,
    Mono,
}

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
    #[allow(dead_code)]
    pub line_height: i16,
    pub glyphs: &'static [GlyphData],
    pub bitmap: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/fonts.rs"));

pub type Font = &'static FaceData;

/// The closest available size of `face`.
pub fn get(face: Face, size: u16) -> Font {
    FACES
        .iter()
        .filter(|f| f.face == face)
        .min_by_key(|f| (f.size as i32 - size as i32).abs())
        .expect("font face missing")
}

impl FaceData {
    pub fn glyph(&self, c: char) -> &GlyphData {
        let cp = c as u32;
        if (0x20..0x7F).contains(&cp) {
            let g = &self.glyphs[(cp - 0x20) as usize];
            if g.ch == cp {
                return g;
            }
        }
        self.glyphs.iter().find(|g| g.ch == cp).unwrap_or(&self.glyphs[('?' as u32 - 0x20) as usize])
    }

    pub fn bitmap_of(&self, g: &GlyphData) -> &[u8] {
        &self.bitmap[g.off as usize..g.off as usize + g.w as usize * g.h as usize]
    }

    /// Width of `s` in pixels.
    pub fn width(&self, s: &str) -> i32 {
        let adv: u32 = s.chars().map(|c| self.glyph(c).adv64).sum();
        (adv as i32 + 32) >> 6
    }

    /// Advance of a single character (useful for monospace grids).
    pub fn advance(&self, c: char) -> i32 {
        (self.glyph(c).adv64 as i32 + 32) >> 6
    }
}
