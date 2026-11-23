//! The Intel driver against a simulated display engine
//! (`aurora_igfx::mock`): the real takeover, GGTT, flip, cursor, dirty,
//! connector and backlight code, with a register file standing in for
//! the GPU's BAR. QEMU has no Intel GPU, so this is how the driver runs
//! in `cargo xtask test`.

use super::*;
use aurora_igfx::mock::{test_edid, Mock, Sink};
use aurora_igfx::regs::Transcoder;
use aurora_igfx::timing::Timings;

/// GGC: an 8 MiB GGTT (4 GiB of space), 64 MiB of stolen memory.
const GGC: u16 = 0x02C0;

fn kaby_lake() -> Info {
    platform::lookup(0x5917).unwrap()
}

/// A Kaby Lake laptop as its GOP leaves it: the panel on pipe A through
/// the eDP transcoder and DDI A, `w`×`h` (CEA 1080p timings at 1920×1080,
/// like the test EDID's, else made up), an HDMI monitor on DDI B's DDC.
fn laptop(w: u32, h: u32) -> Arc<Mock> {
    let info = kaby_lake();
    let m = Arc::new(Mock::new(3));
    let t = if (w, h) == (1920, 1080) {
        Timings::new((1920, 2008, 2052, 2200), (1080, 1084, 1089, 1125), true, true)
    } else {
        Timings::new((w, w + 48, w + 80, w + 160), (h, h + 3, h + 8, h + 30), true, false)
    };
    let stride = (w * 4).next_multiple_of(64);
    m.firmware(&info, Transcoder::Edp, 0, 0, regs::DDI_MODE_DP_SST, &t, 0, stride);
    m.attach_sink(0, Sink::new(0x0A, 4, test_edid(b"PANEL")));
    m.attach_ddc(5, test_edid(b"MONITOR"));
    m
}

fn commit_primary(dev: &Intel<Mock>, fb: &Arc<Framebuffer>, user_data: u64) -> Result<()> {
    let h = &dev.heads[0];
    let mut p = PlaneState::show(h.primary, h.crtc, fb, 0, 0);
    p.hotspot = (0, 0);
    dev.atomic_commit(&Commit {
        planes: alloc::vec![p],
        flags: commit::PAGE_FLIP_EVENT,
        user_data,
        ..Commit::default()
    })
}

pub fn run() {
    let m = laptop(1920, 1080);
    let dev = Intel::new(m.clone(), kaby_lake(), Policy::Takeover, GGC, true).expect("takeover declined");

    // Connectors: the panel (with its EDID over AUX), the monitor over GMBUS.
    let conns = dev.probe();
    let panel = &conns[0];
    assert_eq!((panel.kind, panel.status), (ConnectorKind::Edp, Status::Connected));
    let e = aurora_edid::parse(panel.edid.as_deref().expect("no panel EDID")).unwrap();
    assert_eq!(e.name.as_deref(), Some("PANEL"));
    let hdmi = conns.iter().find(|c| c.kind == ConnectorKind::HdmiA).expect("no HDMI connector");
    assert_eq!(hdmi.status, Status::Connected);
    assert!(hdmi.modes.iter().any(|m| m.size() == (1920, 1080)));
    assert!(dev.caps().cursor.is_some() && !dev.caps().live_modeset);

    // The CRTC runs the firmware's mode; the clock came from the panel's EDID.
    let (cs, _) = dev.state();
    let mode = cs[0].mode.expect("pipe A has no mode");
    assert!(cs[0].active && mode.size() == (1920, 1080));
    assert_eq!((mode.clock, mode.vrefresh), (148_500, 60));
    crate::kprint!("(pipe A {}x{}@{} on {} ", mode.hdisplay, mode.vdisplay, mode.vrefresh, panel.name());

    // The cursor's display buffer share was carved off plane 1's.
    assert_eq!(Ddb::decode(m.peek(regs::cur_buf_cfg(0))), Ddb { start: 852, end: 860 });
    assert_eq!(Ddb::decode(m.peek(regs::plane_buf_cfg(0, 0))), Ddb { start: 0, end: 852 });
    assert!(Wm::decode(m.peek(regs::cur_wm(0, 0))).enabled);

    // A flip to a framebuffer in RAM: mapped into the GGTT, scanned at the next vblank.
    let fb = dev.create_dumb(1920, 1080, fourcc::XRGB8888).unwrap();
    let _ = take_events();
    commit_primary(&dev, &fb, 0x1A7E1).expect("flip failed");
    let surf = m.peek(regs::plane_surf(0, 0));
    assert!(surf as u64 >= 64 << 20 && (surf as u64).is_multiple_of(regs::SURF_ALIGN));
    assert_eq!(m.peek(regs::plane_surflive(0, 0)), surf, "the flip didn't complete");
    assert_eq!(m.peek(regs::plane_stride(0, 0)), 1920 * 4 / 64);
    let ctl = m.peek(regs::plane_ctl(0, 0));
    assert_eq!(ctl & (regs::PLANE_CTL_FORMAT_MASK_SKL | regs::PLANE_CTL_TILED_MASK), regs::PLANE_CTL_FORMAT_8888);
    let first_page = fb.bo.chunks()[0].0;
    assert_eq!(gtt::pte_page(m.gtt_entry(surf as u64 / 4096)), Some(first_page));
    let last = fb.bo.size as u64 / 4096 - 1;
    let (lp, ln) = *fb.bo.chunks().last().unwrap();
    assert_eq!(gtt::pte_page(m.gtt_entry(surf as u64 / 4096 + last)), Some(lp + ln as u64 - 4096));
    let flip = take_events().into_iter().any(|e| matches!(e, Event::Flip { user_data: 0x1A7E1, .. }));
    assert!(flip, "no flip event");
    assert!(dev.panic_framebuffer().is_some_and(|f| f.width == 1920), "the scanout buffer isn't one physical run");

    // Presenting is a cache flush; a second buffer flips, and the first one's
    // GGTT range is released (pointed at the scratch page) when it goes.
    dev.dirty(&fb, &[Damage { x: 10, y: 10, w: 100, h: 20 }]).unwrap();
    let fb2 = dev.create_dumb(1920, 1080, fourcc::XRGB8888).unwrap();
    commit_primary(&dev, &fb2, 2).unwrap();
    let surf2 = m.peek(regs::plane_surf(0, 0));
    assert_ne!(surf2, surf);
    drop(fb);
    assert_eq!(gtt::pte_page(m.gtt_entry(surf as u64 / 4096)), Some(dev.ggtt.scratch));

    // A mode change is refused under the default policy.
    let h = &dev.heads[0];
    let small = dev.create_dumb(1280, 720, fourcc::XRGB8888).unwrap();
    let c = Commit {
        crtcs: alloc::vec![CrtcState { mode: Mode::cvt(1280, 720, 60), ..cs[0].clone() }],
        planes: alloc::vec![PlaneState::show(h.primary, h.crtc, &small, 0, 0)],
        flags: commit::ALLOW_MODESET,
        user_data: 0,
    };
    assert!(dev.atomic_commit(&c).is_err());
    assert_eq!(m.peek(regs::plane_surf(0, 0)), surf2, "a refused commit changed the plane");

    // The cursor: premultiplied into its own buffer, placed by its corner.
    let cur = dev.create_dumb(64, 64, fourcc::ARGB8888).unwrap();
    unsafe { cur.pixels()[0] = 0x80FF_0000 };
    let mut k = PlaneState::show(h.cursor, h.crtc, &cur, -5, 40);
    k.hotspot = (5, 0);
    dev.atomic_commit(&Commit { planes: alloc::vec![k.clone()], ..Commit::default() }).unwrap();
    assert_eq!(m.peek(regs::curcntr(0)), regs::CURSOR_MODE_64_ARGB);
    assert_eq!(plane::cursor_pos_decode(m.peek(regs::curpos(0))), (-5, 40));
    let base = m.peek(regs::curbase(0)) as u64;
    let slot = dev.heads[0].lit.as_ref().unwrap();
    let px = unsafe { *((slot.cursor_bo.vaddr() + base - slot.cursor_gtt) as *const u32) };
    assert_eq!(px, 0x8080_0000);
    k.dst = (300, 200, 64, 64);
    dev.atomic_commit(&Commit { planes: alloc::vec![k], ..Commit::default() }).unwrap();
    assert_eq!(plane::cursor_pos_decode(m.peek(regs::curpos(0))), (300, 200));
    assert_eq!(m.peek(regs::curbase(0)) as u64, base, "a move must not redraw the image");
    dev.atomic_commit(&Commit { planes: alloc::vec![PlaneState::disabled(h.cursor)], ..Commit::default() }).unwrap();
    assert_eq!(m.peek(regs::curcntr(0)), 0);

    // The panel's backlight.
    let b = dev.backlight().expect("no backlight");
    assert_eq!(b.get(&*m), 50);
    b.set(&*m, 80);
    assert_eq!(b.get(&*m), 80);

    // Hotplug: the monitor goes away.
    m.detach_ddc(5);
    assert!(dev.refresh(), "unplugging wasn't noticed");
    let hdmi = dev.connectors().into_iter().find(|c| c.kind == ConnectorKind::HdmiA).unwrap();
    assert_eq!(hdmi.status, Status::Disconnected);
    crate::kprint!("flips, cursor, EDID, hotplug; ");

    // 1366 pixels: rows of 5464 bytes, so the plane scans a padded copy.
    let m = laptop(1366, 768);
    let dev = Intel::new(m.clone(), kaby_lake(), Policy::Takeover, GGC, true).expect("takeover declined");
    let fb = dev.create_dumb(1366, 768, fourcc::XRGB8888).unwrap();
    unsafe { fb.pixels()[1366 * 767 + 1365] = 0x00AB_CDEF };
    commit_primary(&dev, &fb, 3).unwrap();
    assert_eq!(m.peek(regs::plane_stride(0, 0)), 5504 / 64);
    {
        let s = dev.state.lock();
        let scan = s.scan[0].as_ref().unwrap();
        let (va, pitch) = scan.target();
        assert!(scan.shadow.is_some() && pitch == 5504);
        let last = unsafe { *((va + 767 * 5504 + 1365 * 4) as *const u32) };
        assert_eq!(last, 0x00AB_CDEF);
    }
    unsafe { fb.pixels()[0] = 0x0012_3456 };
    dev.dirty(&fb, &[Damage { x: 0, y: 0, w: 1, h: 1 }]).unwrap();
    {
        let s = dev.state.lock();
        let (va, _) = s.scan[0].as_ref().unwrap().target();
        assert_eq!(unsafe { *(va as *const u32) }, 0x0012_3456);
    }
    crate::kprint!("1366-wide padded scanout; ");

    // The firmware lit the panel and an HDMI monitor at the same size: the
    // monitor shows the panel's buffer until a client gives it its own.
    let m = laptop(1920, 1080);
    let t = Timings::new((1920, 2008, 2052, 2200), (1080, 1084, 1089, 1125), true, true);
    m.firmware(&kaby_lake(), Transcoder::Pipe(1), 1, 1, regs::DDI_MODE_HDMI, &t, 0, 7680);
    let dev = Intel::new(m.clone(), kaby_lake(), Policy::Takeover, GGC, true).expect("takeover declined");
    let conns = dev.connectors();
    assert_eq!((conns[0].kind, conns[1].kind), (ConnectorKind::Edp, ConnectorKind::HdmiA));
    let fb = dev.create_dumb(1920, 1080, fourcc::XRGB8888).unwrap();
    commit_primary(&dev, &fb, 4).unwrap();
    assert_eq!(m.peek(regs::plane_surf(1, 0)), m.peek(regs::plane_surf(0, 0)), "pipe B doesn't mirror");
    crate::kprint!("two pipes mirrored; ");

    // What the firmware left must be as expected, else the driver declines.
    let m = laptop(1920, 1080);
    m.poke(regs::plane_ctl(0, 0), m.peek(regs::plane_ctl(0, 0)) | 1 << 10); // X-tiled
    let writes = m.writes();
    assert!(matches!(
        Intel::new(m.clone(), kaby_lake(), Policy::Takeover, GGC, true),
        Err(Declined::Firmware(takeover::Decline::Tiled(0, _)))
    ));
    assert_eq!(m.writes(), writes, "declining wrote to the hardware");
    let m = Arc::new(Mock::new(3));
    assert!(matches!(
        Intel::new(m, kaby_lake(), Policy::Takeover, GGC, true),
        Err(Declined::Firmware(takeover::Decline::NoActivePipe))
    ));
    crate::kprint!("declines tiled and dark) ");
}
