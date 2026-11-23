//! Host tests: the device table, the register layouts, timing decoding,
//! takeover readback and every decline, flips, the cursor, AUX with EDID,
//! hot-plug sense and VRAM placement, against the simulated engine.

use aurora_amdgfx::aux::Engine;
use aurora_amdgfx::mock::{test_edid, Mock, Sink};
use aurora_amdgfx::platform::{self, Block, DcnVer, Info, Layout};
use aurora_amdgfx::regs::{self, Inst};
use aurora_amdgfx::takeover::{self, Decline, Gop};
use aurora_amdgfx::{hpd, scanout, vram};
use aurora_igfx::aux::Transport;
use aurora_igfx::timing::Timings;

fn cea1080() -> Timings {
    Timings::new((1920, 2008, 2052, 2200), (1080, 1084, 1089, 1125), true, true)
}

fn gop() -> Gop {
    Gop { offset: 0, width: 1920, height: 1080, stride: 1920, bgr: true }
}

/// The GPU address of VRAM offset 0 (an FB aperture base, 16 MiB aligned).
const FB: u64 = 0xF4_0000_0000;

fn phoenix() -> Info {
    platform::lookup(0x15BF).unwrap()
}

fn polaris() -> Info {
    platform::lookup(0x67DF).unwrap()
}

fn lit(info: Info, pipes: &[u8]) -> Mock {
    let m = Mock::new(info);
    m.firmware(0, &cea1080(), pipes, FB, 1920, (1920, 1080));
    m
}

fn dcn(info: &Info) -> &'static regs::Dcn {
    match info.block.layout() {
        Layout::Dcn(l) => l,
        Layout::Dce(_) => panic!("not DCN"),
    }
}

fn dce(info: &Info) -> &'static regs::Dce {
    match info.block.layout() {
        Layout::Dce(l) => l,
        Layout::Dcn(_) => panic!("not DCE"),
    }
}

#[test]
fn device_table() {
    let cases = [
        (0x67DF, "Polaris 10", Block::Dce112, false),
        (0x699F, "Polaris 12", Block::Dce112, false),
        (0x687F, "Vega 10", Block::Dce120, false),
        (0x15DD, "Raven", Block::Dcn(DcnVer::V10), true),
        (0x1638, "Cezanne", Block::Dcn(DcnVer::V21), true),
        (0x1681, "Rembrandt", Block::Dcn(DcnVer::V312), true),
        (0x15BF, "Phoenix", Block::Dcn(DcnVer::V314), true),
        (0x731F, "Navi 10", Block::Dcn(DcnVer::V20), false),
        (0x73BF, "Navi 21", Block::Dcn(DcnVer::V30), false),
        (0x73FF, "Navi 23", Block::Dcn(DcnVer::V302), false),
        (0x73F0, "Navi 33", Block::Dcn(DcnVer::V321), false),
        (0x743F, "Navi 24", Block::Dcn(DcnVer::V303), false),
        (0x744C, "Navi 31", Block::Dcn(DcnVer::V32), false),
        (0x747E, "Navi 32", Block::Dcn(DcnVer::V32), false),
        (0x7480, "Navi 33", Block::Dcn(DcnVer::V321), false),
    ];
    for (id, asic, block, apu) in cases {
        let i = platform::lookup(id).unwrap_or_else(|| panic!("{id:#x} missing"));
        assert_eq!((i.asic, i.block, i.apu), (asic, block, apu), "{id:#x}");
    }
    for id in [0x7550, 0x150E, 0x9874, 0x7300, 0x1002] {
        assert!(platform::lookup(id).is_none(), "{id:#x} shouldn't be driven");
    }
    assert_eq!(platform::unsupported_reason(0x7550), "RDNA 4 (DCN 4)");
    assert_eq!(platform::unsupported_reason(0x9874), "Carrizo/Stoney (DCE 11.0)");
}

fn dcn_fields(l: &regs::Dcn) -> Vec<(&'static str, &Inst)> {
    vec![
        ("otg_control", &l.otg_control),
        ("otg_h_total", &l.otg_h_total),
        ("otg_h_blank", &l.otg_h_blank),
        ("otg_h_sync", &l.otg_h_sync),
        ("otg_v_total", &l.otg_v_total),
        ("otg_v_blank", &l.otg_v_blank),
        ("otg_v_sync", &l.otg_v_sync),
        ("otg_frame_count", &l.otg_frame_count),
        ("hubp_cntl", &l.hubp_cntl),
        ("surface_config", &l.surface_config),
        ("tiling_config", &l.tiling_config),
        ("viewport_start", &l.viewport_start),
        ("viewport_dim", &l.viewport_dim),
        ("pitch", &l.pitch),
        ("addr", &l.addr),
        ("addr_high", &l.addr_high),
        ("surface_control", &l.surface_control),
        ("flip_control", &l.flip_control),
        ("cur_settings", &l.cur_settings),
        ("cur_control", &l.cur_control),
        ("cur_addr", &l.cur_addr),
        ("cur_size", &l.cur_size),
        ("cur_position", &l.cur_position),
        ("cur_hot_spot", &l.cur_hot_spot),
        ("dpp_cur_control", &l.dpp_cur_control),
        ("hpd_status", &l.hpd_status),
        ("aux_control", &l.aux_control),
        ("aux_sw_data", &l.aux_sw_data),
    ]
}

#[test]
fn layouts() {
    let blocks = [
        Block::Dce112,
        Block::Dce120,
        Block::Dcn(DcnVer::V10),
        Block::Dcn(DcnVer::V20),
        Block::Dcn(DcnVer::V21),
        Block::Dcn(DcnVer::V30),
        Block::Dcn(DcnVer::V301),
        Block::Dcn(DcnVer::V302),
        Block::Dcn(DcnVer::V303),
        Block::Dcn(DcnVer::V312),
        Block::Dcn(DcnVer::V314),
        Block::Dcn(DcnVer::V315),
        Block::Dcn(DcnVer::V316),
        Block::Dcn(DcnVer::V32),
        Block::Dcn(DcnVer::V321),
    ];
    for b in blocks {
        let l = b.layout();
        assert!(regs::max_offset(l) < 0x40000, "{}: registers beyond 256 KiB", b.name());
        if let Layout::Dcn(d) = l {
            let fields = dcn_fields(d);
            let mut seen = std::collections::BTreeSet::new();
            for (name, inst) in &fields {
                // Every block has at least two of everything.
                assert!(inst[0] != 0 && inst[1] != 0, "{}: {name} missing", b.name());
                assert!(inst[0].is_multiple_of(4), "{}: {name} unaligned", b.name());
                assert!(seen.insert(inst[0]), "{}: {name} shares an offset", b.name());
            }
        }
    }
    // Spot checks against the headers: DCN 1.0's OTG0_OTG_CONTROL is
    // 0x1b41 + segment 2 (0x34c0) dwords; DCE 11.2's DCP0 surface at 0x1a04.
    assert_eq!(dcn(&platform::lookup(0x15DD).unwrap()).otg_control[0], (0x1b41 + 0x34c0) * 4);
    assert_eq!(dce(&polaris()).grph_addr[0], 0x1a04 * 4);
    assert_eq!(dce(&polaris()).grph_addr[1], 0x1c04 * 4);
}

#[test]
fn timing_codec() {
    for t in [(1920, 2008, 2052, 2200), (1366, 1414, 1446, 1526), (3840, 4016, 4104, 4400), (1080, 1084, 1089, 1125)] {
        let (total, blank, sync) = takeover::encode(t);
        assert_eq!(takeover::decode(total, blank, sync, regs::OTG_FIELD), Some(t));
    }
    // Blank start before blank end: nonsense.
    assert_eq!(takeover::decode(2199, 10 | 200 << 16, 44 << 16, regs::OTG_FIELD), None);
}

#[test]
fn dcn_takeover() {
    let m = lit(phoenix(), &[0]);
    let fw = takeover::read(&m, &phoenix(), &gop()).expect("declined");
    assert_eq!(fw.heads.len(), 1);
    let h = &fw.heads[0];
    assert_eq!((h.otg, h.pipes.as_slice(), h.src, h.pitch, h.addr), (0, &[0u8][..], (1920, 1080), 1920, FB));
    assert_eq!(h.timings, cea1080());
    assert!(!h.scaled && !h.immediate);
    assert_eq!(fw.gpu_addr(&gop(), 8 << 20), FB + (8 << 20));
    assert_eq!(m.writes(), 0, "reading back wrote to the hardware");

    // A wide mode split across two pipes.
    let m = lit(phoenix(), &[0, 1]);
    let fw = takeover::read(&m, &phoenix(), &gop()).expect("declined");
    assert_eq!(fw.heads[0].pipes, vec![0, 1]);

    // A smaller framebuffer scaled to the panel.
    let m = Mock::new(phoenix());
    m.firmware(0, &cea1080(), &[0], FB, 1280, (1280, 720));
    let g = Gop { width: 1280, height: 720, stride: 1280, ..gop() };
    let fw = takeover::read(&m, &phoenix(), &g).expect("declined");
    assert!(fw.heads[0].scaled);
}

#[test]
fn dcn_declines() {
    let info = phoenix();
    let l = dcn(&info);
    let check = |setup: &dyn Fn(&Mock), want: Decline| {
        let m = lit(info, &[0]);
        setup(&m);
        assert_eq!(takeover::read(&m, &info, &gop()), Err(want));
        assert_eq!(m.writes(), 0);
    };
    check(&|m| m.poke(l.otg_control[0], 0), Decline::NoActiveOtg);
    check(&|m| m.poke(l.otg_interlace[0], 1), Decline::Interlaced(0));
    check(&|m| m.poke(l.tiling_config[0], 9), Decline::Tiled(0, 9));
    check(&|m| m.poke(l.surface_control[0], regs::PRIMARY_SURFACE_DCC_EN), Decline::Compressed(0));
    check(&|m| m.poke(l.surface_control[0], regs::PRIMARY_SURFACE_TMZ), Decline::Protected(0));
    check(&|m| m.poke(l.surface_config[0], 10), Decline::Format(0, 10));
    check(&|m| m.poke(l.surface_config[0], 8 | 1 << 8), Decline::Rotated(0, 0x108));
    check(&|m| m.poke(l.flip_control[0], regs::SURFACE_UPDATE_LOCK), Decline::Locked(0));
    check(&|m| m.poke(l.hubp_cntl[0], regs::HUBP_BLANK_EN), Decline::NoPlane(0));
    check(&|m| m.poke(l.pitch[0], 2048), Decline::NotTheGop(0, (1920, 1080), 2048));
    check(&|m| m.poke(l.viewport_start[0], 8), Decline::Viewport(0));
    check(&|m| m.poke(l.otg_h_blank[0], 0), Decline::Timings(0));

    // Split pipes scanning different surfaces.
    let m = lit(info, &[0, 1]);
    m.poke(l.addr[1], 0x1000);
    assert_eq!(takeover::read(&m, &info, &gop()), Err(Decline::Split(0)));
    // A second display on another surface.
    let m = lit(info, &[0]);
    m.firmware(1, &cea1080(), &[2], FB + (64 << 20), 1920, (1920, 1080));
    assert_eq!(takeover::read(&m, &info, &gop()), Err(Decline::TwoSurfaces));
    // The bootloader's framebuffer is RGBX.
    let m = lit(info, &[0]);
    assert_eq!(takeover::read(&m, &info, &Gop { bgr: false, ..gop() }), Err(Decline::GopFormat));
}

#[test]
fn dce_takeover() {
    for info in [polaris(), platform::lookup(0x687F).unwrap()] {
        let m = lit(info, &[0]);
        let fw = takeover::read(&m, &info, &gop()).expect("declined");
        assert_eq!((fw.heads[0].otg, fw.heads[0].addr, fw.heads[0].timings), (0, FB, cea1080()));
        let l = dce(&info);
        let m = lit(info, &[0]);
        m.poke(l.grph_control[0], regs::GRPH_DEPTH_32BPP | regs::GRPH_ADDRESS_TRANSLATION_ENABLE);
        assert_eq!(takeover::read(&m, &info, &gop()), Err(Decline::Translated(0)));
        let m = lit(info, &[0]);
        m.poke(l.grph_control[0], regs::GRPH_DEPTH_32BPP | 4 << 20);
        assert!(matches!(takeover::read(&m, &info, &gop()), Err(Decline::Tiled(0, _))));
        let m = lit(info, &[0]);
        m.poke(l.grph_swap[0], 0x30);
        assert!(matches!(takeover::read(&m, &info, &gop()), Err(Decline::Format(0, _))));
        assert_eq!(m.writes(), 0);
    }
}

#[test]
fn flips_and_frames() {
    for info in [phoenix(), polaris(), platform::lookup(0x15DD).unwrap()] {
        let m = lit(info, &[0]);
        let f0 = scanout::frame_count(&m, &info, 0);
        let to = FB + (8 << 20);
        scanout::flip(&m, &info, 0, to);
        assert!(scanout::flip_pending(&m, &info, 0));
        assert_eq!(scanout::surface(&m, &info, 0), to);
        assert!(scanout::wait_flip(&m, &info, 0, 100_000), "{}: the flip didn't complete", info.block.name());
        assert_eq!(m.flips(), 1);
        assert!(scanout::frame_count(&m, &info, 0) > f0);
        assert!(scanout::running(&m, &info, 0));
    }
}

#[test]
fn cursor() {
    assert_eq!(scanout::cursor_place(-5, 40, 255), Some((0, 40, 5, 0)));
    assert_eq!(scanout::cursor_place(300, 200, 255), Some((300, 200, 0, 0)));
    assert_eq!(scanout::cursor_place(-64, 0, 255), None);
    for info in [phoenix(), polaris()] {
        let m = lit(info, &[0]);
        let at = FB + (20 << 20);
        assert!(scanout::cursor_setup(&m, &info, 0, at));
        assert!(!scanout::cursor_on(&m, &info, 0));
        scanout::cursor_show(&m, &info, 0, at, -5, 40);
        assert!(scanout::cursor_on(&m, &info, 0));
        assert_eq!(scanout::cursor_regs(&m, &info, 0), ((0, 40), (5, 0)));
        scanout::cursor_show(&m, &info, 0, at, -100, 40);
        assert!(!scanout::cursor_on(&m, &info, 0));
        match info.block.layout() {
            Layout::Dcn(l) => {
                assert_eq!(m.peek(l.cur_size[0]), 64 << 16 | 64);
                assert_eq!(m.peek(l.cur_addr[0]), at as u32);
                assert_eq!(m.peek(l.cur_addr_high[0]), (at >> 32) as u32);
                assert_eq!(m.peek(l.dpp_cur_control[0]) & regs::CUR0_ENABLE, 0);
            }
            Layout::Dce(l) => {
                assert_eq!(m.peek(l.cur_size[0]), 63 << 16 | 63);
                assert_eq!(m.peek(l.cur_update[0]) & regs::CURSOR_UPDATE_LOCK, 0, "left locked");
            }
        }
    }
    assert_eq!(scanout::premultiply(0x80FF_0000), 0x8080_0000);
}

#[test]
fn aux_and_edid() {
    for info in [phoenix(), polaris(), platform::lookup(0x744C).unwrap()] {
        let m = lit(info, &[0]);
        m.attach_sink(2, Sink::new(0x14, 4, test_edid(b"RADEON")));
        let ch = Engine { mmio: &m, info, ch: 2 };
        assert_eq!(Transport::read_byte(&ch, 0), Ok(0x12));
        let mut b = [0u8; 128];
        Transport::read_edid_block(&ch, 0, &mut b).expect("no EDID");
        let e = aurora_edid::parse(&b).unwrap();
        assert_eq!(e.name.as_deref(), Some("RADEON"));
        // The engine is handed back after every transaction.
        let arb = match info.block.layout() {
            Layout::Dcn(l) => l.aux_arb_control[2],
            Layout::Dce(l) => l.aux_arb_control[2],
        };
        assert_eq!(m.peek(arb) & regs::AUX_REG_RW_CNTL_STATUS_MASK, 0);
        // Nothing on channel 0.
        let empty = Engine { mmio: &m, info, ch: 0 };
        assert!(Transport::read_byte(&empty, 0).is_err());
    }
}

#[test]
fn hotplug_sense() {
    let info = phoenix();
    let m = lit(info, &[0]);
    assert_eq!(hpd::sense(&m, &info), 0);
    m.attach_sink(1, Sink::new(0x14, 4, test_edid(b"X")));
    m.plug(4, true);
    assert_eq!(hpd::sense(&m, &info), 0b10010);
    m.detach_sink(1);
    assert_eq!(hpd::sense(&m, &info), 0b10000);
}

#[test]
fn vram_placement() {
    let g = gop();
    assert_eq!(vram::range(&g, 256 << 20), Some((8 << 20, 192 << 20)));
    let mut s = vram::space(&g, 256 << 20).unwrap();
    let a = s.alloc(g.bytes(), vram::ALIGN).unwrap();
    assert!(a >= 8 << 20 && a.is_multiple_of(vram::ALIGN));
    // A huge aperture still keeps to 512 MiB above the framebuffer.
    assert_eq!(vram::range(&g, 16 << 30), Some((8 << 20, (8 << 20) + (512 << 20))));
    // Too small for two screens.
    assert!(vram::space(&g, 24 << 20).is_none());
}
