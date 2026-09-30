//! Cross-checks our PNG decoder and encoder against the `png` crate.

use crate::{decode, png, Image};

fn encode_with_png_crate(
    w: u32,
    h: u32,
    color: ::png::ColorType,
    depth: ::png::BitDepth,
    data: &[u8],
    palette: Option<(&[u8], &[u8])>,
    trns: Option<&[u8]>,
) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = ::png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(depth);
        if let Some((p, t)) = palette {
            e.set_palette(p.to_vec());
            if !t.is_empty() {
                e.set_trns(t.to_vec());
            }
        }
        if let Some(t) = trns {
            e.set_trns(t.to_vec());
        }
        e.write_header().unwrap().write_image_data(data).unwrap();
    }
    out
}

/// Reference decode through the `png` crate, expanded to 8-bit RGBA.
fn reference(bytes: &[u8]) -> Image {
    let mut d = ::png::Decoder::new(bytes);
    d.set_transformations(
        ::png::Transformations::EXPAND | ::png::Transformations::STRIP_16 | ::png::Transformations::ALPHA,
    );
    let mut r = d.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let (w, h) = (info.width, info.height);
    let mut px = Vec::new();
    match info.color_type {
        ::png::ColorType::Rgba => {
            for c in buf.chunks_exact(4).take((w * h) as usize) {
                px.push((c[3] as u32) << 24 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32);
            }
        }
        ::png::ColorType::GrayscaleAlpha => {
            for c in buf.chunks_exact(2).take((w * h) as usize) {
                px.push((c[1] as u32) << 24 | (c[0] as u32) * 0x010101);
            }
        }
        other => panic!("unexpected reference colour type {other:?}"),
    }
    Image { width: w, height: h, pixels: px }
}

fn pattern(n: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.wrapping_mul(2654435761).max(1);
    (0..n)
        .map(|i| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            // Mix smooth gradients (exercise filters) with noise.
            if i % 7 < 4 {
                (i * 3 + seed as usize) as u8
            } else {
                s as u8
            }
        })
        .collect()
}

#[test]
fn decodes_every_colour_type_and_depth() {
    use ::png::{BitDepth::*, ColorType::*};
    let (w, h) = (37u32, 23u32);
    let cases = [
        (Grayscale, One, 1),
        (Grayscale, Two, 2),
        (Grayscale, Four, 4),
        (Grayscale, Eight, 8),
        (Grayscale, Sixteen, 16),
        (Rgb, Eight, 24),
        (Rgb, Sixteen, 48),
        (GrayscaleAlpha, Eight, 16),
        (GrayscaleAlpha, Sixteen, 32),
        (Rgba, Eight, 32),
        (Rgba, Sixteen, 64),
    ];
    for (i, (color, depth, bpp)) in cases.into_iter().enumerate() {
        let row = (w as usize * bpp).div_ceil(8);
        let data = pattern(row * h as usize, i as u32 + 1);
        let bytes = encode_with_png_crate(w, h, color, depth, &data, None, None);
        assert_eq!(decode(&bytes).unwrap(), reference(&bytes), "{color:?} {depth:?}");
    }
}

#[test]
fn palettes_and_transparency() {
    use ::png::{BitDepth::*, ColorType::*};
    let (w, h) = (19u32, 11u32);
    let palette: Vec<u8> = (0..16 * 3).map(|i| (i * 17) as u8).collect();
    let alpha: Vec<u8> = (0..10).map(|i| (i * 25) as u8).collect();
    for (depth, bits) in [(One, 1), (Two, 2), (Four, 4), (Eight, 8)] {
        let n = 1usize << bits.min(4);
        let row = (w as usize * bits).div_ceil(8);
        let data: Vec<u8> =
            pattern(row * h as usize, bits as u32).iter().map(|&b| if bits == 8 { b % n as u8 } else { b }).collect();
        let pal = &palette[..3 * n];
        let bytes = encode_with_png_crate(w, h, Indexed, depth, &data, Some((pal, &alpha[..n.min(10)])), None);
        assert_eq!(decode(&bytes).unwrap(), reference(&bytes), "palette {bits}-bit");
    }
    // Colour-key transparency.
    let data = pattern(w as usize * h as usize * 3, 9);
    let key = [0u8, data[0], 0, data[1], 0, data[2]];
    let bytes = encode_with_png_crate(w, h, Rgb, Eight, &data, None, Some(&key));
    let img = decode(&bytes).unwrap();
    assert_eq!(img, reference(&bytes));
    assert_eq!(img.pixels[0] >> 24, 0, "keyed pixel is transparent");
}

/// Builds an Adam7-interlaced 8-bit RGBA PNG by hand (the png crate only writes progressive).
fn interlaced_rgba(w: u32, h: u32, pixels: &[u32]) -> Vec<u8> {
    const ADAM7: [(u32, u32, u32, u32); 7] =
        [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];
    let mut raw = Vec::new();
    for (x0, y0, dx, dy) in ADAM7 {
        let pw = (w + dx - x0 - 1) / dx;
        let ph = (h + dy - y0 - 1) / dy;
        if pw == 0 {
            continue;
        }
        for r in 0..ph {
            raw.push(0); // filter None
            for c in 0..pw {
                let p = pixels[((y0 + r * dy) * w + x0 + c * dx) as usize];
                raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
            }
        }
    }
    let mut out = png::SIGNATURE.to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&crc32(kind, body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 1]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
    chunk(b"IEND", &[]);
    out
}

fn crc32(kind: &[u8], body: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in kind.iter().chain(body) {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[test]
fn adam7_interlacing() {
    for (w, h) in [(1, 1), (3, 5), (8, 8), (33, 17)] {
        let pixels: Vec<u32> = pattern((w * h * 4) as usize, w + h)
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let bytes = interlaced_rgba(w, h, &pixels);
        let img = decode(&bytes).unwrap();
        assert_eq!(img.pixels, pixels, "{w}x{h}");
        assert_eq!(img, reference(&bytes));
    }
}

#[test]
fn encoder_round_trips() {
    let (w, h) = (64u32, 40u32);
    let mut img = Image::new(w, h, 0);
    for y in 0..h {
        for x in 0..w {
            img.pixels[(y * w + x) as usize] = 0xFF00_0000 | (x * 4) << 16 | (y * 6) << 8 | ((x ^ y) * 3);
        }
    }
    let opaque = png::encode(&img);
    assert_eq!(reference(&opaque), img);
    assert_eq!(decode(&opaque).unwrap(), img);
    img.pixels[5] = 0x8012_3456;
    let alpha = png::encode(&img);
    assert_eq!(reference(&alpha), img);
    // Smooth content compresses well with the filter heuristic.
    assert!(opaque.len() < (w * h * 3) as usize / 4, "{} bytes", opaque.len());
}

#[test]
fn rejects_corruption() {
    let img = Image::new(8, 8, 0xFF11_2233);
    let mut bytes = png::encode(&img);
    let n = bytes.len();
    bytes[n - 20] ^= 0x55; // inside IDAT → CRC mismatch
    assert_eq!(decode(&bytes), Err(crate::Error::Corrupt));
    assert_eq!(decode(b"GIF89a"), Err(crate::Error::Corrupt)); // truncated
    assert_eq!(decode(b"RIFF....WEBP"), Err(crate::Error::Unsupported));
    assert!(decode(&png::SIGNATURE).is_err());
}

#[test]
fn bmp_24_and_32_bit() {
    // 3x2, 24-bit, bottom-up, rows padded to 4 bytes.
    let (w, h) = (3u32, 2u32);
    let px = [0xFF102030u32, 0xFF405060, 0xFF708090, 0xFFA0B0C0, 0xFFD0E0F0, 0xFF010203];
    let stride = 12;
    let mut d = vec![0u8; 54 + stride * 2];
    d[0..2].copy_from_slice(b"BM");
    d[10..14].copy_from_slice(&54u32.to_le_bytes());
    d[14..18].copy_from_slice(&40u32.to_le_bytes());
    d[18..22].copy_from_slice(&w.to_le_bytes());
    d[22..26].copy_from_slice(&h.to_le_bytes());
    d[28..30].copy_from_slice(&24u16.to_le_bytes());
    for y in 0..2 {
        for x in 0..3 {
            let p = px[y * 3 + x];
            let row = 1 - y; // bottom-up
            let o = 54 + row * stride + 3 * x;
            d[o..o + 3].copy_from_slice(&[p as u8, (p >> 8) as u8, (p >> 16) as u8]);
        }
    }
    assert_eq!(decode(&d).unwrap().pixels, px);
}

#[test]
fn thumbnails_keep_aspect() {
    let img = Image::new(400, 100, 0xFF336699);
    let t = img.thumbnail(128, 128);
    assert_eq!((t.width, t.height), (128, 32));
    assert!(t.pixels.iter().all(|&p| p == 0xFF336699));
}

/// A smooth test photo: gradients plus some texture.
fn photo(w: u32, h: u32) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = ((x * 7 + y * 13) % 32) as u32;
            rgb.push((x * 255 / w) as u8);
            rgb.push((y * 255 / h) as u8);
            rgb.push((128 + t * 2) as u8);
        }
    }
    rgb
}

fn jpeg_with(
    w: u16,
    h: u16,
    rgb: &[u8],
    progressive: bool,
    sampling: jpeg_encoder::SamplingFactor,
    restart: Option<u16>,
    gray: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut out, 90);
    enc.set_progressive(progressive);
    enc.set_sampling_factor(sampling);
    if let Some(r) = restart {
        enc.set_restart_interval(r);
    }
    if gray {
        let luma: Vec<u8> =
            rgb.chunks(3).map(|p| ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8).collect();
        enc.encode(&luma, w, h, jpeg_encoder::ColorType::Luma).unwrap();
    } else {
        enc.encode(rgb, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
    }
    out
}

/// Our decode against jpeg-decoder's: every channel within `tol`.
fn check_jpeg(bytes: &[u8], tol: i32) {
    let ours = decode(bytes).unwrap();
    let mut dec = jpeg_decoder::Decoder::new(bytes);
    let theirs = dec.decode().unwrap();
    let info = dec.info().unwrap();
    assert_eq!((ours.width, ours.height), (info.width as u32, info.height as u32));
    let channels = theirs.len() / (info.width as usize * info.height as usize);
    let mut worst = 0;
    for (i, p) in ours.pixels.iter().enumerate() {
        let t = &theirs[i * channels..];
        let (r, g, b) = if channels == 1 { (t[0], t[0], t[0]) } else { (t[0], t[1], t[2]) };
        for (a, b) in [((p >> 16) & 0xFF, r), ((p >> 8) & 0xFF, g), (p & 0xFF, b)] {
            worst = worst.max((a as i32 - b as i32).abs());
        }
    }
    assert!(worst <= tol, "channels differ by up to {worst}");
}

#[test]
fn jpeg_baseline_progressive_and_subsampling() {
    use jpeg_encoder::SamplingFactor as S;
    let (w, h) = (61u16, 37u16); // not multiples of the MCU size
    let rgb = photo(w as u32, h as u32);
    for progressive in [false, true] {
        for sampling in [S::F_1_1, S::F_2_1, S::F_2_2, S::F_1_2] {
            // Chroma upsampling is bilinear here, "fancy" (triangular) there.
            let tol = if sampling == S::F_1_1 { 3 } else { 12 };
            check_jpeg(&jpeg_with(w, h, &rgb, progressive, sampling, None, false), tol);
        }
        check_jpeg(&jpeg_with(w, h, &rgb, progressive, S::F_2_2, Some(3), false), 12);
        check_jpeg(&jpeg_with(w, h, &rgb, progressive, S::F_1_1, None, true), 3);
    }
}

#[test]
fn gif_frames_with_transparency_and_interlace() {
    let (w, h) = (23u16, 17u16);
    let palette: Vec<u8> = (0..16u8).flat_map(|i| [i * 16, 255 - i * 16, i * 7]).collect();
    let indices: Vec<u8> = (0..(w as usize * h as usize)).map(|i| ((i * 7 + i / 23) % 16) as u8).collect();
    for interlaced in [false, true] {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, w, h, &palette).unwrap();
            // The encoder writes rows as given: put them in interlaced order ourselves.
            let data = if interlaced {
                let order = (0..h as usize)
                    .step_by(8)
                    .chain((4..h as usize).step_by(8))
                    .chain((2..h as usize).step_by(4))
                    .chain((1..h as usize).step_by(2));
                order.flat_map(|r| indices[r * w as usize..(r + 1) * w as usize].to_vec()).collect()
            } else {
                indices.clone()
            };
            let mut frame = gif::Frame::from_indexed_pixels(w, h, data, Some(3));
            frame.interlaced = interlaced;
            enc.write_frame(&frame).unwrap();
        }
        let img = decode(&out).unwrap();
        assert_eq!((img.width, img.height), (w as u32, h as u32));
        for (i, &idx) in indices.iter().enumerate() {
            let want = if idx == 3 {
                0
            } else {
                let c = &palette[idx as usize * 3..];
                0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32
            };
            assert_eq!(img.pixels[i], want, "pixel {i} (interlaced {interlaced})");
        }
    }
}
