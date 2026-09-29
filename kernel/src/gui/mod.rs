//! Crest — the WaveOS Aurora compositor and desktop.
//!
//! Runs as a kernel task. Each frame it drains the input queue, lets the
//! desktop update, repaints only damaged regions into a RAM back buffer and
//! copies those regions to the GOP framebuffer.

pub mod apps;
pub mod clipboard;
pub mod cursor;
pub mod desktop;
pub mod dnd;
pub mod notify;
pub mod prefs;
pub mod server;
pub mod settings;

pub use aurora_gfx::{canvas, geom, icons, theme, wallpaper, widgets};

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

pub fn init(fb: Framebuffer) {
    *FB.lock() = Some(fb);
    load_fonts();
}

fn current_fb() -> Option<Framebuffer> {
    *FB.lock()
}

/// Installs the TrueType UI faces from the system image (text falls back to
/// a few built-in sizes without them).
fn load_fonts() {
    use aurora_gfx::font::{self, Face};
    for face in [Face::Regular, Face::SemiBold, Face::Mono] {
        let ok = crate::fs::read_all(face.file())
            .is_ok_and(|data| font::install(face, alloc::boxed::Box::leak(data.into_boxed_slice())));
        if !ok {
            log!("gui", "font {} unavailable; using built-in sizes", face.file());
        }
    }
}

pub fn screen_size() -> (i32, i32) {
    current_fb().map(|f| (f.width as i32, f.height as i32)).unwrap_or((1024, 768))
}

pub fn start() {
    sched::spawn("crest", run);
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
        }
    }
}

fn front_buffer(fb: &Framebuffer) -> &'static mut [u32] {
    unsafe { core::slice::from_raw_parts_mut(phys_to_virt(fb.phys_addr) as *mut u32, (fb.stride * fb.height) as usize) }
}

/// Copies `r` from the back buffer to the framebuffer.
fn flush(front: &mut [u32], back: &[u32], fb: &Framebuffer, r: Rect) {
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
}

fn run() {
    let mut fb = current_fb().expect("gui::init not called");
    let (mut w, mut h) = (fb.width as i32, fb.height as i32);
    input::set_consumer(sched::current_id());
    server::set_compositor(sched::current_id());

    crate::telemetry::stage("gui", "Crest window server");
    load_settings();
    let t0 = time::uptime_ms();
    let mut desktop = Desktop::new(w, h);
    log!("gui", "desktop composed in {} ms ({}x{})", time::uptime_ms() - t0, w, h);

    let mut back = vec![0u32; (w * h) as usize];
    let mut front = front_buffer(&fb);
    let mut ready = false;
    let mut next_tick = 0;
    loop {
        let now = time::uptime_ms();
        desktop.set_now(now);
        while let Some(ev) = input::pop() {
            desktop.handle(ev, now);
        }
        desktop.process_commands();
        // A new resolution: new framebuffer, back buffer and layout.
        if let Some((nw, nh)) = desktop.resolution_request.take() {
            match crate::drivers::display::set_mode(nw, nh) {
                Some(new_fb) => {
                    fb = new_fb;
                    *FB.lock() = Some(fb);
                    (w, h) = (fb.width as i32, fb.height as i32);
                    back = vec![0u32; (w * h) as usize];
                    front = front_buffer(&fb);
                    input::set_screen_size(w, h);
                    desktop.resize(w, h);
                    server::broadcast(aurora_abi::Event {
                        kind: aurora_abi::event::SCREEN,
                        x: w,
                        y: h,
                        ..Default::default()
                    });
                }
                None => log!("gui", "the display can't show {}x{}", nw, nh),
            }
        }
        // Something started moving: draw the next frame promptly.
        if desktop.wants_frames() {
            next_tick = next_tick.min(now + 16);
        }
        if now >= next_tick {
            next_tick = now + desktop.tick(now);
        }
        let damage = desktop.take_damage();
        if !damage.is_empty() {
            crate::telemetry::FRAMES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        for r in damage {
            crate::telemetry::PIXELS.fetch_add(r.area() as u64, core::sync::atomic::Ordering::Relaxed);
            let mut cv = Canvas::new(&mut back, w, h);
            cv.clip = r;
            desktop.paint(&mut cv);
            flush(front, &back, &fb, r);
        }
        if core::mem::take(&mut desktop.screenshot) {
            take_screenshot(&back, w as u32, h as u32);
        }
        if !ready {
            ready = true;
            log!("boot", "Aurora desktop ready");
            crate::drivers::audio::play_sound("startup");
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
    theme::set_dark(settings::get_bool("dark", false));
    theme::set_accent(settings::get_int("accent", 0) as u8);
    prefs::apply_all_system();
    let wp = settings::get_int("wallpaper", 0) as u8;
    let valid = (wp as usize) < wallpaper::NAMES.len() || wp == wallpaper::CUSTOM;
    theme::set_wallpaper(if valid { wp } else { 0 });
    log!("gui", "restored settings (dark: {}, wallpaper: {})", theme::current().dark, theme::wallpaper());
}

/// Persists appearance settings to the home volume.
pub fn save_settings() {
    settings::set_bool("dark", theme::current().dark);
    settings::set_int("wallpaper", theme::wallpaper() as i64);
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
    let front = front_buffer(&fb);
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
