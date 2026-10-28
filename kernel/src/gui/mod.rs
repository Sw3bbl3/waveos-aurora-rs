//! Lumen Server — the WaveOS Aurora compositor and desktop.
//!
//! Runs as a kernel task. Each frame it drains the input queue, lets the
//! desktop update, repaints only damaged regions into a RAM back buffer and
//! copies those regions to the GOP framebuffer.

pub mod account;
pub mod apps;
pub mod boot;
pub mod clipboard;
pub mod cursor;
pub mod desktop;
pub mod dnd;
pub mod installed;
pub mod notify;
pub mod prefs;
pub mod server;
pub mod settings;

pub use lumen::{canvas, geom, icons, theme, wallpaper, widgets};

use crate::drivers::input;
use crate::mm::phys_to_virt;
use crate::{sched, time};
use alloc::vec;
use bootinfo::{Framebuffer, PixelFormat};
use canvas::{rgb, Canvas};
use core::fmt::Write;
use desktop::{Desktop, PowerAction};
use geom::Rect;

/// The framebuffer in use (replaced when the resolution changes).
static FB: spin::Mutex<Option<Framebuffer>> = spin::Mutex::new(None);

pub fn init(fb: Framebuffer, dark: bool) {
    *FB.lock() = Some(fb);
    load_fonts();
    boot::show(fb, dark);
}

fn current_fb() -> Option<Framebuffer> {
    *FB.lock()
}

/// Installs the TrueType UI faces from the system image (text falls back to
/// a few built-in sizes without them).
fn load_fonts() {
    use lumen::font::{self, Face};
    lumen::icons::set_loader(load_icon);
    for face in [Face::Regular, Face::SemiBold, Face::Bold, Face::Mono, Face::Light] {
        let ok = crate::fs::read_all(face.file())
            .is_ok_and(|data| font::install(face, alloc::boxed::Box::leak(data.into_boxed_slice())));
        if !ok {
            log!("gui", "font {} unavailable; using built-in sizes", face.file());
        }
    }
}

/// Decodes an app icon picture from the system image for [`lumen::icons`].
fn load_icon(path: &str) -> Option<(alloc::vec::Vec<u32>, u32, u32)> {
    let data = crate::fs::read_all(path).ok()?;
    let img = aurora_image::decode(&data).ok()?;
    Some((img.pixels, img.width, img.height))
}

/// The screen's size in points (what windows and the pointer use).
pub fn screen_size() -> (i32, i32) {
    let (w, h) = screen_pixels();
    (w / scale(), h / scale())
}

/// The screen's size in device pixels.
pub fn screen_pixels() -> (i32, i32) {
    current_fb().map(|f| (f.width as i32, f.height as i32)).unwrap_or((1024, 768))
}

/// Device pixels per point: 2 on a HiDPI screen.
static SCALE: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(1);

pub fn scale() -> i32 {
    SCALE.load(core::sync::atomic::Ordering::Relaxed)
}

/// The scale for a `w`×`h` screen: Settings' choice (`pref::DISPLAY_SCALE`
/// 1 or 2, when 2× still leaves 1024×640 points), or automatically 2× when
/// the screen is at least 2560 pixels wide and 1440 tall.
fn scale_for(w: i32, h: i32) -> i32 {
    let fits = w / 2 >= 1024 && h / 2 >= 640;
    match settings::get(aurora_abi::pref::DISPLAY_SCALE).as_deref() {
        Some("1") => 1,
        Some("2") if fits => 2,
        _ if w >= 2560 && h >= 1440 => 2,
        _ => 1,
    }
}

pub fn start() {
    // A ported GPU driver took over the screen in a mode of its own.
    if let Some(fb) = crate::drivers::display::drm_framebuffer() {
        *FB.lock() = Some(fb);
        input::set_screen_size(fb.width as i32, fb.height as i32);
    }
    // The compositor keeps its own SIMD registers, so drawing can use SSE2.
    lumen::simd::set_gate(simd_allowed);
    sched::spawn_simd("lumen-server", run);
}

/// SSE2 drawing is safe only in a kernel task that owns saved SIMD state,
/// and never inside an interrupt handler (which runs with interrupts off).
fn simd_allowed() -> bool {
    crate::arch::percpu::this().simd.load(core::sync::atomic::Ordering::Relaxed)
        && x86_64::instructions::interrupts::are_enabled()
}

/// A network interface got (or lost) its address.
/// A ported GPU driver reported new display modes (see `crate::linux`).
pub fn display_changed() {
    server::command(server::Command::DisplayChanged);
}

pub fn network_changed() {
    server::command(server::Command::NetworkChanged);
}

/// The ACPI power button was pressed: ask what to do, like the menu's Shut Down….
pub fn power_button() {
    server::command(server::Command::PowerButton);
}

/// Battery or power adapter state changed (from ACPI): update the menu bar,
/// and warn once each time the charge falls to 10 % and 5 %.
pub fn power_changed(old: Option<aurora_abi::PowerInfo>, new: aurora_abi::PowerInfo) {
    use aurora_abi::power::*;
    server::command(server::Command::PowerChanged);
    let discharging = new.flags & DISCHARGING != 0;
    let before = old.filter(|o| o.flags & BATTERY != 0).map_or(101, |o| o.percent);
    for level in [10, 5] {
        if discharging && new.percent <= level && before > level {
            let body = alloc::format!("{}% remaining. Connect your computer to power soon.", new.percent);
            notify::system("Low Battery", &body);
            crate::drivers::audio::play_sound("error");
            break;
        }
    }
    if let Some(o) = old {
        if o.flags & AC_ONLINE != new.flags & AC_ONLINE && new.flags & AC_PRESENT != 0 {
            log!("power", "power adapter {}", if new.flags & AC_ONLINE != 0 { "connected" } else { "disconnected" });
        }
        if o.flags & LID_OPEN != new.flags & LID_OPEN && new.flags & LID_PRESENT != 0 {
            log!("power", "lid {}", if new.flags & LID_OPEN != 0 { "opened" } else { "closed" });
            // Closing the lid puts the computer to sleep, as on any laptop.
            if new.flags & LID_OPEN == 0 && crate::power::s3::available() {
                crate::power::sleep();
            }
        }
    }
}

/// The framebuffer, mapped write-combining when the CPU allows (so copies
/// to it go out in bursts) and through the physical window otherwise.
///
/// When a ported GPU driver runs the display (`linux::display`), its
/// framebuffer — ordinary memory the driver copies to the GPU on flush —
/// takes the firmware framebuffer's place.
fn front_buffer(fb: &Framebuffer) -> &'static mut [u32] {
    #[cfg(linuxkpi)]
    if let Some(d) = crate::linux::display() {
        return unsafe { core::slice::from_raw_parts_mut(d.pixels, (d.pitch / 4 * d.height) as usize) };
    }
    // (physical address, length in pixels, virtual address) of the last mapping.
    static MAPPED: spin::Mutex<(u64, usize, u64)> = spin::Mutex::new((0, 0, 0));
    let len = (fb.stride * fb.height) as usize;
    let mut mapped = MAPPED.lock();
    if mapped.0 != fb.phys_addr || mapped.1 < len {
        let virt = match crate::mm::vmm::kmap_wc(fb.phys_addr, len as u64 * 4) {
            Some(v) => {
                log!("gui", "framebuffer mapped write-combining at {:#x}", v);
                v
            }
            None => phys_to_virt(fb.phys_addr),
        };
        *mapped = (fb.phys_addr, len, virt);
    }
    unsafe { core::slice::from_raw_parts_mut(mapped.2 as *mut u32, len) }
}

/// Copies `r` from the back buffer to the framebuffer.
fn flush(front: &mut [u32], back: &[u32], fb: &Framebuffer, r: Rect) {
    #[cfg(linuxkpi)]
    if let Some(d) = crate::linux::display() {
        // XRGB8888 in memory is the back buffer's own layout.
        let r = r.intersect(&Rect::new(0, 0, fb.width.min(d.width) as i32, fb.height.min(d.height) as i32));
        let (w, stride) = (fb.width as usize, (d.pitch / 4) as usize);
        for y in r.y as usize..r.bottom() as usize {
            front[y * stride + r.x as usize..y * stride + r.right() as usize]
                .copy_from_slice(&back[y * w + r.x as usize..y * w + r.right() as usize]);
        }
        if r.w > 0 && r.h > 0 {
            crate::linux::display_flush(r.x, r.y, r.w, r.h);
        }
        return;
    }
    let w = fb.width as usize;
    let stride = fb.stride as usize;
    for y in r.y as usize..r.bottom() as usize {
        let src = &back[y * w + r.x as usize..y * w + r.right() as usize];
        let dst = &mut front[y * stride + r.x as usize..y * stride + r.right() as usize];
        match fb.format {
            PixelFormat::Bgr => dst.copy_from_slice(src),
            PixelFormat::Rgb => {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = (s & 0x00FF00) | (s >> 16 & 0xFF) | (s & 0xFF) << 16;
                }
            }
        }
    }
    // Drain the write-combining buffers so the frame reaches the screen now.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}

fn run() {
    let mut fb = current_fb().expect("gui::init not called");
    input::set_consumer(sched::current_id());
    server::set_compositor(sched::current_id());

    crate::telemetry::stage("gui", "Lumen Server window server");
    load_settings();
    prefs::apply_appearance();
    // The desktop works in points; the back buffer is in device pixels.
    let mut s = scale_for(fb.width as i32, fb.height as i32);
    SCALE.store(s, core::sync::atomic::Ordering::Relaxed);
    let (mut w, mut h) = (fb.width as i32 / s, fb.height as i32 / s);
    input::set_screen_size(w, h);
    let t0 = time::uptime_ms();
    let mut desktop = Desktop::new(w, h);
    log!("gui", "desktop composed in {} ms ({}x{})", time::uptime_ms() - t0, w, h);

    // A ported GPU driver starts in the screen's preferred mode; switch to
    // the resolution chosen in Settings, if the screen has it.
    if crate::drivers::display::drm() {
        if let Some((bw, bh)) = prefs::boot_resolution().filter(|&r| r != (fb.width, fb.height)) {
            desktop.resolution_request = Some((bw, bh));
        }
    }
    let mut back = vec![0u32; (fb.width * fb.height) as usize];
    let mut materials = lumen::material::MaterialCache::default();
    let mut front = front_buffer(&fb);
    let mut ready = false;
    let mut next_tick = 0;
    // Full-screen frames (count, total ns, max ns) and partial updates (count,
    // total ns), logged every 10 seconds.
    let mut frame_stats = (0u64, 0u64, 0u64, time::uptime_ms());
    let mut partial_stats = (0u64, 0u64);
    loop {
        let now = time::uptime_ms();
        desktop.set_now(now);
        while let Some(ev) = input::pop() {
            desktop.handle(ev, now);
        }
        desktop.process_commands();
        // A new scale at the same resolution: lay out and draw everything again.
        if core::mem::take(&mut desktop.rescale_request) {
            let ns = scale_for(fb.width as i32, fb.height as i32);
            if ns != s {
                s = ns;
                SCALE.store(s, core::sync::atomic::Ordering::Relaxed);
                (w, h) = (fb.width as i32 / s, fb.height as i32 / s);
                materials.clear();
                input::set_screen_size(w, h);
                desktop.resize(w, h);
                server::broadcast(aurora_abi::Event {
                    kind: aurora_abi::event::SCREEN,
                    x: w,
                    y: h,
                    ..Default::default()
                });
                log!("gui", "drawing at {}x ({}x{} points)", s, w, h);
            }
        }
        // A new resolution: new framebuffer, back buffer and layout.
        if let Some((nw, nh)) = desktop.resolution_request.take() {
            match crate::drivers::display::set_mode(nw, nh) {
                // Following the screen: it already has this size.
                Some(new_fb) if (nw, nh) == (0, 0) && (new_fb.width, new_fb.height) == (fb.width, fb.height) => {
                    front = front_buffer(&new_fb);
                    fb = new_fb;
                    *FB.lock() = Some(fb);
                    desktop.damage_all();
                }
                Some(new_fb) => {
                    fb = new_fb;
                    *FB.lock() = Some(fb);
                    let before = s;
                    s = scale_for(fb.width as i32, fb.height as i32);
                    SCALE.store(s, core::sync::atomic::Ordering::Relaxed);
                    (w, h) = (fb.width as i32 / s, fb.height as i32 / s);
                    back = vec![0u32; (fb.width * fb.height) as usize];
                    materials.clear();
                    front = front_buffer(&fb);
                    input::set_screen_size(w, h);
                    desktop.resize(w, h);
                    if s != before {
                        log!("gui", "drawing at {}x ({}x{} points)", s, w, h);
                    }
                    server::broadcast(aurora_abi::Event {
                        kind: aurora_abi::event::SCREEN,
                        x: w,
                        y: h,
                        ..Default::default()
                    });
                }
                None if (nw, nh) == (0, 0) => {}
                None => log!("gui", "the display can't show {}x{}", nw, nh),
            }
        }
        // Input can start an animation, change focus, or unhover an expired
        // banner. Recalculate before waiting so new work isn't delayed.
        next_tick = next_tick.min(desktop.next_wakeup());
        if now >= next_tick {
            next_tick = now + desktop.tick(now);
        }
        let scene_changed = desktop.scene_changed();
        let damage = desktop.take_damage();
        if !damage.is_empty() {
            crate::telemetry::FRAMES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        let batch_start = time::uptime_ms();
        let batch_pixels: i64 = damage.iter().map(|r| r.area()).sum();
        let batch_full = damage.iter().any(|r| r.area() == i64::from(w) * i64::from(h));
        for r in damage {
            let started = time::now_ns();
            crate::telemetry::PIXELS.fetch_add(r.area() as u64, core::sync::atomic::Ordering::Relaxed);
            let mut cv = Canvas::new(&mut back, fb.width as i32, fb.height as i32).with_scale(s);
            materials.begin_frame();
            cv.materials = Some(&mut materials);
            cv.clip = r;
            desktop.paint(&mut cv);
            let px = cv.to_px(r);
            flush(front, &back, &fb, px);
            let elapsed = time::now_ns() - started;
            if r.area() == i64::from(w) * i64::from(h) {
                frame_stats.0 += 1;
                frame_stats.1 += elapsed;
                frame_stats.2 = frame_stats.2.max(elapsed);
            } else if scene_changed {
                partial_stats.0 += 1;
                partial_stats.1 += elapsed;
            }
        }
        desktop.finish_frame();
        if batch_pixels > 0 {
            log!(
                "frame",
                "{}, {}, {}, {}x{}",
                if batch_full {
                    "full"
                } else if scene_changed {
                    "partial"
                } else {
                    "cursor"
                },
                batch_pixels,
                time::uptime_ms() - batch_start,
                w,
                h
            );
        }
        if (frame_stats.0 > 0 || partial_stats.0 > 0) && time::uptime_ms() - frame_stats.3 >= 10000 {
            log!(
                "lumen",
                "{} scene frames, mean {} us, max {} us; {} partial updates, mean {} us; at {}x{}",
                frame_stats.0,
                frame_stats.1 / frame_stats.0.max(1) / 1000,
                frame_stats.2 / 1000,
                partial_stats.0,
                partial_stats.1 / partial_stats.0.max(1) / 1000,
                w,
                h
            );
            frame_stats = (0, 0, 0, time::uptime_ms());
            partial_stats = (0, 0);
        }
        if core::mem::take(&mut desktop.screenshot) {
            take_screenshot(&back, w as u32, h as u32);
        }
        if !ready {
            ready = true;
            boot::finish();
            log!("boot", "Aurora desktop ready");
            crate::drivers::audio::play_sound("startup");
            // Written a moment later, off the compositor, so it includes late devices.
            sched::spawn("bootlog", || {
                sched::sleep_ms(3000);
                crate::report::save_boot_log();
            });
            crate::telemetry::stage("desktop", "Desktop ready");
        }
        if let Some(action) = desktop.power {
            crate::fs::sync_all();
            sched::sleep_ms(800);
            match action {
                PowerAction::Shutdown => crate::power::shutdown(),
                PowerAction::Reboot => crate::power::reboot(),
            }
        }
        let wait = next_tick.saturating_sub(time::uptime_ms()).max(1);
        sched::wait_until(wait, || input::pending() || server::pending() || notify::pending());
    }
}

static SCREENSHOT: crate::sync::IrqMutex<Option<(alloc::vec::Vec<u32>, u32, u32)>> = crate::sync::IrqMutex::new(None);

/// Saves the screen to `/Pictures` as PNG (encoded on a background task).
fn take_screenshot(back: &[u32], w: u32, h: u32) {
    let mut slot = SCREENSHOT.lock();
    if slot.is_some() {
        return; // one at a time
    }
    *slot = Some((back.to_vec(), w, h));
    drop(slot);
    crate::drivers::audio::play_sound("screenshot");
    sched::spawn("screenshot", || {
        let Some((pixels, w, h)) = SCREENSHOT.lock().take() else { return };
        let t0 = time::uptime_ms();
        let png = aurora_image::png::encode(&aurora_image::Image { width: w, height: h, pixels });
        let d = crate::drivers::rtc::now();
        let path = alloc::format!(
            "/Pictures/Screenshot {}-{:02}-{:02} at {:02}.{:02}.{:02}.png",
            d.year,
            d.month,
            d.day,
            d.hour,
            d.minute,
            d.second
        );
        let _ = crate::fs::mkdir("/Pictures");
        match crate::fs::write_all(&path, &png) {
            Ok(()) => {
                log!("gui", "saved {} ({} KiB, {} ms)", path, png.len() / 1024, time::uptime_ms() - t0);
                notify::system("Screenshot saved", path.trim_start_matches("/Pictures/"));
            }
            Err(e) => log!("gui", "could not save a screenshot: {}", aurora_abi::err::name(e)),
        }
    });
}

/// Restores appearance and system preferences.
fn load_settings() {
    settings::load();
    theme::set_dark(settings::get_bool("dark", true));
    theme::set_accent(settings::get_int("accent", 0) as u8);
    prefs::apply_all_system();
    let wp = settings::get_int("wallpaper", 0) as u8;
    let valid = (wp as usize) < wallpaper::NAMES.len() || wp == wallpaper::CUSTOM;
    theme::set_wallpaper(if valid { wp } else { 0 });
    log!("gui", "restored settings (dark: {}, wallpaper: {})", theme::current().dark, theme::wallpaper());
}

/// Persists appearance settings to the home volume.
pub fn save_settings() {
    let dark = theme::current().dark;
    settings::set_bool("dark", dark);
    settings::set_int("wallpaper", theme::wallpaper() as i64);
    // The boot screen follows the appearance from the next start.
    if prefs::boot_option("appearance").as_deref() != Some(if dark { "dark" } else { "light" }) {
        if let Err(e) = prefs::set_boot_option("appearance", if dark { "dark" } else { "light" }) {
            log!("gui", "could not save the boot appearance: {}", aurora_abi::err::name(e));
        }
    }
}

/// Fixed-size text buffer for formatting without the heap.
pub struct StackText<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackText<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N], len: 0 }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> Write for StackText<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len < N {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Paints the "Aurora ran into a problem" screen straight onto the
/// framebuffer. Uses no heap, so it works even if the allocator is wedged.
pub fn panic_screen(details: &str) {
    // Never block here: the panic may have hit while the lock was held.
    let Some(fb) = FB.try_lock().and_then(|g| *g) else { return };
    // A ported GPU driver's screen has no physical framebuffer to write to.
    if fb.phys_addr == 0 {
        return;
    }
    // Through the physical window: mapping anew could allocate or wait on a lock.
    let front = unsafe {
        core::slice::from_raw_parts_mut(phys_to_virt(fb.phys_addr) as *mut u32, (fb.stride * fb.height) as usize)
    };
    let mut cv = Canvas::new(front, fb.stride as i32, fb.height as i32);
    cv.clip = Rect::new(0, 0, fb.width as i32, fb.height as i32);
    let (w, h) = (fb.width as i32, fb.height as i32);
    let top = rgb(0x1E, 0x14, 0x4A);
    let bottom = rgb(0x0B, 0x2A, 0x3F);
    for y in 0..h {
        cv.fill_rect(Rect::new(0, y, w, 1), canvas::mix(top, bottom, y * 256 / h));
    }
    let x = w / 8;
    let mut y = h / 4;
    cv.text(x, y, ":(", theme::ui_bold(40), 0xFFFF_FFFF);
    y += 64;
    cv.text(x, y, "Aurora ran into a problem and had to stop.", theme::ui_bold(20), 0xFFFF_FFFF);
    y += 34;
    cv.text(
        x,
        y,
        "The details below were also written to the serial port. Restart your computer to continue.",
        theme::ui(14),
        0xCCFF_FFFF,
    );
    y += 40;
    let f = theme::mono(13);
    let max_chars = ((w - 2 * x) / f.advance('M').max(1)).max(20) as usize;
    for line in details.lines() {
        let mut rest = line;
        while !rest.is_empty() && y < h - 40 {
            let cut = rest.char_indices().nth(max_chars).map(|(i, _)| i).unwrap_or(rest.len());
            cv.text(x, y, &rest[..cut], f, 0xFFB8_F4FF);
            rest = &rest[cut..];
            y += 20;
        }
    }
    // Convert to RGB order if needed (painted as BGR above).
    if fb.format == PixelFormat::Rgb {
        for p in front.iter_mut() {
            *p = (*p & 0x00FF00) | (*p >> 16 & 0xFF) | (*p & 0xFF) << 16;
        }
    }
}
