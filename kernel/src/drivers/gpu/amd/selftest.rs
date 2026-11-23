//! The AMD driver against a simulated display engine
//! (`aurora_amdgfx::mock`): the real takeover, VRAM placement, flip,
//! dirty, cursor, connector and hot-plug code, with a register file
//! standing in for the register BAR and a buffer in RAM for the VRAM
//! aperture. QEMU has no AMD GPU, so this is how the driver runs in
//! `cargo xtask test`.

use super::*;
use aurora_amdgfx::mock::{test_edid, Mock, Sink};
use aurora_amdgfx::platform::Layout;
use aurora_igfx::timing::Timings;

/// The GPU address of aperture offset 0.
const FB: u64 = 0xF4_0000_0000;
/// The aperture the mock pretends to have; the driver uses 4–15 MiB of it.
const BAR: u64 = 20 << 20;

struct TestHw {
    mock: Mock,
    vram: Arc<BufferObject>,
}

impl Mmio for TestHw {
    fn read(&self, reg: u32) -> u32 {
        self.mock.read(reg)
    }
    fn write(&self, reg: u32, value: u32) {
        self.mock.write(reg, value)
    }
    fn delay_us(&self, us: u32) {
        self.mock.delay_us(us)
    }
}

impl Aperture for TestHw {
    fn vram_va(&self) -> u64 {
        self.vram.vaddr()
    }
    fn vram_phys(&self) -> u64 {
        self.vram.chunks()[0].0
    }
    fn vram_mapped(&self) -> u64 {
        self.vram.size as u64
    }
}

fn hd720() -> Timings {
    Timings::new((1280, 1390, 1430, 1650), (720, 725, 730, 750), true, true)
}

fn gop() -> Gop {
    Gop { offset: 0, width: 1280, height: 720, stride: 1280, bgr: true }
}

/// A Phoenix laptop as its GOP leaves it: OTG 0 on HUBP 0, 720p, a DP
/// panel on AUX channel / HPD pin 0.
fn laptop(info: Info) -> Arc<TestHw> {
    let mock = Mock::new(info);
    mock.firmware(0, &hd720(), &[0], FB, 1280, (1280, 720));
    mock.attach_sink(0, Sink::new(0x14, 4, test_edid(b"PANEL")));
    let vram = BufferObject::alloc_system(vram::range(&gop(), BAR).unwrap().1 as usize)
        .expect("no memory for the test aperture");
    Arc::new(TestHw { mock, vram })
}

fn vram_px(dev: &Amd<TestHw>, at: u64, x: u32, y: u32) -> u32 {
    unsafe { *((dev.hw.vram_va() + at + y as u64 * 1280 * 4 + x as u64 * 4) as *const u32) }
}

fn commit_primary(dev: &Amd<TestHw>, fb: &Arc<Framebuffer>, user_data: u64) -> Result<()> {
    let h = &dev.heads[0];
    dev.atomic_commit(&Commit {
        planes: alloc::vec![PlaneState::show(h.primary, h.crtc, fb, 0, 0)],
        flags: commit::PAGE_FLIP_EVENT,
        user_data,
        ..Commit::default()
    })
}

pub fn run() {
    let info = platform::lookup(0x15BF).unwrap();
    let hw = laptop(info);
    let dev = Amd::new(hw.clone(), info, gop(), BAR, true).expect("takeover declined");

    // The panel: its EDID over AUX, the firmware's mode (the clock measured
    // from the frame counter, 60 Hz in simulated time).
    let conns = dev.probe();
    assert_eq!((conns[0].kind, conns[0].status), (ConnectorKind::DisplayPort, Status::Connected));
    let e = aurora_edid::parse(conns[0].edid.as_deref().expect("no panel EDID")).unwrap();
    assert_eq!(e.name.as_deref(), Some("PANEL"));
    let (cs, _) = dev.state();
    let mode = cs[0].mode.unwrap();
    assert_eq!((mode.size(), mode.vrefresh), ((1280, 720), 60));
    assert!(dev.caps().cursor.is_some() && !dev.caps().live_modeset);
    crate::kprint!("(OTG 0 {}x{}@{} on {} ", mode.hdisplay, mode.vdisplay, mode.vrefresh, conns[0].name());

    // A flip: the picture is copied into VRAM above the firmware's
    // framebuffer and the surface address moves there at the next vblank.
    let Layout::Dcn(l) = info.block.layout() else { panic!() };
    let fb = dev.create_dumb(1280, 720, fourcc::XRGB8888).unwrap();
    unsafe { fb.pixels()[1280 * 719 + 1279] = 0x00AB_CDEF };
    let _ = take_events();
    commit_primary(&dev, &fb, 0xA3D1).expect("flip failed");
    let surf = (hw.mock.peek(l.addr_high[0]) as u64) << 32 | hw.mock.peek(l.addr[0]) as u64;
    let at = surf - FB;
    assert!(at >= 1 << 20 && at.is_multiple_of(vram::ALIGN), "scanout at {at:#x}");
    assert!(!scanout::flip_pending(&hw.mock, &info, 0), "the flip didn't complete");
    assert_eq!(vram_px(&dev, at, 1279, 719), 0x00AB_CDEF);
    assert!(take_events().iter().any(|e| matches!(e, Event::Flip { user_data: 0xA3D1, .. })), "no flip event");
    assert_eq!(dev.panic_framebuffer().map(|f| f.phys_addr), Some(hw.vram_phys() + at));

    // Presenting copies the damage into the buffer on screen.
    unsafe { fb.pixels()[0] = 0x0012_3456 };
    dev.dirty(&fb, &[Damage { x: 0, y: 0, w: 1, h: 1 }]).unwrap();
    assert_eq!(vram_px(&dev, at, 0, 0), 0x0012_3456);
    // A second framebuffer goes to the other buffer.
    let fb2 = dev.create_dumb(1280, 720, fourcc::XRGB8888).unwrap();
    commit_primary(&dev, &fb2, 2).unwrap();
    let surf2 = hw.mock.peek(l.addr[0]) as u64 | (hw.mock.peek(l.addr_high[0]) as u64) << 32;
    assert_ne!(surf2, surf);
    commit_primary(&dev, &fb, 3).unwrap();
    assert_eq!(hw.mock.peek(l.addr[0]) as u64 | (hw.mock.peek(l.addr_high[0]) as u64) << 32, surf);

    // Mode changes and other sizes are refused.
    let h = &dev.heads[0];
    let small = dev.create_dumb(800, 600, fourcc::XRGB8888).unwrap();
    assert!(commit_primary(&dev, &small, 4).is_err());
    let c = Commit {
        crtcs: alloc::vec![CrtcState { mode: Mode::cvt(1024, 768, 60), ..cs[0].clone() }],
        flags: commit::ALLOW_MODESET,
        ..Commit::default()
    };
    assert!(dev.atomic_commit(&c).is_err());
    crate::kprint!("flips into VRAM, dirty, events; ");

    // The cursor: premultiplied into VRAM, placed by its corner.
    let cur = dev.create_dumb(64, 64, fourcc::ARGB8888).unwrap();
    unsafe { cur.pixels()[0] = 0x80FF_0000 };
    let k = PlaneState::show(h.cursor, h.crtc, &cur, -5, 40);
    dev.atomic_commit(&Commit { planes: alloc::vec![k.clone()], ..Commit::default() }).unwrap();
    assert!(scanout::cursor_on(&hw.mock, &info, 0));
    assert_eq!(scanout::cursor_regs(&hw.mock, &info, 0), ((0, 40), (5, 0)));
    let cur_at = (hw.mock.peek(l.cur_addr_high[0]) as u64) << 32 | hw.mock.peek(l.cur_addr[0]) as u64;
    let px = unsafe { *((hw.vram_va() + cur_at - FB) as *const u32) };
    assert_eq!(px, 0x8080_0000);
    let mut k2 = k.clone();
    k2.dst = (300, 200, 64, 64);
    dev.atomic_commit(&Commit { planes: alloc::vec![k2], ..Commit::default() }).unwrap();
    assert_eq!(scanout::cursor_regs(&hw.mock, &info, 0).0, (300, 200));
    let moved = (hw.mock.peek(l.cur_addr_high[0]) as u64) << 32 | hw.mock.peek(l.cur_addr[0]) as u64;
    assert_eq!(moved, cur_at, "a move must not redraw the image");
    dev.atomic_commit(&Commit { planes: alloc::vec![PlaneState::disabled(h.cursor)], ..Commit::default() }).unwrap();
    assert!(!scanout::cursor_on(&hw.mock, &info, 0));
    crate::kprint!("cursor; ");

    // Hot-plug: a monitor on HPD pin 3 (DP, with EDID), then unplugged.
    hw.mock.attach_sink(3, Sink::new(0x14, 4, test_edid(b"MONITOR")));
    assert!(dev.refresh(), "plugging in wasn't noticed");
    let mon = dev.connectors().into_iter().find(|c| c.id != dev.heads[0].connector).expect("no new connector");
    assert_eq!((mon.kind, mon.status), (ConnectorKind::DisplayPort, Status::Connected));
    assert!(mon.modes.iter().any(|m| m.size() == (1920, 1080)));
    hw.mock.detach_sink(3);
    assert!(dev.refresh(), "unplugging wasn't noticed");
    let mon = dev.connectors().into_iter().find(|c| c.id == mon.id).unwrap();
    assert_eq!(mon.status, Status::Disconnected);
    crate::kprint!("hot-plug; ");

    // A Polaris card (DCE 11.2): takeover and a flip.
    let polaris = platform::lookup(0x67DF).unwrap();
    let hw = laptop(polaris);
    let dev = Amd::new(hw.clone(), polaris, gop(), BAR, true).expect("Polaris takeover declined");
    let fb = dev.create_dumb(1280, 720, fourcc::XRGB8888).unwrap();
    commit_primary(&dev, &fb, 5).unwrap();
    assert_ne!(scanout::surface(&hw.mock, &polaris, 0), FB);
    assert!(!scanout::flip_pending(&hw.mock, &polaris, 0));
    crate::kprint!("DCE 11.2; ");

    // Anything unexpected declines without a single write.
    let hw = laptop(info);
    hw.mock.poke(l.tiling_config[0], 9);
    let writes = hw.mock.writes();
    assert!(matches!(
        Amd::new(hw.clone(), info, gop(), BAR, true),
        Err(Declined::Firmware(takeover::Decline::Tiled(0, 9)))
    ));
    assert_eq!(hw.mock.writes(), writes, "declining wrote to the hardware");
    let hw = laptop(info);
    let other = Gop { stride: 1536, ..gop() };
    assert!(matches!(Amd::new(hw.clone(), info, other, BAR, true), Err(Declined::Firmware(_))));
    assert_eq!(hw.mock.writes(), 0);
    crate::kprint!("declines) ");
}
