//! Aurora image codecs: PNG (decode and encode) and BMP (decode).
//!
//! Images are 32-bit `0xAARRGGBB` pixels with straight (non-premultiplied)
//! alpha — the format the Aurora canvas draws. `no_std` + `alloc`, integer
//! only, so the window server can use it as well as apps. Deflate comes from
//! `miniz_oxide`; everything else (chunk parsing, filters, Adam7, colour
//! conversion, the encoder's filter choice) is ours.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bmp;
pub mod gif;
pub mod jpeg;
pub mod png;

use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Row-major `0xAARRGGBB`.
    pub pixels: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a format we recognise.
    Unsupported,
    /// Structurally broken data (bad chunk, CRC, header, …).
    Corrupt,
    /// Larger than [`MAX_PIXELS`].
    TooLarge,
}

/// Refuse images above 64 megapixels (256 MiB decoded).
pub const MAX_PIXELS: u64 = 64 << 20;

impl Image {
    pub fn new(width: u32, height: u32, fill: u32) -> Image {
        Image { width, height, pixels: alloc::vec![fill; width as usize * height as usize] }
    }

    pub fn is_opaque(&self) -> bool {
        self.pixels.iter().all(|p| p >> 24 == 0xFF)
    }

    /// A copy scaled to fit within `max_w` × `max_h` (never enlarged), averaging
    /// the source pixels each destination pixel covers.
    pub fn thumbnail(&self, max_w: u32, max_h: u32) -> Image {
        if self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let (w, h) = fit(self.width, self.height, max_w, max_h);
        if (w, h) == (self.width, self.height) {
            return self.clone();
        }
        let mut out = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h {
            let (sy0, sy1) = (y * self.height / h, ((y + 1) * self.height / h).max(y * self.height / h + 1));
            for x in 0..w {
                let (sx0, sx1) = (x * self.width / w, ((x + 1) * self.width / w).max(x * self.width / w + 1));
                let (mut a, mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let p = self.pixels[(sy * self.width + sx) as usize] as u64;
                        let pa = p >> 24;
                        // Weight colour by alpha so transparent pixels don't darken edges.
                        a += pa;
                        r += (p >> 16 & 0xFF) * pa;
                        g += (p >> 8 & 0xFF) * pa;
                        b += (p & 0xFF) * pa;
                        n += 1;
                    }
                }
                let px = if a == 0 { 0 } else { ((a / n) << 24 | (r / a) << 16 | (g / a) << 8 | (b / a)) as u32 };
                out.push(px);
            }
        }
        Image { width: w, height: h, pixels: out }
    }
}

/// The largest size with the aspect ratio of `w`×`h` inside `max_w`×`max_h` (never enlarged).
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    if w as u64 * max_h as u64 > h as u64 * max_w as u64 {
        (max_w, ((h as u64 * max_w as u64 / w as u64) as u32).max(1))
    } else {
        (((w as u64 * max_h as u64 / h as u64) as u32).max(1), max_h)
    }
}

/// Decodes a PNG, JPEG, GIF or BMP, recognised by its signature.
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.starts_with(&png::SIGNATURE) {
        png::decode(data)
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg::decode(data)
    } else if data.starts_with(b"GIF8") {
        gif::decode(data)
    } else if data.starts_with(b"BM") {
        bmp::decode(data)
    } else {
        Err(Error::Unsupported)
    }
}

/// True for file names we can open.
pub fn is_image_name(name: &str) -> bool {
    let lower = |ext: &str| name.len() > ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext);
    [".png", ".bmp", ".jpg", ".jpeg", ".gif"].iter().any(|e| lower(e))
}

#[cfg(test)]
mod tests;
