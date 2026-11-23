//! EDID: what a display says about itself.
//!
//! Parses the 128-byte EDID 1.3/1.4 base block (identity, size, the
//! established, standard and detailed timings, the monitor name and range
//! limits) and the extension blocks a display driver meets in practice:
//! CTA-861 (HDMI and most TVs: detailed timings and short video
//! descriptors) and DisplayID (type I detailed timings, used for large
//! screens). Everything is integer arithmetic, so the soft-float kernel
//! can use it, and it is tested on the host (`cargo test -p aurora-edid`).
//!
//! Timings come out as [`Timing`]: full CRTC timings, ready for a
//! modesetting driver. Standard timings carry only a size and refresh
//! rate; they are completed with VESA CVT reduced blanking ([`cvt_rb`]).
//!
//! Written from the VESA E-EDID 1.4, CTA-861 and DisplayID 1.3 documents.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Where a timing came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// An 18-byte detailed timing descriptor (base block or CTA-861).
    Detailed,
    /// The established timings bitmap (bytes 35–37).
    Established,
    /// A standard timing (2 bytes), completed with CVT reduced blanking.
    Standard,
    /// A CTA-861 short video descriptor (a VIC from the CTA table).
    Cta,
    /// A DisplayID type I detailed timing.
    DisplayId,
}

/// Full CRTC timings of one video mode, in pixels and lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub clock_khz: u32,
    pub hactive: u32,
    pub hsync_start: u32,
    pub hsync_end: u32,
    pub htotal: u32,
    pub vactive: u32,
    pub vsync_start: u32,
    pub vsync_end: u32,
    pub vtotal: u32,
    pub interlaced: bool,
    pub hsync_positive: bool,
    pub vsync_positive: bool,
    /// The display's preferred mode (the first detailed timing).
    pub preferred: bool,
    pub source: Source,
}

impl Timing {
    /// Vertical refresh in millihertz (frames per second for interlaced modes).
    pub fn refresh_mhz(&self) -> u32 {
        let total = self.htotal as u64 * self.vtotal as u64;
        if total == 0 {
            return 0;
        }
        let mhz = self.clock_khz as u64 * 1_000_000 / total;
        (if self.interlaced { mhz * 2 } else { mhz }) as u32
    }

    /// Refresh rounded to whole hertz.
    pub fn refresh_hz(&self) -> u32 {
        (self.refresh_mhz() + 500) / 1000
    }
}

/// Range limits from the monitor range descriptor (tag 0xFD).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RangeLimits {
    pub min_vfreq_hz: u32,
    pub max_vfreq_hz: u32,
    pub min_hfreq_khz: u32,
    pub max_hfreq_khz: u32,
    /// 0 when not given.
    pub max_clock_khz: u32,
}

/// What a CTA-861 extension adds besides timings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cta {
    pub revision: u8,
    pub underscan: bool,
    pub basic_audio: bool,
    pub ycbcr444: bool,
    pub ycbcr422: bool,
    /// Every VIC listed, the native ones first (including VICs with no
    /// entry in our table).
    pub vics: Vec<u8>,
    /// An HDMI vendor-specific data block (OUI 00-0C-03) is present: the
    /// sink is HDMI rather than DVI.
    pub hdmi: bool,
    /// The HDMI physical address (CEC), e.g. 0x1000 for 1.0.0.0.
    pub hdmi_address: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    TooShort,
    BadHeader,
    /// The base block's checksum is wrong.
    BadChecksum,
}

/// A parsed EDID.
#[derive(Clone, Debug, Default)]
pub struct Edid {
    /// Three-letter PNP manufacturer id, e.g. "DEL".
    pub manufacturer: [u8; 3],
    pub product: u16,
    pub serial: u32,
    pub year: u16,
    pub version: (u8, u8),
    pub digital: bool,
    /// Physical size in centimetres (0 when unknown, e.g. a projector).
    pub width_cm: u8,
    pub height_cm: u8,
    /// Physical size of the preferred mode's picture, in millimetres.
    pub width_mm: u32,
    pub height_mm: u32,
    pub name: Option<String>,
    pub serial_text: Option<String>,
    pub range: Option<RangeLimits>,
    /// Every timing found, the preferred one first, duplicates removed.
    pub timings: Vec<Timing>,
    pub cta: Option<Cta>,
    /// Extension blocks skipped for a bad checksum or an unknown tag.
    pub skipped_extensions: u8,
}

impl Edid {
    pub fn preferred(&self) -> Option<&Timing> {
        self.timings.iter().find(|t| t.preferred).or(self.timings.first())
    }

    /// The largest active area among the timings.
    pub fn max_size(&self) -> (u32, u32) {
        self.timings.iter().map(|t| (t.hactive, t.vactive)).max_by_key(|&(w, h)| w as u64 * h as u64).unwrap_or((0, 0))
    }

    pub fn manufacturer_str(&self) -> &str {
        core::str::from_utf8(&self.manufacturer).unwrap_or("???")
    }
}

const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

fn checksum_ok(block: &[u8]) -> bool {
    block.iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0
}

/// Parses an EDID: the base block and as many extension blocks as follow
/// it in `data` (the base block's count is the upper bound).
pub fn parse(data: &[u8]) -> Result<Edid, Error> {
    if data.len() < 128 {
        return Err(Error::TooShort);
    }
    let base = &data[..128];
    if base[..8] != HEADER {
        return Err(Error::BadHeader);
    }
    if !checksum_ok(base) {
        return Err(Error::BadChecksum);
    }
    let mut e = Edid::default();
    let id = u16::from_be_bytes([base[8], base[9]]);
    e.manufacturer = [b'@' + ((id >> 10) & 0x1F) as u8, b'@' + ((id >> 5) & 0x1F) as u8, b'@' + (id & 0x1F) as u8];
    e.product = u16::from_le_bytes([base[10], base[11]]);
    e.serial = u32::from_le_bytes([base[12], base[13], base[14], base[15]]);
    e.year = 1990 + base[17] as u16;
    e.version = (base[18], base[19]);
    e.digital = base[20] & 0x80 != 0;
    e.width_cm = base[21];
    e.height_cm = base[22];

    let mut timings: Vec<Timing> = Vec::new();
    // Detailed descriptors first: in EDID 1.4 the first is always the
    // preferred mode, and in 1.3 when feature bit 1 says so (it nearly always does).
    let first_preferred = e.version >= (1, 4) || base[24] & 0x02 != 0;
    for (i, d) in base[54..126].as_chunks::<18>().0.iter().enumerate() {
        if d[0] != 0 || d[1] != 0 {
            if let Some(mut t) = detailed(d) {
                if i == 0 && first_preferred {
                    t.preferred = true;
                    e.width_mm = (d[12] as u32) | ((d[14] as u32 >> 4) << 8);
                    e.height_mm = (d[13] as u32) | ((d[14] as u32 & 0xF) << 8);
                }
                timings.push(t);
            }
            continue;
        }
        match d[3] {
            0xFC => e.name = Some(descriptor_text(&d[5..18])),
            0xFF => e.serial_text = Some(descriptor_text(&d[5..18])),
            0xFD => e.range = Some(range_limits(d)),
            // More standard timings.
            0xFA => {
                for st in d[5..17].as_chunks::<2>().0 {
                    timings.extend(standard(st[0], st[1], e.version));
                }
            }
            _ => {}
        }
    }
    timings.extend(established(base[35], base[36], base[37]));
    for st in base[38..54].as_chunks::<2>().0 {
        timings.extend(standard(st[0], st[1], e.version));
    }

    let count = base[126] as usize;
    for n in 1..=count {
        let Some(block) = data.get(n * 128..(n + 1) * 128) else { break };
        if !checksum_ok(block) {
            e.skipped_extensions += 1;
            continue;
        }
        match block[0] {
            0x02 => {
                let (cta, mut t) = cta_block(block);
                timings.append(&mut t);
                e.cta = Some(cta);
            }
            0x70 => timings.append(&mut displayid_block(block)),
            _ => e.skipped_extensions += 1,
        }
    }

    // Preferred first; drop repeats of the same mode (size, refresh, scan).
    timings.sort_by_key(|t| !t.preferred);
    let mut out: Vec<Timing> = Vec::new();
    for t in timings {
        let same = |o: &Timing| {
            (o.hactive, o.vactive, o.interlaced) == (t.hactive, t.vactive, t.interlaced)
                && o.refresh_hz() == t.refresh_hz()
        };
        if !out.iter().any(same) {
            out.push(t);
        }
    }
    e.timings = out;
    Ok(e)
}

fn descriptor_text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0x0A).unwrap_or(b.len());
    b[..end]
        .iter()
        .map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' })
        .collect::<String>()
        .trim_end()
        .into()
}

fn range_limits(d: &[u8]) -> RangeLimits {
    // Byte 4 carries +255 offsets for the 1.4 extended ranges.
    let o = d[4];
    let add = |bit: u8| if o & bit != 0 { 255 } else { 0 };
    RangeLimits {
        min_vfreq_hz: d[5] as u32 + if o & 0x03 == 0x03 { 255 } else { 0 },
        max_vfreq_hz: d[6] as u32 + add(0x02),
        min_hfreq_khz: d[7] as u32 + if o & 0x0C == 0x0C { 255 } else { 0 },
        max_hfreq_khz: d[8] as u32 + add(0x08),
        max_clock_khz: if d[9] == 0 { 0 } else { d[9] as u32 * 10_000 },
    }
}

/// An 18-byte detailed timing descriptor (also the CTA-861 format).
pub fn detailed(d: &[u8]) -> Option<Timing> {
    let clock = u16::from_le_bytes([d[0], d[1]]) as u32 * 10;
    let hactive = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
    let hblank = d[3] as u32 | ((d[4] as u32 & 0xF) << 8);
    let vactive = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
    let vblank = d[6] as u32 | ((d[7] as u32 & 0xF) << 8);
    let hfront = d[8] as u32 | (((d[11] as u32 >> 6) & 3) << 8);
    let hsync = d[9] as u32 | (((d[11] as u32 >> 4) & 3) << 8);
    let vfront = (d[10] as u32 >> 4) | (((d[11] as u32 >> 2) & 3) << 4);
    let vsync = (d[10] as u32 & 0xF) | ((d[11] as u32 & 3) << 4);
    if clock == 0 || hactive == 0 || vactive == 0 || hfront + hsync > hblank || vfront + vsync > vblank {
        return None;
    }
    let flags = d[17];
    let interlaced = flags & 0x80 != 0;
    // Digital separate sync (bits 4–3 = 11) gives both polarities; the
    // other kinds have no vertical polarity bit (taken as negative).
    let separate = flags & 0x18 == 0x18;
    let (vactive, vfront, vsync, vblank) =
        if interlaced { (vactive * 2, vfront, vsync, vblank * 2 + 1) } else { (vactive, vfront, vsync, vblank) };
    Some(Timing {
        clock_khz: clock,
        hactive,
        hsync_start: hactive + hfront,
        hsync_end: hactive + hfront + hsync,
        htotal: hactive + hblank,
        vactive,
        vsync_start: vactive + vfront,
        vsync_end: vactive + vfront + vsync,
        vtotal: vactive + vblank,
        interlaced,
        hsync_positive: flags & 0x02 != 0,
        vsync_positive: separate && flags & 0x04 != 0,
        preferred: false,
        source: Source::Detailed,
    })
}

/// VESA DMT timings of the established-timings bitmap, as
/// (width, height, refresh, clock kHz, htotal, hfront, hsync, vtotal, vfront, vsync, +h, +v).
type Dmt = (u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, bool, bool);

const ESTABLISHED: [(u8, u8, Dmt); 17] = [
    (0, 0x80, (720, 400, 70, 28_322, 900, 18, 108, 449, 12, 2, false, true)),
    (0, 0x40, (720, 400, 88, 35_500, 900, 18, 108, 449, 12, 2, false, true)),
    (0, 0x20, (640, 480, 60, 25_175, 800, 16, 96, 525, 10, 2, false, false)),
    (0, 0x10, (640, 480, 67, 30_240, 864, 64, 64, 525, 3, 3, false, false)),
    (0, 0x08, (640, 480, 72, 31_500, 832, 24, 40, 520, 9, 3, false, false)),
    (0, 0x04, (640, 480, 75, 31_500, 840, 16, 64, 500, 1, 3, false, false)),
    (0, 0x02, (800, 600, 56, 36_000, 1024, 24, 72, 625, 1, 2, true, true)),
    (0, 0x01, (800, 600, 60, 40_000, 1056, 40, 128, 628, 1, 4, true, true)),
    (1, 0x80, (800, 600, 72, 50_000, 1040, 56, 120, 666, 37, 6, true, true)),
    (1, 0x40, (800, 600, 75, 49_500, 1056, 16, 80, 625, 1, 3, true, true)),
    (1, 0x20, (832, 624, 75, 57_284, 1152, 32, 64, 667, 1, 3, false, false)),
    (1, 0x10, (1024, 768, 87, 44_900, 1264, 8, 176, 817, 0, 4, true, true)),
    (1, 0x08, (1024, 768, 60, 65_000, 1344, 24, 136, 806, 3, 6, false, false)),
    (1, 0x04, (1024, 768, 70, 75_000, 1328, 24, 136, 806, 3, 6, false, false)),
    (1, 0x02, (1024, 768, 75, 78_750, 1312, 16, 96, 800, 1, 3, true, true)),
    (1, 0x01, (1280, 1024, 75, 135_000, 1688, 16, 144, 1066, 1, 3, true, true)),
    (2, 0x80, (1152, 870, 75, 100_000, 1456, 32, 128, 915, 3, 3, false, false)),
];

fn from_dmt(t: Dmt, source: Source) -> Timing {
    let (w, h, _, clock, htotal, hfront, hsync, vtotal, vfront, vsync, hp, vp) = t;
    Timing {
        clock_khz: clock,
        hactive: w,
        hsync_start: w + hfront,
        hsync_end: w + hfront + hsync,
        htotal,
        vactive: h,
        vsync_start: h + vfront,
        vsync_end: h + vfront + vsync,
        vtotal,
        interlaced: false,
        hsync_positive: hp,
        vsync_positive: vp,
        preferred: false,
        source,
    }
}

fn established(a: u8, b: u8, c: u8) -> Vec<Timing> {
    let bytes = [a, b, c];
    ESTABLISHED
        .iter()
        // 1024x768 at 87 Hz is interlaced; nothing wants it today.
        .filter(|(byte, bit, t)| bytes[*byte as usize] & bit != 0 && t.2 != 87)
        .map(|&(_, _, t)| from_dmt(t, Source::Established))
        .collect()
}

/// A 2-byte standard timing.
fn standard(b0: u8, b1: u8, version: (u8, u8)) -> Option<Timing> {
    if (b0 == 0x01 && b1 == 0x01) || b0 == 0 {
        return None;
    }
    let w = (b0 as u32 + 31) * 8;
    let h = match b1 >> 6 {
        // EDID before 1.3 meant 1:1 here.
        0 if version < (1, 3) => w,
        0 => w * 10 / 16,
        1 => w * 3 / 4,
        2 => w * 4 / 5,
        _ => w * 9 / 16,
    };
    let refresh = (b1 & 0x3F) as u32 + 60;
    let mut t = cvt_rb(w, h, refresh)?;
    t.source = Source::Standard;
    Some(t)
}

/// VESA CVT reduced-blanking (version 1) timings for `w`×`h` at `refresh`
/// Hz: what flat panels accept for any size. `None` for impossible inputs.
pub fn cvt_rb(w: u32, h: u32, refresh: u32) -> Option<Timing> {
    if w < 8 || h < 8 || !(20..=480).contains(&refresh) {
        return None;
    }
    const MIN_VBLANK_PS: u64 = 460_000_000; // 460 µs
    const V_FRONT: u32 = 3;
    const MIN_V_BACK: u32 = 6;
    let w = w / 8 * 8;
    let vsync = match (w * 3 == h * 4, w * 9 == h * 16, w * 10 == h * 16, w * 4 == h * 5, w * 9 == h * 15) {
        (true, ..) => 4,
        (_, true, ..) => 5,
        (_, _, true, ..) => 6,
        (_, _, _, true, _) | (.., true) => 7,
        _ => 10,
    };
    let frame_ps = 1_000_000_000_000u64 / refresh as u64;
    let line_ps = frame_ps.checked_sub(MIN_VBLANK_PS)? / h as u64;
    if line_ps == 0 {
        return None;
    }
    let vbi = MIN_VBLANK_PS.div_ceil(line_ps) as u32;
    let vbi = vbi.max(V_FRONT + vsync + MIN_V_BACK);
    let vtotal = h + vbi;
    let htotal = w + 160;
    // Pixel clock in steps of 0.25 MHz, rounded down.
    let hz = refresh as u64 * vtotal as u64 * htotal as u64;
    let clock_khz = (hz / 250_000 * 250) as u32;
    Some(Timing {
        clock_khz,
        hactive: w,
        hsync_start: w + 48,
        hsync_end: w + 80,
        htotal,
        vactive: h,
        vsync_start: h + V_FRONT,
        vsync_end: h + V_FRONT + vsync,
        vtotal,
        interlaced: false,
        hsync_positive: true,
        vsync_positive: false,
        preferred: false,
        source: Source::Standard,
    })
}

/// CTA-861 formats by VIC: (vic, w, h, clock kHz, htotal, hfront, hsync,
/// vtotal (frame), vfront, vsync, interlaced, +sync). The common subset:
/// SD, 720p, 1080i/p and 4K.
type Vic = (u8, u32, u32, u32, u32, u32, u32, u32, u32, u32, bool, bool);

const VICS: [Vic; 23] = [
    (1, 640, 480, 25_175, 800, 16, 96, 525, 10, 2, false, false),
    (2, 720, 480, 27_000, 858, 16, 62, 525, 9, 6, false, false),
    (3, 720, 480, 27_000, 858, 16, 62, 525, 9, 6, false, false),
    (4, 1280, 720, 74_250, 1650, 110, 40, 750, 5, 5, false, true),
    (5, 1920, 1080, 74_250, 2200, 88, 44, 1125, 2, 5, true, true),
    (16, 1920, 1080, 148_500, 2200, 88, 44, 1125, 4, 5, false, true),
    (17, 720, 576, 27_000, 864, 12, 64, 625, 5, 5, false, false),
    (18, 720, 576, 27_000, 864, 12, 64, 625, 5, 5, false, false),
    (19, 1280, 720, 74_250, 1980, 440, 40, 750, 5, 5, false, true),
    (20, 1920, 1080, 74_250, 2640, 528, 44, 1125, 2, 5, true, true),
    (31, 1920, 1080, 148_500, 2640, 528, 44, 1125, 4, 5, false, true),
    (32, 1920, 1080, 74_250, 2750, 638, 44, 1125, 4, 5, false, true),
    (33, 1920, 1080, 74_250, 2640, 528, 44, 1125, 4, 5, false, true),
    (34, 1920, 1080, 74_250, 2200, 88, 44, 1125, 4, 5, false, true),
    (63, 1920, 1080, 297_000, 2200, 88, 44, 1125, 4, 5, false, true),
    (64, 1920, 1080, 297_000, 2640, 528, 44, 1125, 4, 5, false, true),
    (93, 3840, 2160, 297_000, 5500, 1276, 88, 2250, 8, 10, false, true),
    (94, 3840, 2160, 297_000, 5280, 1056, 88, 2250, 8, 10, false, true),
    (95, 3840, 2160, 297_000, 4400, 176, 88, 2250, 8, 10, false, true),
    (96, 3840, 2160, 594_000, 5280, 1056, 88, 2250, 8, 10, false, true),
    (97, 3840, 2160, 594_000, 4400, 176, 88, 2250, 8, 10, false, true),
    (101, 4096, 2160, 594_000, 5280, 968, 88, 2250, 8, 10, false, true),
    (102, 4096, 2160, 594_000, 4400, 88, 88, 2250, 8, 10, false, true),
];

/// The timing of CTA-861 format `vic`, if it is in our table.
pub fn vic_timing(vic: u8) -> Option<Timing> {
    let &(_, w, h, clock, htotal, hfront, hsync, vtotal, vfront, vsync, interlaced, pos) =
        VICS.iter().find(|v| v.0 == vic)?;
    // Interlaced front porch and sync are per field; the frame has twice the lines.
    let (vfront, vsync) = if interlaced { (vfront * 2, vsync * 2) } else { (vfront, vsync) };
    Some(Timing {
        clock_khz: clock,
        hactive: w,
        hsync_start: w + hfront,
        hsync_end: w + hfront + hsync,
        htotal,
        vactive: h,
        vsync_start: h + vfront,
        vsync_end: h + vfront + vsync,
        vtotal,
        interlaced,
        hsync_positive: pos,
        vsync_positive: pos,
        preferred: false,
        source: Source::Cta,
    })
}

fn cta_block(b: &[u8]) -> (Cta, Vec<Timing>) {
    let mut cta = Cta { revision: b[1], ..Default::default() };
    let mut timings = Vec::new();
    let dtd_start = (b[2] as usize).min(127);
    if b[1] >= 2 {
        cta.underscan = b[3] & 0x80 != 0;
        cta.basic_audio = b[3] & 0x40 != 0;
        cta.ycbcr444 = b[3] & 0x20 != 0;
        cta.ycbcr422 = b[3] & 0x10 != 0;
    }
    // The data block collection, revision 3 on (4..dtd_start).
    let mut native = Vec::new();
    let mut others = Vec::new();
    if b[1] >= 3 && dtd_start > 4 {
        let mut i = 4;
        while i < dtd_start {
            let tag = b[i] >> 5;
            let len = (b[i] & 0x1F) as usize;
            let Some(body) = b.get(i + 1..(i + 1 + len).min(dtd_start)) else { break };
            match tag {
                // Video data block: short video descriptors.
                2 => {
                    for &svd in body {
                        let (vic, is_native) = match svd {
                            1..=64 => (svd, false),
                            129..=192 => (svd & 0x7F, true),
                            _ => (svd, false),
                        };
                        if vic == 0 {
                            continue;
                        }
                        if is_native {
                            native.push(vic)
                        } else {
                            others.push(vic)
                        }
                    }
                }
                // Vendor-specific: the HDMI 1.x OUI 00-0C-03 (little-endian).
                3 if body.len() >= 5 && body[..3] == [0x03, 0x0C, 0x00] => {
                    cta.hdmi = true;
                    cta.hdmi_address = u16::from_be_bytes([body[3], body[4]]);
                }
                _ => {}
            }
            i += 1 + len;
        }
    }
    cta.vics = native.iter().chain(others.iter()).copied().collect();
    for (k, &vic) in cta.vics.iter().enumerate() {
        if let Some(mut t) = vic_timing(vic) {
            // A native format the sink prefers, when no DTD says otherwise.
            t.preferred = k == 0 && !native.is_empty() && b[3] & 0x0F == 0 && dtd_start <= 4;
            timings.push(t);
        }
    }
    // Detailed timing descriptors fill the rest of the block.
    if dtd_start >= 4 {
        let mut i = dtd_start;
        while i + 18 <= 127 {
            let d = &b[i..i + 18];
            if d[0] == 0 && d[1] == 0 {
                break;
            }
            if let Some(t) = detailed(d) {
                timings.push(t);
            }
            i += 18;
        }
    }
    (cta, timings)
}

/// DisplayID 1.x extension: type I detailed timings (data block 0x03).
fn displayid_block(b: &[u8]) -> Vec<Timing> {
    let mut out = Vec::new();
    // b[0] = 0x70, then the DisplayID section: version, length, type, count.
    let section_len = b[2] as usize;
    let end = (5 + section_len).min(127);
    let mut i = 5;
    while i + 3 <= end {
        let (tag, len) = (b[i], b[i + 2] as usize);
        let Some(body) = b.get(i + 3..(i + 3 + len).min(end)) else { break };
        if tag == 0x03 {
            for d in body.as_chunks::<20>().0 {
                let u16le = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as u32;
                let clock = (d[0] as u32 | (d[1] as u32) << 8 | (d[2] as u32) << 16) + 1;
                let hactive = u16le(4) + 1;
                let hblank = u16le(6) + 1;
                let hfront = (u16le(8) & 0x7FFF) + 1;
                let hsync = u16le(10) + 1;
                let vactive = u16le(12) + 1;
                let vblank = u16le(14) + 1;
                let vfront = (u16le(16) & 0x7FFF) + 1;
                let vsync = u16le(18) + 1;
                out.push(Timing {
                    clock_khz: clock * 10,
                    hactive,
                    hsync_start: hactive + hfront,
                    hsync_end: hactive + hfront + hsync,
                    htotal: hactive + hblank,
                    vactive,
                    vsync_start: vactive + vfront,
                    vsync_end: vactive + vfront + vsync,
                    vtotal: vactive + vblank,
                    interlaced: d[3] & 0x10 != 0,
                    hsync_positive: d[9] & 0x80 != 0,
                    vsync_positive: d[17] & 0x80 != 0,
                    // Bit 7 of the options byte: the preferred timing.
                    preferred: d[3] & 0x80 != 0,
                    source: Source::DisplayId,
                });
            }
        }
        i += 3 + len;
    }
    out
}

#[cfg(test)]
mod tests;
