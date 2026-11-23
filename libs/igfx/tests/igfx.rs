//! Host tests of the Intel display library: tables, encodings and divider
//! arithmetic against hand-computed values from the PRM formulas, and the
//! controller drivers and sequences against the simulated display engine.

use aurora_igfx::aux::{self, AuxCh, AuxError, Reply};
use aurora_igfx::ddb::{self, Ddb, NoCursor, Wm};
use aurora_igfx::dp::{self, dpcd, Drive, Link, SinkCaps, TrainError, Trainer};
use aurora_igfx::gmbus::Gmbus;
use aurora_igfx::gtt::{self, Space};
use aurora_igfx::mock::{self, Mock, Sink};
use aurora_igfx::platform::{self, BacklightLayout, Pch, Platform};
use aurora_igfx::regs::{self, Transcoder};
use aurora_igfx::takeover::{self, Decline, Output};
use aurora_igfx::timing::{self, Timings};
use aurora_igfx::{backlight, dpll, modeset, plane, Mmio};

fn t1080() -> Timings {
    Timings::new((1920, 2008, 2052, 2200), (1080, 1084, 1089, 1125), true, true)
}

fn t720() -> Timings {
    Timings::new((1280, 1390, 1430, 1650), (720, 725, 730, 750), true, true)
}

// ------------------------------------------------------------ platform

#[test]
fn device_table() {
    let cases = [
        (0x1916, Platform::Skylake, 9),
        (0x5917, Platform::KabyLake, 9),
        (0x87C0, Platform::KabyLake, 9),
        (0x3EA0, Platform::CoffeeLake, 9),
        (0x3E92, Platform::CoffeeLake, 9),
        (0x9BC4, Platform::CometLake, 9),
        (0x5A85, Platform::Broxton, 9),
        (0x3185, Platform::GeminiLake, 10),
        (0x8A52, Platform::IceLake, 11),
        (0x4E61, Platform::ElkhartLake, 11),
        (0x9A49, Platform::TigerLake, 12),
        (0x4C8A, Platform::RocketLake, 12),
        (0x4680, Platform::AlderLakeS, 12),
        (0xA780, Platform::RaptorLakeS, 12),
        (0x46A6, Platform::AlderLakeP, 13),
        (0x46D1, Platform::AlderLakeN, 13),
        (0xA7A0, Platform::RaptorLakeP, 13),
    ];
    for (id, p, d) in cases {
        let info = platform::lookup(id).unwrap_or_else(|| panic!("{id:#06x} missing"));
        assert_eq!((info.platform, info.display), (p, d), "{id:#06x}");
    }
    // Not driven: Haswell, Broadwell, Meteor Lake, Arc.
    for id in [0x0416, 0x1616, 0x7D55, 0x56A0] {
        assert!(platform::lookup(id).is_none(), "{id:#06x}");
    }
    assert_eq!(platform::unsupported_reason(0x1616), "an older generation (Broadwell or before)");
    assert_eq!(platform::unsupported_reason(0x7D55), "a newer generation (display 14 or later)");
    // Every id appears once.
    let mut all: Vec<u16> = platform::DEVICES.iter().flat_map(|(ids, _)| ids.iter().copied()).collect();
    let n = all.len();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), n, "duplicate device ids");
}

#[test]
fn platform_facts() {
    let skl = platform::lookup(0x1916).unwrap();
    assert!(skl.skylake_class() && skl.edp_transcoder());
    assert_eq!((skl.pch, skl.backlight()), (Pch::Spt, BacklightLayout::Split16));
    assert_eq!((skl.gmbus_pin(1), skl.gmbus_pin(2), skl.gmbus_pin(3)), (Some(5), Some(4), Some(6)));
    let cfl = platform::lookup(0x3EA0).unwrap();
    assert_eq!((cfl.backlight(), cfl.gmbus_pin(3)), (BacklightLayout::Bxt, Some(4)));
    let tgl = platform::lookup(0x9A49).unwrap();
    assert!(!tgl.edp_transcoder() && !tgl.skylake_class());
    assert_eq!((tgl.pipes, tgl.gmbus_pin(0)), (4, Some(1)));
    assert!(!platform::lookup(0x5A85).unwrap().skylake_class());
}

// -------------------------------------------------------------- timing

#[test]
fn timing_registers() {
    let r = t1080().to_regs();
    // Values minus one, total/end high, active/start low.
    assert_eq!(r.htotal, 0x0897_077F);
    assert_eq!(r.hblank, 0x0897_077F);
    assert_eq!(r.hsync, 0x0803_07D7);
    assert_eq!(r.vtotal, 0x0464_0437);
    assert_eq!(r.vsync, 0x0440_043B);
    assert_eq!(Timings::from_regs(&r, true, true), t1080());
    assert!(t1080().sane());
    assert_eq!(timing::pipesrc(1920, 1080), 0x077F_0437);
    assert_eq!(timing::pipesrc_size(0x077F_0437), (1920, 1080));
    assert_eq!(t1080().refresh_mhz(148_500), 60_000);
    let mut bad = t1080();
    bad.hsync_end = 2300;
    assert!(!bad.sane());
}

// ---------------------------------------------------------------- DPLL

#[test]
fn hdmi_dpll_148_5() {
    // AFE 742.5 MHz. Only central 9000 MHz has a divider inside +1/-6 %:
    // P = 12 gives 8910 MHz (-1.0 %).
    let w = dpll::hdmi(148_500).unwrap();
    assert_eq!((w.dco_khz, w.central_mhz), (8_910_000, 9000));
    assert_eq!((w.p0, w.p1, w.p2), (2, 3, 2));
    // DCO / 24 MHz = 371.25: integer 371, fraction 0.25 × 0x8000.
    assert_eq!(w.cfgcr1, 1 << 31 | 0x2000 << 9 | 371);
    // QDIV mode + ratio 3, KDIV 2 (1), PDIV 2 (1), central 9000 (1).
    assert_eq!(w.cfgcr2, 0x80 | 3 << 8 | 1 << 5 | 1 << 2 | 1);
    assert_eq!(dpll::hdmi_clock(w.cfgcr1, w.cfgcr2), Some(148_500));
}

#[test]
fn hdmi_dpll_25_175() {
    // AFE 125.875 MHz: best is P = 76 at 9566.5 MHz (-0.35 % of 9600).
    let w = dpll::hdmi(25_175).unwrap();
    assert_eq!((w.dco_khz, w.central_mhz), (9_566_500, 9600));
    assert_eq!((w.p0, w.p1, w.p2), (2, 19, 2));
    // 9566.5 / 24 = 398.6041…: 398 and 19797/32768.
    assert_eq!(w.cfgcr1 & 0x1FF, 398);
    assert_eq!(w.cfgcr1 >> 9 & 0x7FFF, 19797);
    assert_eq!(w.cfgcr2, 0x80 | 19 << 8 | 1 << 5 | 1 << 2);
    assert!(dpll::hdmi_clock(w.cfgcr1, w.cfgcr2).unwrap().abs_diff(25_175) <= 1);
}

#[test]
fn hdmi_dpll_odd_and_limits() {
    // 297 MHz (4K30): AFE 1485 MHz; P = 6 gives 8910 MHz at central 9000.
    let w = dpll::hdmi(297_000).unwrap();
    assert_eq!((w.dco_khz, w.p0 * w.p1 * w.p2), (8_910_000, 6));
    // 594 MHz: AFE 2970 MHz; no even divider fits, odd P = 3 does.
    let w = dpll::hdmi(594_000).unwrap();
    assert_eq!((w.dco_khz, w.p0, w.p1, w.p2), (8_910_000, 3, 1, 1));
    // Every clock from 25 to 300 MHz has a setting inside the DCO range.
    for clock in (25_000..300_000).step_by(250) {
        let w = dpll::hdmi(clock).unwrap_or_else(|| panic!("{clock}"));
        let c = w.central_mhz * 1000;
        assert!(w.dco_khz <= c + c / 100 && w.dco_khz >= c - 6 * c / 100, "{clock}");
        assert_eq!(w.dco_khz, clock as u64 * 5 * (w.p0 * w.p1 * w.p2) as u64);
    }
}

#[test]
fn dpll_divider_split() {
    for p in [
        3u64, 4, 5, 6, 7, 8, 9, 10, 12, 14, 15, 16, 18, 20, 21, 24, 28, 30, 32, 35, 36, 40, 42, 44, 48, 52, 54, 56, 60,
        64, 66, 68, 70, 72, 76, 78, 80, 84, 88, 90, 92, 96, 98,
    ] {
        let (a, b, c) = dpll::split(p).unwrap_or_else(|| panic!("P {p}"));
        assert_eq!((a * b * c) as u64, p, "P {p}");
        assert!(matches!(a, 1 | 2 | 3 | 7) && matches!(c, 1 | 2 | 3 | 5) && b <= 255, "P {p}");
    }
}

#[test]
fn dp_rate_codes() {
    for (rate, code) in [(162_000, 2), (270_000, 1), (540_000, 0), (216_000, 4), (324_000, 3), (432_000, 5)] {
        assert_eq!(dpll::dp_rate_code(rate), Some(code));
        assert_eq!(dpll::dp_rate_from_code(code), Some(rate));
    }
    assert_eq!(dpll::dp_rate_code(810_000), None);
}

// ------------------------------------------------------------------ DP

fn caps(max_rate: u8, lanes: u8) -> SinkCaps {
    let mut c = [0u8; 16];
    c[0] = 0x12;
    c[1] = max_rate;
    c[2] = lanes | 0x80;
    SinkCaps::parse(&c, None).unwrap()
}

#[test]
fn link_selection() {
    let all = [162_000, 270_000, 540_000];
    let hbr2 = caps(0x14, 4);
    assert_eq!(hbr2.rates, vec![162_000, 270_000, 540_000]);
    // 1080p60: 3.564 Gbit/s fits RBR on four lanes.
    assert_eq!(dp::choose_link(148_500, 24, &all, 4, &hbr2), Some(Link { rate: 162_000, lanes: 4 }));
    // 4K60 (CVT-RB, 533.25 MHz): 12.8 Gbit/s needs HBR2.
    assert_eq!(dp::choose_link(533_250, 24, &all, 4, &hbr2), Some(Link { rate: 540_000, lanes: 4 }));
    // A two-lane HBR sink can't.
    assert_eq!(dp::choose_link(533_250, 24, &all, 4, &caps(0x0A, 2)), None);
    // The source's lanes limit too.
    assert_eq!(dp::choose_link(148_500, 24, &all, 2, &hbr2), Some(Link { rate: 270_000, lanes: 2 }));
    assert!(SinkCaps::parse(&[0; 16], None).is_none());
}

#[test]
fn edp_rate_table() {
    let mut c = [0u8; 16];
    c[0] = 0x13;
    c[2] = 4;
    let mut t = [0u8; 16];
    // 8100 (1.62 Gbit/s) and 13500 (2.7) in 200 kHz units.
    t[..4].copy_from_slice(&[0xA4, 0x1F, 0xBC, 0x34]);
    let s = SinkCaps::parse(&c, Some(&t)).unwrap();
    assert_eq!(s.rates, vec![162_000, 270_000]);
    assert!(s.rate_table);
}

#[test]
fn m_n_values() {
    let mn = dp::m_n(148_500, 24, Link { rate: 270_000, lanes: 4 });
    // 148.5 × 24 / (8 × 4 × 270) = 0.4125; 148.5 / 270 = 0.55; N = 0x8000.
    assert_eq!((mn.data_m, mn.data_n), (13_516, 0x8000));
    assert_eq!((mn.link_m, mn.link_n), (18_022, 0x8000));
}

#[test]
fn drive_levels() {
    assert_eq!(Drive { swing: 0, pre: 0 }.skl_entry(), 0);
    assert_eq!(Drive { swing: 1, pre: 2 }.skl_entry(), 6);
    assert_eq!(Drive { swing: 2, pre: 1 }.skl_entry(), 8);
    assert_eq!(Drive { swing: 3, pre: 0 }.skl_entry(), 9);
    assert_eq!(Drive { swing: 3, pre: 2 }.clamp(), Drive { swing: 3, pre: 0 });
    assert_eq!(
        Drive { swing: 2, pre: 1 }.lane_set(),
        2 | 1 << 3 | dpcd::MAX_SWING_REACHED | dpcd::MAX_PRE_EMPHASIS_REACHED
    );
    assert_eq!(Drive { swing: 1, pre: 0 }.lane_set(), 1);
}

/// A sink that needs `need` swing for clock recovery and `eq_tries` extra
/// polls before channel equalization.
struct FakeLink {
    need: u8,
    eq_tries: u32,
    drive: Drive,
    pattern: u8,
    patterns: Vec<u8>,
}

impl Trainer for FakeLink {
    type Error = ();
    fn set_pattern(&mut self, tp: u8) -> Result<(), ()> {
        self.pattern = tp;
        self.patterns.push(tp);
        Ok(())
    }
    fn set_drive(&mut self, d: Drive, _lanes: u8) -> Result<(), ()> {
        self.drive = d;
        Ok(())
    }
    fn status(&mut self) -> Result<[u8; 6], ()> {
        let cr = self.drive.swing >= self.need;
        let mut lane = if cr { dpcd::CR_DONE } else { 0 };
        let mut align = 0;
        if cr && self.pattern >= 2 {
            if self.eq_tries == 0 {
                lane |= dpcd::CHANNEL_EQ_DONE | dpcd::SYMBOL_LOCKED;
                align = 1;
            } else {
                self.eq_tries -= 1;
            }
        }
        let both = lane | lane << 4;
        let adj = self.need.min(3) | self.need.min(3) << 4;
        Ok([both, both, align, 0, adj, adj])
    }
    fn delay_us(&mut self, _us: u32) {}
}

#[test]
fn link_training() {
    let mut l = FakeLink { need: 2, eq_tries: 2, drive: Drive::default(), pattern: 0, patterns: vec![] };
    assert_eq!(dp::train(&mut l, 4, false, 100, 400), Ok(Drive { swing: 2, pre: 0 }));
    assert_eq!(l.patterns, vec![1, 2, 0]);
    let mut l = FakeLink { need: 1, eq_tries: 0, drive: Drive::default(), pattern: 0, patterns: vec![] };
    assert!(dp::train(&mut l, 2, true, 100, 400).is_ok());
    assert_eq!(l.patterns, vec![1, 3, 0]);
    // A sink that never recovers the clock.
    let mut l = FakeLink { need: 4, eq_tries: 0, drive: Drive::default(), pattern: 0, patterns: vec![] };
    assert_eq!(dp::train(&mut l, 4, false, 100, 400), Err(TrainError::ClockRecovery));
    assert_eq!(l.pattern, 0);
    // One that never equalizes.
    let mut l = FakeLink { need: 0, eq_tries: 100, drive: Drive::default(), pattern: 0, patterns: vec![] };
    assert_eq!(dp::train(&mut l, 1, false, 100, 400), Err(TrainError::ChannelEq));
}

// ----------------------------------------------------------------- AUX

#[test]
fn aux_framing() {
    assert_eq!(aux::header(aux::NATIVE_READ, 0x00202, 6), ([0x90, 0x02, 0x02, 0x05], 4));
    assert_eq!(aux::header(aux::NATIVE_WRITE, 0x70123, 1), ([0x87, 0x01, 0x23, 0x00], 4));
    // Address-only I²C (a stop): three bytes.
    assert_eq!(aux::header(aux::I2C_READ, 0x50, 0).1, 3);
    assert_eq!(aux::header(aux::I2C_WRITE | aux::I2C_MOT, 0x50, 1).0, [0x40, 0x00, 0x50, 0x00]);
    let p = aux::pack(&[1, 2, 3, 4, 5]);
    assert_eq!(p[..2], [0x0102_0304, 0x0500_0000]);
    let mut out = [0u8; 5];
    aux::unpack(&p, &mut out);
    assert_eq!(out, [1, 2, 3, 4, 5]);
    assert_eq!(aux::decode_reply(0x00), Reply::Ack);
    assert_eq!(aux::decode_reply(0x10), Reply::Nack);
    assert_eq!(aux::decode_reply(0x20), Reply::Defer);
    assert_eq!(aux::decode_reply(0x40), Reply::I2cNack);
    assert_eq!(aux::decode_reply(0x80), Reply::I2cDefer);
}

#[test]
fn aux_on_the_simulated_engine() {
    let m = Mock::new(3);
    let edid = mock::test_edid(b"DP TEST");
    m.attach_sink(2, Sink::new(0x14, 4, edid.clone()));
    let aux = AuxCh { mmio: &m, ch: 2 };
    let mut c = [0u8; 16];
    aux.read(dpcd::REV, &mut c).unwrap();
    assert_eq!(&c[..3], &[0x12, 0x14, 0x84]);
    aux.write_byte(dpcd::LINK_BW_SET, 0x0A).unwrap();
    assert_eq!(aux.read_byte(dpcd::LINK_BW_SET), Ok(0x0A));
    let mut block = [0u8; 128];
    aux.read_edid_block(0, &mut block).unwrap();
    assert_eq!(&block[..], &edid[..]);
    let e = aurora_edid::parse(&block).unwrap();
    assert_eq!(e.name.as_deref(), Some("DP TEST"));
    assert_eq!((e.timings[0].hactive, e.timings[0].vactive, e.timings[0].clock_khz), (1920, 1080, 148_500));
    // Nothing on channel 1.
    assert_eq!(AuxCh { mmio: &m, ch: 1 }.read_byte(0), Err(AuxError::Timeout));
    let caps = modeset::sink_caps(&m, 2).unwrap();
    assert_eq!((caps.max_lanes, caps.rates.len(), caps.enhanced_framing), (4, 3, true));
}

// --------------------------------------------------------------- GMBUS

#[test]
fn gmbus_reads_edid() {
    let m = Mock::new(3);
    let edid = mock::test_edid(b"HDMI TEST");
    m.attach_ddc(5, edid.clone());
    let g = Gmbus { mmio: &m, pin: 5 };
    let mut block = [0u8; 128];
    g.read_edid_block(0, &mut block).unwrap();
    assert_eq!(&block[..], &edid[..]);
    assert_eq!(m.peek(gmbus_reg0()), 0, "the pin is released");
    // Through the generic I²C interface too.
    let mut g = Gmbus { mmio: &m, pin: 5 };
    let mut hdr = [0u8; 8];
    aurora_i2c::Bus::transfer(&mut g, 0x50, &[0], &mut hdr).unwrap();
    assert_eq!(hdr, [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0]);
    // Nothing on pin 4.
    let g = Gmbus { mmio: &m, pin: 4 };
    assert_eq!(g.read_edid_block(0, &mut block), Err(aurora_i2c::Error::AddressNack));
}

fn gmbus_reg0() -> u32 {
    aurora_igfx::gmbus::GMBUS0
}

// ------------------------------------------------------------ takeover

fn skl() -> platform::Info {
    platform::lookup(0x1916).unwrap()
}

#[test]
fn takeover_reads_firmware_edp() {
    let info = skl();
    let m = Mock::new(3);
    m.firmware(&info, Transcoder::Edp, 0, 0, regs::DDI_MODE_DP_SST, &t1080(), 0, 7680);
    let fw = takeover::read(&m, &info).unwrap();
    assert_eq!(fw.pipes.len(), 1);
    let p = &fw.pipes[0];
    assert_eq!((p.pipe, p.trans, p.port, p.output, p.lanes, p.bpc), (0, Transcoder::Edp, 0, Output::Edp, 4, 8));
    assert_eq!(p.timings, t1080());
    assert_eq!((p.src, p.scaled), ((1920, 1080), false));
    assert_eq!((p.primary.stride_bytes, p.primary.surf, p.primary.size), (7680, 0, (1920, 1080)));
    assert!(fw.panel_on);
    assert_eq!(fw.cdclk_khz, Some(675_000));
}

#[test]
fn takeover_reads_tiger_lake_hdmi() {
    let info = platform::lookup(0x9A49).unwrap();
    let m = Mock::new(4);
    // TGL encodes the port as index + 1: DDI B on transcoder/pipe B.
    m.firmware(&info, Transcoder::Pipe(1), 1, 1, regs::DDI_MODE_HDMI, &t720(), 0x40_0000, 5120);
    let fw = takeover::read(&m, &info).unwrap();
    let p = &fw.pipes[0];
    assert_eq!((p.pipe, p.port, p.output), (1, 1, Output::Hdmi));
    assert_eq!(p.primary.surf, 0x40_0000);
    assert_eq!(fw.cdclk_khz, None);
}

#[test]
fn takeover_declines_the_unexpected() {
    let info = skl();
    let lit = || {
        let m = Mock::new(3);
        m.firmware(&info, Transcoder::Edp, 0, 0, regs::DDI_MODE_DP_SST, &t1080(), 0, 7680);
        m
    };
    assert_eq!(takeover::read(&Mock::new(3), &info), Err(Decline::NoActivePipe));
    let m = lit();
    m.poke(regs::plane_ctl(0, 0), m.peek(regs::plane_ctl(0, 0)) | 1 << 10);
    assert!(matches!(takeover::read(&m, &info), Err(Decline::Tiled(0, _))));
    let m = lit();
    m.poke(regs::plane_ctl(0, 0), m.peek(regs::plane_ctl(0, 0)) | 2);
    assert!(matches!(takeover::read(&m, &info), Err(Decline::Rotated(0, _))));
    let m = lit();
    m.poke(regs::plane_ctl(0, 0), regs::PLANE_CTL_ENABLE | 0xE << 24);
    assert!(matches!(takeover::read(&m, &info), Err(Decline::Format(0, _))));
    let m = lit();
    m.poke(regs::plane_pos(0, 0), 10);
    assert_eq!(takeover::read(&m, &info), Err(Decline::Coverage(0)));
    let m = lit();
    m.poke(regs::plane_stride(0, 0), 10);
    assert_eq!(takeover::read(&m, &info), Err(Decline::Stride(0)));
    let m = lit();
    m.poke(regs::trans_ddi_func_ctl(Transcoder::Edp), m.peek(regs::trans_ddi_func_ctl(Transcoder::Edp)) | 1 << 24);
    assert!(matches!(takeover::read(&m, &info), Err(Decline::OutputMode(0, _))));
    let m = lit();
    m.poke(regs::transconf(Transcoder::Edp), regs::TRANSCONF_ENABLE | 3 << 21);
    assert_eq!(takeover::read(&m, &info), Err(Decline::Interlaced(0)));
    let m = lit();
    m.poke(regs::hsync(Transcoder::Edp), timing::pack(2300, 2400));
    assert_eq!(takeover::read(&m, &info), Err(Decline::Timings(0)));
    // A second transcoder claiming pipe A.
    let m = lit();
    m.poke(regs::transconf(Transcoder::Pipe(0)), regs::TRANSCONF_ENABLE);
    assert_eq!(takeover::read(&m, &info), Err(Decline::PipeConflict(0)));
}

#[test]
fn pixel_clock_readback() {
    let info = skl();
    let m = Mock::new(3);
    m.firmware(&info, Transcoder::Pipe(1), 1, 2, regs::DDI_MODE_DP_SST, &t1080(), 0, 7680);
    // DDI C on DPLL 2 at HBR, with the link M/N for 148.5 MHz.
    m.poke(regs::DPLL_CTRL1, (regs::dpll_ctrl1_override(2) | 1 << regs::dpll_ctrl1_link_rate_shift(2)) as u32);
    m.poke(regs::DPLL_CTRL2, 2 << regs::ddi_clk_sel_shift(2) | regs::ddi_sel_override(2));
    let mn = dp::m_n(148_500, 24, Link { rate: 270_000, lanes: 4 });
    m.poke(regs::link_m1(Transcoder::Pipe(1)), mn.link_m);
    m.poke(regs::link_n1(Transcoder::Pipe(1)), mn.link_n);
    let fw = takeover::read(&m, &info).unwrap();
    let clock = takeover::pixel_clock(&m, &info, &fw.pipes[0]).unwrap();
    assert!(clock.abs_diff(148_500) < 10, "{clock}");
}

// ------------------------------------------------- planes, DDB and GTT

#[test]
fn plane_values() {
    let fw = regs::PLANE_CTL_ENABLE
        | regs::PLANE_CTL_PIPE_GAMMA
        | regs::PLANE_CTL_FORMAT_2101010
        | regs::PLANE_CTL_ORDER_RGBX;
    assert_eq!(
        plane::primary_ctl(fw, 9),
        regs::PLANE_CTL_ENABLE | regs::PLANE_CTL_PIPE_GAMMA | regs::PLANE_CTL_FORMAT_8888
    );
    assert_eq!(plane::stride(7680), 120);
    assert_eq!(plane::cursor_pos(-5, 10), 10 << 16 | 0x8000 | 5);
    assert_eq!(plane::cursor_pos_decode(plane::cursor_pos(-63, -1)), (-63, -1));
    assert_eq!(plane::premultiply(0x80FF_8000), 0x8080_4000);
    assert_eq!(plane::premultiply(0xFF12_3456), 0xFF12_3456);
    assert_eq!(plane::premultiply(0x00FF_FFFF), 0);
}

#[test]
fn cursor_ddb_plan() {
    let wm = |lines: u32, blocks: u32| Wm { enabled: true, lines, blocks }.encode();
    let mut levels = [0u32; 8];
    levels[..3].copy_from_slice(&[wm(2, 30), wm(4, 90), wm(8, 855)]);
    let plan = ddb::plan_cursor(Ddb { start: 0, end: 860 }.encode(), levels, None).unwrap();
    assert_eq!(plan.plane_ddb, Ddb { start: 0, end: 852 });
    assert_eq!(plan.cursor_ddb, Ddb { start: 852, end: 860 });
    // Level 2 no longer fits plane 1 (855 ≥ 852 blocks): off, with the rest.
    assert!(plan.plane_wm[0].enabled && plan.plane_wm[1].enabled && !plan.plane_wm[2].enabled);
    assert_eq!(plan.cursor_wm0, Wm { enabled: true, lines: 2, blocks: 4 });
    assert_eq!(Ddb::decode(Ddb { start: 852, end: 860 }.encode()), Ddb { start: 852, end: 860 });
    assert_eq!(Ddb::decode(0).size(), 0);
    // A plane with nothing set up, or too little room.
    assert_eq!(ddb::plan_cursor(0, levels, None), Err(NoCursor::PlaneUnset));
    assert_eq!(ddb::plan_cursor(Ddb { start: 0, end: 60 }.encode(), levels, None), Err(NoCursor::TooSmall));
    levels[0] = wm(2, 858);
    assert_eq!(ddb::plan_cursor(Ddb { start: 0, end: 860 }.encode(), levels, None), Err(NoCursor::Level0));
    // Display 12: a SAGV level is derived too.
    levels[0] = wm(2, 30);
    let plan = ddb::plan_cursor(Ddb { start: 0, end: 860 }.encode(), levels, Some(wm(5, 70))).unwrap();
    assert_eq!(plan.cursor_sagv, Some(Wm { enabled: true, lines: 5, blocks: 7 }));
}

#[test]
fn gtt_space() {
    assert_eq!(gtt::ggtt_size(3 << 6), 4 << 30);
    assert_eq!(gtt::ggtt_size(1 << 6), 1 << 30);
    assert_eq!(gtt::stolen_size(0x02 << 8), 64 << 20);
    assert_eq!(gtt::stolen_size(0xF0 << 8), 4 << 20);
    assert_eq!(gtt::pte(0x1_2345_6000), 0x1_2345_6001);
    assert_eq!(gtt::pte_page(0x1_2345_6001), Some(0x1_2345_6000));
    let mut s = Space::new(64 << 20, 128 << 20);
    let a = s.alloc(8 << 20, regs::SURF_ALIGN).unwrap();
    let b = s.alloc(4096, regs::SURF_ALIGN).unwrap();
    assert_eq!(a, 64 << 20);
    assert!(b.is_multiple_of(regs::SURF_ALIGN) && b >= a + (8 << 20) + gtt::GUARD_PAGES * 4096);
    s.free(a, 8 << 20);
    s.free(b, 4096);
    assert_eq!(s.available(), 64 << 20);
    assert!(s.alloc(65 << 20, 4096).is_none());
}

// ------------------------------------------------------------ backlight

#[test]
fn backlight_pwm() {
    let m = Mock::new(3);
    m.firmware(&skl(), Transcoder::Edp, 0, 0, regs::DDI_MODE_DP_SST, &t1080(), 0, 7680);
    let b = backlight::Backlight::probe(&m, BacklightLayout::Split16).unwrap();
    assert_eq!((b.period, b.get(&m)), (1000, 50));
    b.set(&m, 80);
    assert_eq!(m.peek(regs::BLC_PWM_CTL2), 1000 << 16 | 800);
    b.set(&m, 0); // never black
    assert_eq!(b.get(&m), backlight::MIN_PERCENT);
    // Inverted PWM, the newer layout.
    let m = Mock::new(3);
    m.poke(regs::BLC_PWM_CTL1, regs::BLC_PWM_ENABLE | regs::BLC_PWM_POLARITY);
    m.poke(regs::BLC_PWM_CTL2, 2000);
    m.poke(regs::BLC_PWM_DUTY, 500);
    let b = backlight::Backlight::probe(&m, BacklightLayout::Bxt).unwrap();
    assert_eq!(b.get(&m), 75);
    b.set(&m, 100);
    assert_eq!(m.peek(regs::BLC_PWM_DUTY), 0);
    // Off: not ours to drive.
    assert!(backlight::Backlight::probe(&Mock::new(3), BacklightLayout::Bxt).is_none());
}

// ------------------------------------------------------------- modeset

/// Skylake with HDMI on DDI B, pipe/transcoder B, DPLL 1 in HDMI mode at 148.5 MHz.
fn skl_hdmi(m: &Mock) {
    let info = skl();
    m.firmware(&info, Transcoder::Pipe(1), 1, 1, regs::DDI_MODE_HDMI, &t1080(), 0, 7680);
    let w = dpll::hdmi(148_500).unwrap();
    m.poke(regs::DPLL_CTRL1, regs::dpll_ctrl1_override(1) | regs::dpll_ctrl1_hdmi(1));
    m.poke(regs::DPLL_CTRL2, 1 << regs::ddi_clk_sel_shift(1) | regs::ddi_sel_override(1));
    m.poke(regs::dpll_cfgcr1(1), w.cfgcr1);
    m.poke(regs::dpll_cfgcr2(1), w.cfgcr2);
    m.poke(regs::skl_dpll_enable(1), regs::DPLL_ENABLE);
    m.poke(regs::DPLL_STATUS, regs::dpll_lock(0) | regs::dpll_lock(1));
}

#[test]
fn hdmi_mode_switch() {
    let info = skl();
    let m = Mock::new(3);
    skl_hdmi(&m);
    let fw = takeover::read(&m, &info).unwrap();
    let clock = takeover::pixel_clock(&m, &info, &fw.pipes[0]).unwrap();
    assert_eq!(clock, 148_500);
    let from = modeset::snapshot(&m, &info, &fw.pipes[0], clock).unwrap();
    // Larger than the firmware's mode: refused.
    let big = Timings::new((2560, 2608, 2640, 2720), (1440, 1443, 1448, 1481), true, false);
    assert_eq!(modeset::plan(&info, &from, &big, 241_500, None), Err(modeset::Refuse::LargerThanFirmware));
    let to = modeset::plan(&info, &from, &t720(), 74_250, None).unwrap();
    assert_eq!(modeset::switch(&m, &info, &from, &to), modeset::Switch::Done);
    let t = Transcoder::Pipe(1);
    assert_eq!(Timings::from_regs(&timing::read(&m, t), true, true), t720());
    assert_eq!(timing::pipesrc_size(m.peek(regs::pipesrc(1))), (1280, 720));
    let w = dpll::hdmi(74_250).unwrap();
    assert_eq!((m.peek(regs::dpll_cfgcr1(1)), m.peek(regs::dpll_cfgcr2(1))), (w.cfgcr1, w.cfgcr2));
    assert_ne!(m.peek(regs::transconf(t)) & regs::TRANSCONF_STATE, 0);
    assert_ne!(m.peek(regs::trans_ddi_func_ctl(t)) & regs::DDI_FUNC_ENABLE, 0);
    assert_eq!(m.peek(regs::ddi_buf_ctl(1)) & (regs::DDI_BUF_ENABLE | regs::DDI_BUF_IDLE), regs::DDI_BUF_ENABLE);
    assert_eq!(m.peek(regs::DPLL_CTRL2) & regs::ddi_clk_off(1), 0);
    // The caller then shows a framebuffer of the new size.
    let ctl = plane::primary_ctl(fw.pipes[0].primary.ctl, 9);
    plane::flip(&m, 1, &plane::Primary { ctl, stride: plane::stride(5120), surf: 0x100_0000 });
    let fw2 = takeover::read(&m, &info).unwrap();
    assert_eq!(fw2.pipes[0].timings, t720());
}

/// Skylake with DP on DDI C, pipe/transcoder A, DPLL 2 at HBR x4.
fn skl_dp(m: &Mock, sink: Sink) {
    let info = skl();
    m.firmware(&info, Transcoder::Pipe(0), 0, 2, regs::DDI_MODE_DP_SST, &t1080(), 0, 7680);
    m.poke(regs::DPLL_CTRL1, regs::dpll_ctrl1_override(2) | 1 << regs::dpll_ctrl1_link_rate_shift(2));
    m.poke(regs::DPLL_CTRL2, 2 << regs::ddi_clk_sel_shift(2) | regs::ddi_sel_override(2));
    let mn = dp::m_n(148_500, 24, Link { rate: 270_000, lanes: 4 });
    let t = Transcoder::Pipe(0);
    m.poke(regs::data_m1(t), 63 << 25 | mn.data_m);
    m.poke(regs::data_n1(t), mn.data_n);
    m.poke(regs::link_m1(t), mn.link_m);
    m.poke(regs::link_n1(t), mn.link_n);
    m.poke(regs::dp_tp_ctl(2), regs::DP_TP_ENABLE | regs::DP_TP_NORMAL | regs::DP_TP_ENHANCED_FRAMING);
    m.attach_sink(2, sink);
}

#[test]
fn dp_mode_switch_with_link_training() {
    let info = skl();
    let m = Mock::new(3);
    skl_dp(&m, Sink::new(0x14, 4, mock::test_edid(b"DP")));
    let fw = takeover::read(&m, &info).unwrap();
    let p = &fw.pipes[0];
    assert_eq!((p.port, p.output, p.lanes), (2, Output::Dp, 4));
    let clock = takeover::pixel_clock(&m, &info, p).unwrap();
    let from = modeset::snapshot(&m, &info, p, clock).unwrap();
    assert_eq!(from.link, Some(Link { rate: 270_000, lanes: 4 }));
    let caps = modeset::sink_caps(&m, 2).unwrap();
    let to = modeset::plan(&info, &from, &t720(), 74_250, Some(&caps)).unwrap();
    assert_eq!(to.link, Some(Link { rate: 162_000, lanes: 4 }));
    assert_eq!(modeset::switch(&m, &info, &from, &to), modeset::Switch::Done);
    let sink = m.sink(2).unwrap();
    assert_eq!(sink.trained, 1);
    assert_eq!(sink.dpcd[dpcd::LINK_BW_SET as usize], 0x06);
    assert_eq!(sink.dpcd[dpcd::LANE_COUNT_SET as usize], 0x84);
    // The source ends on the normal pattern, at the drive the sink asked for (swing 1: entry 4).
    assert_eq!(m.peek(regs::dp_tp_ctl(2)) & regs::DP_TP_PATTERN_MASK, regs::DP_TP_NORMAL);
    assert_eq!(m.peek(regs::ddi_buf_ctl(2)) & regs::DDI_BUF_TRANS_SELECT_MASK, 4 << 24);
    assert_eq!(m.peek(regs::DPLL_CTRL1) >> regs::dpll_ctrl1_link_rate_shift(2) & 7, 2);
    let mn = dp::m_n(74_250, 24, Link { rate: 162_000, lanes: 4 });
    assert_eq!(m.peek(regs::link_m1(Transcoder::Pipe(0))), mn.link_m);
}

#[test]
fn failed_mode_switch_restores_the_firmware_mode() {
    let info = skl();
    let m = Mock::new(3);
    skl_dp(&m, Sink::new(0x14, 4, mock::test_edid(b"DP")));
    let fw = takeover::read(&m, &info).unwrap();
    let clock = takeover::pixel_clock(&m, &info, &fw.pipes[0]).unwrap();
    let from = modeset::snapshot(&m, &info, &fw.pipes[0], clock).unwrap();
    let caps = modeset::sink_caps(&m, 2).unwrap();
    let to = modeset::plan(&info, &from, &t720(), 74_250, Some(&caps)).unwrap();
    // The new setting's DPLL never locks: the firmware's mode comes back.
    let mut broken = to;
    broken.dpll = 3;
    m.set_dpll_broken(3);
    let r = modeset::switch(&m, &info, &from, &broken);
    assert_eq!(r, modeset::Switch::Restored(modeset::Fail::DpllLock));
    assert_eq!(Timings::from_regs(&timing::read(&m, Transcoder::Pipe(0)), true, true), t1080());
    assert_ne!(m.peek(regs::transconf(Transcoder::Pipe(0))) & regs::TRANSCONF_STATE, 0);
    assert_eq!(m.sink(2).unwrap().dpcd[dpcd::LINK_BW_SET as usize], 0x0A);
}

// ----------------------------------------------- mock vblank and flips

#[test]
fn flips_complete_at_vblank() {
    let info = skl();
    let m = Mock::new(3);
    m.firmware(&info, Transcoder::Edp, 0, 0, regs::DDI_MODE_DP_SST, &t1080(), 0, 7680);
    let p = plane::Primary { ctl: plane::primary_ctl(m.peek(regs::plane_ctl(0, 0)), 9), stride: 120, surf: 0x100_0000 };
    plane::flip(&m, 0, &p);
    assert_eq!(m.peek(regs::plane_surflive(0, 0)), 0, "not before the vblank");
    let frames = m.read(regs::frame_count(0));
    m.vblank();
    assert_eq!(m.read(regs::plane_surflive(0, 0)), 0x100_0000);
    assert_eq!(m.read(regs::frame_count(0)), frames + 1);
    assert_ne!(m.read(regs::de_pipe_iir(0)) & regs::PIPE_PLANE1_FLIP_DONE, 0);
    m.write(regs::de_pipe_iir(0), regs::PIPE_PLANE1_FLIP_DONE);
    assert_eq!(m.read(regs::de_pipe_iir(0)) & regs::PIPE_PLANE1_FLIP_DONE, 0);
}
