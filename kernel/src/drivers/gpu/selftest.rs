//! Self-tests of the display core (`cargo xtask test`, with any of
//! `--gpu std|virtio|virtio-linux`).

use super::*;
use output::Output;

/// EDID parsing in the kernel, the primary device's connectors, two mode
/// sets through the compositor's path, a present, atomic rules, flip
/// events and the cursor plane.
pub fn display() {
    // The EDID library works in the soft-float kernel: CVT timings, and a
    // real EDID when the device has one.
    let t = aurora_edid::cvt_rb(1920, 1080, 60).expect("CVT");
    assert_eq!((t.clock_khz, t.htotal, t.vtotal), (138_500, 2080, 1111));

    let dev = primary().expect("no display device");
    let conns = dev.probe();
    let conn = conns.iter().find(|c| c.status == Status::Connected).expect("no connected display");
    assert!(!conn.modes.is_empty(), "{} has no modes", conn.name());
    let pref = conn.preferred().unwrap();
    crate::kprint!(
        "({} {} prefers {}x{}, {} modes",
        dev.name(),
        conn.name(),
        pref.hdisplay,
        pref.vdisplay,
        conn.modes.len()
    );
    if let Some(edid) = &conn.edid {
        let e = aurora_edid::parse(edid).expect("the display's EDID does not parse");
        let (w, h) = e.max_size();
        assert!(w >= 640 && h >= 480, "EDID without usable timings");
        crate::kprint!(", EDID {} {:?} up to {}x{}", e.manufacturer_str(), e.name.as_deref().unwrap_or("?"), w, h);
    }
    crate::kprint!(") ");

    let caps = dev.caps();
    // A live device: two sizes. Otherwise the one it has.
    let sizes: alloc::vec::Vec<(u32, u32)> =
        if caps.live_modeset { alloc::vec![(1024, 768), (1280, 800)] } else { alloc::vec![pref.size()] };
    let mut out = Output::open(Some(sizes[0])).expect("could not open the display");
    for &(w, h) in &sizes {
        out.set_size(w, h).unwrap_or_else(|e| panic!("mode set to {w}x{h} failed: {e:?}"));
        assert_eq!(out.size(), (w, h));
        let state = dev.state();
        let crtc = state.0.iter().find(|s| s.crtc == out.crtc).unwrap();
        assert!(crtc.active && crtc.mode.map(|m| m.size()) == Some((w, h)), "CRTC not in {w}x{h}");
        if dev.name() == "bochs-vga" {
            assert_eq!(bochs::Bochs::registers(), (w, h), "the adapter's registers disagree");
        }
        // Draw a pattern and present all of it, then one small rectangle.
        let px = unsafe { out.back_buffer() };
        for (i, p) in px.iter_mut().enumerate() {
            *p = 0xFF00_0000 | (i as u32 % w * 255 / w) << 16 | (i as u32 / w * 255 / h) << 8;
        }
        out.present(&[Damage { x: 0, y: 0, w, h }]).expect("present failed");
        out.present(&[Damage { x: 10, y: 10, w: 64, h: 32 }]).expect("partial present failed");
    }
    if caps.live_modeset {
        // A size the device can't show is refused and changes nothing.
        assert!(out.set_size(100_000, 100).is_err());
        assert_eq!(out.size(), sizes[sizes.len() - 1]);
    } else {
        assert!(out.set_size(pref.hdisplay + 16, pref.vdisplay).is_err());
    }

    // Atomic rules: a primary plane that doesn't cover the CRTC is refused.
    let (w, h) = out.size();
    let small = dev.create_dumb(w / 2, h / 2, fourcc::XRGB8888).unwrap();
    let primary_plane = dev.planes().into_iter().find(|p| p.kind == PlaneKind::Primary).unwrap().id;
    let bad = Commit {
        planes: alloc::vec![PlaneState::show(primary_plane, out.crtc, &small, 0, 0)],
        flags: commit::TEST_ONLY,
        ..Commit::default()
    };
    assert_eq!(dev.atomic_check(&bad), Err(Error::Invalid));
    // A page flip to a new framebuffer, with a completion event.
    let next = dev.create_dumb(w, h, fourcc::XRGB8888).unwrap();
    take_events();
    let before = dev.vblank(out.crtc).seq;
    let flip = Commit {
        planes: alloc::vec![PlaneState::show(primary_plane, out.crtc, &next, 0, 0)],
        flags: commit::PAGE_FLIP_EVENT,
        user_data: 0xF11F,
        ..Commit::default()
    };
    dev.atomic_commit(&flip).expect("page flip failed");
    let flipped =
        take_events().iter().any(|e| matches!(e, Event::Flip { crtc, user_data: 0xF11F, .. } if *crtc == out.crtc));
    assert!(flipped, "no flip event");
    assert!(dev.vblank(out.crtc).seq > before, "the frame counter did not advance");
    // Back to the output's own buffer.
    out.set_size(w, h).unwrap();

    // The cursor plane: an image, a move, hidden, shown again.
    if out.has_cursor() {
        let max = out.cursor_size().0;
        let img = crate::gui::cursor::image(aurora_abi::cursor::ARROW, 0, 1, max).expect("arrow too big");
        assert!(img.pixels.iter().any(|&p| p >> 24 == 0xFF), "the cursor image is empty");
        // A shape too large for the plane is refused (the compositor draws it).
        assert!(out.set_cursor(&alloc::vec![0; 4 * max as usize * max as usize], 2 * max, 2 * max, (0, 0)).is_err());
        out.set_cursor(&img.pixels, img.width, img.height, img.hot).expect("cursor image");
        out.move_cursor(w as i32 / 2, h as i32 / 2).expect("cursor move");
        out.hide_cursor().expect("cursor hide");
        out.show_cursor().expect("cursor show");
        let (_, planes) = dev.state();
        let cursor = planes.iter().find(|p| p.fb.as_ref().is_some_and(|f| f.format == fourcc::ARGB8888));
        let cursor = cursor.expect("no cursor plane in the committed state");
        assert_eq!((cursor.dst.0 + cursor.hotspot.0, cursor.dst.1 + cursor.hotspot.1), (w as i32 / 2, h as i32 / 2));
        crate::kprint!("(cursor plane) ");
    }
    // Leave the screen showing something sensible for the following tests.
    let px = unsafe { out.back_buffer() };
    px.fill(0xFF10_2040);
    out.present(&[Damage { x: 0, y: 0, w, h }]).unwrap();
}

/// After sleep: the primary display shows its framebuffer again and takes presents.
pub fn after_sleep() {
    let Some(dev) = primary() else { return };
    let (cs, ps) = dev.state();
    let Some(crtc) = cs.iter().find(|s| s.active) else { return };
    let crtcs = dev.crtcs();
    let index = crtcs.iter().find(|c| c.id == crtc.crtc).unwrap().index;
    let planes = dev.planes();
    let primary_fb = ps
        .iter()
        .filter(|p| {
            planes
                .iter()
                .any(|i| i.id == p.plane && i.kind == PlaneKind::Primary && i.possible_crtcs & (1 << index) != 0)
        })
        .find_map(|p| p.fb.clone());
    if let Some(fb) = primary_fb {
        dev.dirty(&fb, &[Damage { x: 0, y: 0, w: fb.width, h: fb.height }]).expect("no present after sleep");
        crate::kprint!("[display back on {}] ", dev.name());
    }
}
