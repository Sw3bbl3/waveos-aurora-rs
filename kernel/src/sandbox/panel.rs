//! The system open and save panel (the "powerbox"): an app asks, the
//! window server shows its own panel (`gui::apps::PanelDialog`, so the app
//! never sees what else is there), the person picks a file or folder, and
//! the asking process is given exactly that ([`super::Sandbox::grant`]).

use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use aurora_abi::err::*;
use aurora_abi::sandbox as cap;

/// How long a panel waits for the person.
const TIMEOUT_MS: u64 = 15 * 60 * 1000;

#[derive(Clone)]
pub struct Request {
    pub id: u64,
    pub pid: u32,
    /// `PANEL_OPEN`, `PANEL_SAVE` or `PANEL_FOLDER`.
    pub mode: u64,
    /// The asking app's name.
    pub app: String,
    /// Saving: the name suggested.
    pub name: String,
    /// The folder to start in.
    pub start: String,
}

struct Slot {
    req: Request,
    /// Some(None): cancelled.
    answer: Option<Option<String>>,
}

static QUEUE: IrqMutex<VecDeque<Slot>> = IrqMutex::new(VecDeque::new());
static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
/// Self-tests pick for the person.
static AUTO: IrqMutex<Option<Option<String>>> = IrqMutex::new(None);

#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn set_auto_answer(answer: Option<Option<String>>) {
    *AUTO.lock() = answer;
}

/// Shows the panel for the calling process; returns the path picked and
/// grants it to the caller (read and write).
pub fn ask(mode: u64, name: &str) -> Result<String, isize> {
    if !matches!(mode, cap::PANEL_OPEN | cap::PANEL_SAVE | cap::PANEL_FOLDER) {
        return Err(EINVAL);
    }
    let me = crate::proc::current().ok_or(EPERM)?;
    let auto = AUTO.lock().clone();
    let picked = match auto {
        Some(a) => a,
        None => {
            if !crate::gui::server::running() {
                return Err(ECANCELED);
            }
            if QUEUE.lock().iter().any(|s| s.req.pid == me.pid) {
                return Err(EBUSY);
            }
            let uid = me.cred.lock().uid;
            let app = me
                .sandbox
                .lock()
                .as_ref()
                .map(|s| s.profile.name.clone())
                .unwrap_or_else(|| me.name());
            let name: String = name.chars().filter(|c| !c.is_control() && *c != '/').take(120).collect();
            let start = super::home_of(uid);
            let id = NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            QUEUE.lock().push_back(Slot { req: Request { id, pid: me.pid, mode, app, name, start }, answer: None });
            crate::gui::server::command(crate::gui::server::Command::Panel);
            let deadline = crate::time::uptime_ms() + TIMEOUT_MS;
            let answered = || QUEUE.lock().iter().find(|s| s.req.id == id).is_none_or(|s| s.answer.is_some());
            while !answered() && !crate::proc::interrupted() && crate::time::uptime_ms() < deadline {
                crate::sched::wait_until(deadline.saturating_sub(crate::time::uptime_ms()), || {
                    answered() || crate::proc::interrupted()
                });
            }
            let answer = {
                let mut q = QUEUE.lock();
                let i = q.iter().position(|s| s.req.id == id);
                i.and_then(|i| q.remove(i)).and_then(|s| s.answer)
            };
            crate::gui::server::command(crate::gui::server::Command::Panel);
            answer.flatten()
        }
    };
    let path = crate::fs::resolve("/", &picked.ok_or(ECANCELED)?);
    if let Some(s) = me.sandbox.lock().clone() {
        s.grant(&path, true);
    }
    Ok(path)
}

/// The panel the window server should show now.
pub fn pending() -> Option<Request> {
    QUEUE.lock().iter().find(|s| s.answer.is_none()).map(|s| s.req.clone())
}

/// The person picked `path` (None: cancelled) for request `id`.
pub fn answer(id: u64, path: Option<String>) {
    let pid = {
        let mut q = QUEUE.lock();
        let Some(s) = q.iter_mut().find(|s| s.req.id == id) else { return };
        s.answer = Some(path);
        s.req.pid
    };
    crate::sched::wake_process(pid);
}
