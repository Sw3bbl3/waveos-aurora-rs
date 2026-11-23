//! Lumen Server — the WaveOS Aurora compositor and desktop.
//!
//! Runs as a kernel task. Each frame it drains the input queue, lets the
//! desktop update, repaints only damaged regions into each display's back
//! buffer and presents them (see `screens` for several displays).

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
pub mod screens;
pub mod server;
pub mod settings;
pub mod stages;
pub mod sysupdate;

pub use lumen::{canvas, geom, icons, theme, wallpaper, widgets};

use crate::drivers::gpu::output::Output;
use crate::drivers::input;
use crate::mm::phys_to_virt;
use crate::{sched, time};
use bootinfo::{Framebuffer, PixelFormat};
use canvas::{rgb, Canvas};
use core::fmt::Write;
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

/// The main display's size in points (what windows and the pointer use).
pub fn screen_size() -> (i32, i32) {
    let (w, h) = screen_pixels();
    (w / scale(), h / scale())
}

/// The main display's size in device pixels.
pub fn screen_pixels() -> (i32, i32) {
    current_fb().map(|f| (f.width as i32, f.height as i32)).unwrap_or((1024, 768))
}

/// Device pixels per point on the main display: 2 on a HiDPI screen.
static SCALE: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(1);

pub fn scale() -> i32 {
    SCALE.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn start() {
    // The compositor keeps its own SIMD registers, so drawing can use SSE2.
    lumen::simd::set_gate(simd_allowed);
    sched::spawn_simd("lumen-server", run);
}

/// SSE2 drawing is safe only in a kernel task that owns saved SIMD state,
/// and never inside an interrupt handler (which runs with interrupts off).
pub(crate) fn simd_allowed() -> bool {
    crate::arch::percpu::this().simd.load(core::sync::atomic::Ordering::Relaxed)
        && x86_64::instructions::interrupts::are_enabled()
}

/// A display driver reported new display modes (a host window resized,
/// a monitor plugged in).
pub fn display_changed() {
    server::command(server::Command::DisplayChanged);
}

/// A network interface got (or lost) its address.
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

/// The screen as a `Framebuffer`: its size, and where the panic screen
/// may draw (a physical framebuffer the CPU writes, or none: `phys_addr` 0).
fn describe(out: &Output) -> Framebuffer {
    let (w, h) = out.size();
    out.panic_framebuffer().filter(|f| (f.width, f.height) == (w, h)).unwrap_or(Framebuffer {
        phys_addr: 0,
        size: 0,
        width: w,
        height: h,
        stride: w,
        format: PixelFormat::Bgr,
    })
}

/// Where the pointer goes this frame.
#[derive(PartialEq)]
pub(crate) enum PointerOn {
    /// The display's cursor plane.
    Plane,
    /// The picture: this shape is larger than the plane at this scale.
    Picture,
    /// The plane failed; the compositor draws the pointer from now on.
    Failed,
}

/// Shows the pointer on the display's cursor plane: draws a new cursor
/// image when the shape, its activity frame or the scale changed, and
/// moves it.
pub(crate) fn update_hw_cursor(
    out: &mut Output,
    shown: &mut Option<(u32, u64, i32)>,
    pos: (i32, i32),
    shape: u32,
    now: u64,
    s: i32,
) -> PointerOn {
    if out.move_cursor(pos.0 * s, pos.1 * s).is_err() {
        return PointerOn::Failed;
    }
    let key = (shape, cursor::frame(shape, now), s);
    if *shown == Some(key) {
        return PointerOn::Plane;
    }
    if shown.is_some_and(|k| (k.0, k.2) == (u32::MAX, s) && k.1 == shape as u64) {
        return PointerOn::Picture; // still the oversized shape
    }
    if shape == aurora_abi::cursor::HIDDEN {
        *shown = Some(key);
        return if out.hide_cursor().is_ok() { PointerOn::Plane } else { PointerOn::Failed };
    }
    let max = out.cursor_size().0;
    match cursor::image(shape, key.1, s, max) {
        Some(img) => {
            *shown = Some(key);
            let ok = out.set_cursor(&img.pixels, img.width, img.height, img.hot).is_ok() && out.show_cursor().is_ok();
            if ok {
                PointerOn::Plane
            } else {
                PointerOn::Failed
            }
        }
        None => {
            // Remembered as (MAX, shape, scale) so the image isn't tried again each frame.
            *shown = Some((u32::MAX, shape as u64, s));
            let _ = out.hide_cursor();
            PointerOn::Picture
        }
    }
}

fn run() {
    input::set_consumer(sched::current_id());
    crate::drivers::gamepad::set_consumer(sched::current_id());
    server::set_compositor(sched::current_id());

    crate::telemetry::stage("gui", "Lumen Server window server");
    load_settings();
    prefs::apply_appearance();
    screens::Compositor::new().run()
}

/// The main display is `out`, drawn at `scale`: what `screen_size`,
/// `scale` and the panic screen go by.
pub(crate) fn set_main(out: &Output, scale: i32) {
    *FB.lock() = Some(describe(out));
    SCALE.store(scale, core::sync::atomic::Ordering::Relaxed);
}

static SCREENSHOT: crate::sync::IrqMutex<Option<(alloc::vec::Vec<u32>, u32, u32)>> = crate::sync::IrqMutex::new(None);

/// Saves the screen to `/Pictures` as PNG (encoded on a background task).
pub(crate) fn take_screenshot(back: &[u32], w: u32, h: u32) {
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
    // The RTC has kept local time in this zone.
    time::learn_zone(settings::get_int("utc_offset", 0));
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
    // A scanout buffer in ordinary RAM (Intel graphics) is read by a display
    // engine that doesn't snoop the CPU caches: write the picture back.
    unsafe { core::arch::asm!("wbinvd", options(nostack)) };
}
