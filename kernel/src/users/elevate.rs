//! Elevation: a process without the administrator capability asks for it
//! for a privileged action. The window server shows the system prompt
//! (its own window: the asking app never sees what is typed), checks the
//! administrator's password itself, and answers. A yes gives the process
//! the capability for [`super::ELEVATION_MS`]; anything else is `EPERM`.

use crate::proc::{Pid, Process};
use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_abi::err::*;

/// How long a prompt waits for an answer.
const TIMEOUT_MS: u64 = 120_000;

/// A question waiting for the prompt.
#[derive(Clone)]
pub struct Request {
    pub id: u64,
    pub pid: Pid,
    /// The asking program's name.
    pub app: String,
    /// What it wants to do, in its words ("change the password of bob").
    pub reason: String,
}

struct Slot {
    req: Request,
    /// Some(true) granted, Some(false) refused.
    answer: Option<bool>,
}

static QUEUE: IrqMutex<VecDeque<Slot>> = IrqMutex::new(VecDeque::new());
static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

/// Asks for the administrator capability for `p` (blocks until answered).
pub fn ask(p: &Arc<Process>, reason: &str) -> Result<(), isize> {
    if !crate::gui::server::running() {
        return Err(EPERM);
    }
    // One question per process at a time.
    if QUEUE.lock().iter().any(|s| s.req.pid == p.pid) {
        return Err(EBUSY);
    }
    let id = NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let reason: String = reason.chars().filter(|c| !c.is_control()).take(120).collect();
    QUEUE.lock().push_back(Slot { req: Request { id, pid: p.pid, app: p.name(), reason }, answer: None });
    crate::gui::server::command(crate::gui::server::Command::Elevate);
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
    if answer != Some(true) {
        // Timed out or interrupted: the prompt goes away.
        crate::gui::server::command(crate::gui::server::Command::Elevate);
        return Err(EPERM);
    }
    p.cred.lock().elevated_until = crate::time::uptime_ms() + super::ELEVATION_MS;
    log!("users", "pid {} '{}' got the administrator capability", p.pid, p.name());
    Ok(())
}

/// The question the prompt should show now (the oldest unanswered one).
pub fn pending() -> Option<Request> {
    QUEUE.lock().iter().find(|s| s.answer.is_none()).map(|s| s.req.clone())
}

/// The prompt's answer to request `id`.
pub fn answer(id: u64, granted: bool) {
    let pid = {
        let mut q = QUEUE.lock();
        let Some(s) = q.iter_mut().find(|s| s.req.id == id) else { return };
        s.answer = Some(granted);
        s.req.pid
    };
    crate::sched::wake_process(pid);
}
