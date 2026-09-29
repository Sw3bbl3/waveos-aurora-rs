//! Notifications: posted by apps (`notify`) or the system, shown as banners
//! and kept in the Notification Center.

use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone)]
pub struct Notification {
    pub id: u32,
    /// Posting process (0 = the system).
    pub pid: u32,
    /// Program path of the poster, for its icon and to focus it on click.
    pub app_path: String,
    pub title: String,
    pub body: String,
    /// "2:05 PM"
    pub time: String,
}

static INCOMING: IrqMutex<VecDeque<Notification>> = IrqMutex::new(VecDeque::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn clip(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

fn time_now() -> String {
    let d = crate::drivers::rtc::now();
    let h24 = super::settings::get_bool("clock24", false);
    if h24 {
        alloc::format!("{:02}:{:02}", d.hour, d.minute)
    } else {
        let (h, ampm) = match d.hour {
            0 => (12, "AM"),
            1..=11 => (d.hour, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        alloc::format!("{h}:{:02} {ampm}", d.minute)
    }
}

/// Queues a notification; the desktop shows it on its next frame.
pub fn post(pid: u32, app_path: &str, title: &str, body: &str) {
    let n = Notification {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        pid,
        app_path: String::from(app_path),
        title: clip(title, 80),
        body: clip(body, 240),
        time: time_now(),
    };
    let mut q = INCOMING.lock();
    if q.len() < 64 {
        q.push_back(n);
    }
    drop(q);
    super::server::poke();
}

/// A notification from the system itself (screenshots, devices, …).
pub fn system(title: &str, body: &str) {
    post(0, "", title, body);
}

pub fn take() -> Option<Notification> {
    INCOMING.lock().pop_front()
}

pub fn pending() -> bool {
    !INCOMING.lock().is_empty()
}
