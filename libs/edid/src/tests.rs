use super::*;
use alloc::vec;

/// Fixes up a block's checksum byte.
fn seal(block: &mut [u8]) {
    let sum = block[..127].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    block[127] = 0u8.wrapping_sub(sum);
}

/// An 18-byte detailed timing descriptor.
fn dtd(clock_khz: u32, h: (u32, u32, u32, u32), v: (u32, u32, u32, u32), flags: u8, mm: (u32, u32)) -> [u8; 18] {
    let (ha, hf, hs, hb) = h;
    let (va, vf, vs, vb) = v;
    let c = (clock_khz / 10) as u16;
    [
        c as u8,
        (c >> 8) as u8,
        ha as u8,
        hb as u8,
        ((ha >> 8) << 4 | (hb >> 8)) as u8,
        va as u8,
        vb as u8,
        ((va >> 8) << 4 | (vb >> 8)) as u8,
        hf as u8,
        hs as u8,
        ((vf & 0xF) << 4 | (vs & 0xF)) as u8,
        (((hf >> 8) & 3) << 6 | ((hs >> 8) & 3) << 4 | ((vf >> 4) & 3) << 2 | ((vs >> 4) & 3)) as u8,
        mm.0 as u8,
        mm.1 as u8,
        ((mm.0 >> 8) << 4 | (mm.1 >> 8)) as u8,
        0,
        0,
        flags,
    ]
}

fn text_descriptor(tag: u8, text: &str) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[3] = tag;
    let mut body = [0x20u8; 13];
    for (i, b) in text.bytes().take(13).enumerate() {
        body[i] = b;
    }
    if text.len() < 13 {
        body[text.len()] = 0x0A;
    }
    d[5..].copy_from_slice(&body);
    d
}

/// A 1.4 base block for "DEL" product 0x4321: 1920x1080@60 preferred
/// (CEA timing, 527x296 mm), a name, a range descriptor, two standard
/// timings and the 640x480/800x600/1024x768 established timings.
fn base_block(extensions: u8) -> [u8; 128] {
    let mut b = [0u8; 128];
    b[..8].copy_from_slice(&HEADER);
    // "DEL": D=4, E=5, L=12 → 0 00100 00101 01100
    let id: u16 = (4 << 10) | (5 << 5) | 12;
    b[8..10].copy_from_slice(&id.to_be_bytes());
    b[10..12].copy_from_slice(&0x4321u16.to_le_bytes());
    b[12..16].copy_from_slice(&0xA1B2C3D4u32.to_le_bytes());
    b[16] = 10;
    b[17] = 30; // 2020
    b[18] = 1;
    b[19] = 4;
    b[20] = 0xA5; // digital, 8 bits per colour, DisplayPort
    b[21] = 53;
    b[22] = 30;
    b[24] = 0x0A;
    b[35] = 0x21; // 640x480@60, 800x600@60
    b[36] = 0x08; // 1024x768@60
    b[37] = 0;
    // Standard timings: 1280x1024@60 (5:4), 1600x900@60 (16:9); rest unused.
    let std = [((1280 / 8 - 31) as u8, 0x80), ((1600 / 8 - 31) as u8, 0xC0)];
    for i in 0..8 {
        let (a, c) = std.get(i).copied().unwrap_or((1, 1));
        b[38 + 2 * i] = a;
        b[39 + 2 * i] = c;
    }
    b[54..72].copy_from_slice(&dtd(148_500, (1920, 88, 44, 280), (1080, 4, 5, 45), 0x1E, (527, 296)));
    b[72..90].copy_from_slice(&text_descriptor(0xFC, "DELL U2420"));
    let mut range = [0u8; 18];
    range[3] = 0xFD;
    range[5..10].copy_from_slice(&[48, 76, 30, 85, 17]);
    range[10] = 0x0A;
    b[90..108].copy_from_slice(&range);
    b[108..126].copy_from_slice(&text_descriptor(0xFF, "SN12345"));
    b[126] = extensions;
    seal(&mut b);
    b
}

/// A CTA-861 revision 3 block: a video data block (1080p60 native, 720p60,
/// 2160p30, VIC 200 unknown), an HDMI VSDB at 1.0.0.0, and one DTD
/// (2560x1440@60 CVT-RB-like).
fn cta_block_bytes() -> [u8; 128] {
    let mut b = [0u8; 128];
    b[0] = 0x02;
    b[1] = 3;
    b[3] = 0xF1; // underscan, audio, 4:4:4, 4:2:2, 1 native DTD
    let mut i = 4;
    let vdb = [0x40 | 4, 16 | 0x80, 4, 95, 200];
    b[i..i + 5].copy_from_slice(&vdb);
    i += 5;
    let vsdb = [0x60 | 5, 0x03, 0x0C, 0x00, 0x10, 0x00];
    b[i..i + 6].copy_from_slice(&vsdb);
    i += 6;
    b[2] = i as u8;
    b[i..i + 18].copy_from_slice(&dtd(241_500, (2560, 48, 32, 160), (1440, 3, 5, 41), 0x1A, (597, 336)));
    seal(&mut b);
    b
}

#[test]
fn base_block_identity_and_timings() {
    let e = parse(&base_block(0)).unwrap();
    assert_eq!(e.manufacturer_str(), "DEL");
    assert_eq!(e.product, 0x4321);
    assert_eq!(e.serial, 0xA1B2C3D4);
    assert_eq!(e.year, 2020);
    assert_eq!(e.version, (1, 4));
    assert!(e.digital);
    assert_eq!((e.width_cm, e.height_cm), (53, 30));
    assert_eq!((e.width_mm, e.height_mm), (527, 296));
    assert_eq!(e.name.as_deref(), Some("DELL U2420"));
    assert_eq!(e.serial_text.as_deref(), Some("SN12345"));
    let r = e.range.unwrap();
    assert_eq!((r.min_vfreq_hz, r.max_vfreq_hz, r.min_hfreq_khz, r.max_hfreq_khz), (48, 76, 30, 85));
    assert_eq!(r.max_clock_khz, 170_000);

    let p = e.preferred().unwrap();
    assert!(p.preferred);
    assert_eq!((p.hactive, p.vactive, p.htotal, p.vtotal), (1920, 1080, 2200, 1125));
    assert_eq!((p.hsync_start, p.hsync_end, p.vsync_start, p.vsync_end), (2008, 2052, 1084, 1089));
    assert_eq!(p.refresh_hz(), 60);
    assert!(p.hsync_positive && p.vsync_positive);
    assert_eq!(p.source, Source::Detailed);
    assert_eq!(e.timings[0], *p, "the preferred timing comes first");

    let sizes: Vec<(u32, u32, u32)> = e.timings.iter().map(|t| (t.hactive, t.vactive, t.refresh_hz())).collect();
    for want in [(640, 480, 60), (800, 600, 60), (1024, 768, 60), (1280, 1024, 60), (1600, 900, 60)] {
        assert!(sizes.contains(&want), "{want:?} missing from {sizes:?}");
    }
    let est = e.timings.iter().find(|t| t.hactive == 1024).unwrap();
    assert_eq!((est.clock_khz, est.htotal, est.vtotal, est.source), (65_000, 1344, 806, Source::Established));
    assert_eq!(e.max_size(), (1920, 1080));
}

#[test]
fn cta_extension() {
    let mut data = vec![];
    data.extend_from_slice(&base_block(1));
    data.extend_from_slice(&cta_block_bytes());
    let e = parse(&data).unwrap();
    let cta = e.cta.as_ref().unwrap();
    assert_eq!(cta.revision, 3);
    assert!(cta.underscan && cta.basic_audio && cta.ycbcr444 && cta.ycbcr422);
    assert_eq!(cta.vics, vec![16, 4, 95, 200], "native VIC first, unknown VICs kept");
    assert!(cta.hdmi);
    assert_eq!(cta.hdmi_address, 0x1000);
    let find = |w, h, hz| e.timings.iter().find(|t| (t.hactive, t.vactive, t.refresh_hz()) == (w, h, hz));
    let qhd = find(2560, 1440, 60).expect("CTA DTD");
    assert_eq!((qhd.clock_khz, qhd.htotal, qhd.vtotal), (241_500, 2720, 1481));
    assert!(!qhd.vsync_positive && qhd.hsync_positive);
    let uhd = find(3840, 2160, 30).expect("VIC 95");
    assert_eq!((uhd.clock_khz, uhd.htotal, uhd.vtotal, uhd.source), (297_000, 4400, 2250, Source::Cta));
    let hd = find(1280, 720, 60).expect("VIC 4");
    assert_eq!(hd.source, Source::Cta);
    // VIC 16 repeats the base block's preferred 1080p60: kept once.
    assert_eq!(e.timings.iter().filter(|t| (t.hactive, t.vactive, t.refresh_hz()) == (1920, 1080, 60)).count(), 1);
    assert_eq!(e.skipped_extensions, 0);
}

#[test]
fn bad_input() {
    assert_eq!(parse(&[0u8; 64]).unwrap_err(), Error::TooShort);
    let mut b = base_block(0);
    b[0] = 1;
    assert_eq!(parse(&b).unwrap_err(), Error::BadHeader);
    let mut b = base_block(0);
    b[100] ^= 0x55;
    assert_eq!(parse(&b).unwrap_err(), Error::BadChecksum);
    // A corrupt extension is skipped, the base block still parses.
    let mut data = vec![];
    data.extend_from_slice(&base_block(1));
    let mut cta = cta_block_bytes();
    cta[20] ^= 1;
    data.extend_from_slice(&cta);
    let e = parse(&data).unwrap();
    assert_eq!(e.skipped_extensions, 1);
    assert!(e.cta.is_none());
    // The count may promise more blocks than were read.
    let e = parse(&base_block(3)).unwrap();
    assert_eq!(e.preferred().unwrap().hactive, 1920);
}

#[test]
fn cvt_reduced_blanking() {
    // VESA's published CVT-RB 1920x1080@60: 138.5 MHz, 2080 x 1111.
    let t = cvt_rb(1920, 1080, 60).unwrap();
    assert_eq!((t.clock_khz, t.htotal, t.vtotal), (138_500, 2080, 1111));
    assert_eq!((t.hsync_start, t.hsync_end, t.vsync_start, t.vsync_end), (1968, 2000, 1083, 1088));
    // 2560x1600@60 (16:10, vsync 6): 268.5 MHz, 2720 x 1646.
    let t = cvt_rb(2560, 1600, 60).unwrap();
    assert_eq!((t.clock_khz, t.htotal, t.vtotal), (268_500, 2720, 1646));
    assert_eq!(t.vsync_end - t.vsync_start, 6);
    assert_eq!(t.refresh_hz(), 60);
    assert!(cvt_rb(0, 0, 60).is_none());
}

#[test]
fn interlaced_and_displayid() {
    // VIC 5, 1080i: 60 fields per second.
    let t = vic_timing(5).unwrap();
    assert!(t.interlaced);
    assert_eq!(t.refresh_hz(), 60);
    // A DisplayID extension with one type I timing: 5120x2880@60 preferred.
    let mut base = base_block(1);
    base[54..72].copy_from_slice(&dtd(148_500, (1920, 88, 44, 280), (1080, 4, 5, 45), 0x1E, (527, 296)));
    seal(&mut base);
    let mut did = [0u8; 128];
    did[0] = 0x70;
    did[1] = 0x12;
    did[2] = 3 + 20; // section: one block header + one timing
    did[5] = 0x03;
    did[6] = 0;
    did[7] = 20;
    let clock = 938_250 / 10 - 1;
    let t = &mut did[8..28];
    t[..3].copy_from_slice(&[clock as u8, (clock >> 8) as u8, (clock >> 16) as u8]);
    t[3] = 0x80 | 0x04; // preferred, 16:9
    let put = |t: &mut [u8], o: usize, v: u32| t[o..o + 2].copy_from_slice(&((v - 1) as u16).to_le_bytes());
    put(t, 4, 5120);
    put(t, 6, 160);
    put(t, 8, 48);
    t[9] |= 0x80; // +hsync
    put(t, 10, 32);
    put(t, 12, 2880);
    put(t, 14, 74);
    put(t, 16, 3);
    put(t, 18, 5);
    let sum = did[..127].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    did[127] = 0u8.wrapping_sub(sum);
    let mut data = vec![];
    data.extend_from_slice(&base);
    data.extend_from_slice(&did);
    let e = parse(&data).unwrap();
    let big = e.timings.iter().find(|t| t.hactive == 5120).expect("DisplayID timing");
    assert_eq!((big.vactive, big.htotal, big.vtotal, big.clock_khz), (2880, 5280, 2954, 938_250));
    assert!(big.hsync_positive && !big.vsync_positive);
    assert_eq!(big.refresh_hz(), 60);
    assert_eq!(e.max_size(), (5120, 2880));
}
