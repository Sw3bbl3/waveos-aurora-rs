//! Where the compositor's time goes: nanoseconds per drawing stage, summed
//! over frames and logged with the compositor's 10-second statistics.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

#[derive(Clone, Copy)]
pub enum Stage {
    Wallpaper,
    Water,
    Items,
    Shadow,
    Body,
    App,
    Chrome,
    Overlays,
    Scene,
    Cursor,
    Flush,
    Backdrop,
}

const NAMES: [&str; 12] = [
    "wallpaper",
    "water",
    "items",
    "shadow",
    "body",
    "app",
    "chrome",
    "overlays",
    "scene",
    "cursor",
    "flush",
    "backdrop",
];

static TOTALS: [AtomicU64; 12] = [const { AtomicU64::new(0) }; 12];

/// Times `f` as part of `stage`.
#[inline]
pub fn time<R>(stage: Stage, f: impl FnOnce() -> R) -> R {
    let t = crate::time::now_ns();
    let r = f();
    TOTALS[stage as usize].fetch_add(crate::time::now_ns().saturating_sub(t), Relaxed);
    r
}

/// Writes "stage ms, …" for the stages that took time, and starts again.
pub fn take(out: &mut alloc::string::String) {
    use core::fmt::Write;
    for (name, total) in NAMES.iter().zip(&TOTALS) {
        let ns = total.swap(0, Relaxed);
        if ns >= 100_000 {
            let _ = write!(
                out,
                "{}{} {}.{} ms",
                if out.is_empty() { "" } else { ", " },
                name,
                ns / 1_000_000,
                ns / 100_000 % 10
            );
        }
    }
}

/// Adds `ns` to `stage`.
#[inline]
pub fn add(stage: Stage, ns: u64) {
    TOTALS[stage as usize].fetch_add(ns, Relaxed);
}
