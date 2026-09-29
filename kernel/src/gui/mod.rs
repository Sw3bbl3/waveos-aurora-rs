//! Crest — the WaveOS Aurora compositor and desktop.
//!
//! Runs as a kernel task. Each frame it drains the input queue, lets the
//! desktop update, repaints only damaged regions into a RAM back buffer and
//! copies those regions to the GOP framebuffer.

pub mod apps;
pub mod cursor;
pub mod desktop;
pub mod server;

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
use spin::Once;

static FB: Once<Framebuffer> = Once::new();

pub fn init(fb: Framebuffer) {
    FB.call_once(|| fb);
}

pub fn screen_size() -> (i32, i32) {
    FB.get().map(|f| (f.width as i32, f.height as i32)).unwrap_or((1024, 768))
}

pub fn start() {
    sched::spawn("crest", run);
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
    let fb = *FB.get().expect("gui::init not called");
    let (w, h) = (fb.width as i32, fb.height as i32);
    input::set_consumer(sched::current_id());
    server::set_compositor(sched::current_id());

    let t0 = time::uptime_ms();
    let mut desktop = Desktop::new(w, h);
    log!("gui", "desktop composed in {} ms ({}x{})", time::uptime_ms() - t0, w, h);

    let mut back = vec![0u32; (w * h) as usize];
    let front = front_buffer(&fb);
    let mut ready = false;
    let mut next_tick = 0;
    loop {
        let now = time::uptime_ms();
        while let Some(ev) = input::pop() {
            desktop.handle(ev, now);
        }
        desktop.process_commands();
        if now >= next_tick {
            next_tick = now + desktop.tick(now);
        }
        for r in desktop.take_damage() {
            let mut cv = Canvas::new(&mut back, w, h);
            cv.clip = r;
            desktop.paint(&mut cv);
            flush(front, &back, &fb, r);
        }
        if !ready {
            ready = true;
            log!("boot", "Aurora desktop ready");
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
        sched::wait_until(wait, || input::pending() || server::pending());
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
    let Some(fb) = FB.get() else { return };
    let front = front_buffer(fb);
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
