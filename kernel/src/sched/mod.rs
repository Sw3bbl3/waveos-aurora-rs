//! Preemptive round-robin scheduler.
//!
//! Every thread of execution is a task: kernel threads (pid 0) and the threads
//! of user processes, which run in ring 3 and enter the kernel through
//! interrupts and `syscall` on their own kernel stack. Task 0 is the boot
//! context, which becomes the idle task. Timer ticks drive preemption every
//! `QUANTUM_MS`; tasks can also sleep, yield, or block until woken.

use crate::arch::{switch, syscall};
use crate::mm::vmm;
use crate::sync::IrqMutex;
use crate::time;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::instructions::interrupts;

const STACK_SIZE: usize = 128 * 1024;
const QUANTUM_MS: u64 = 10;

pub type TaskId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ready,
    Running,
    /// Sleeping until the given tick, or until woken.
    Sleeping(u64),
    Dead,
}

struct Task {
    id: TaskId,
    name: String,
    rsp: u64,
    state: State,
    _stack: Option<Box<[u8]>>,
    cpu_ticks: u64,
    /// Owning process (0 = kernel).
    pid: u32,
    /// Page table root to run with.
    cr3: u64,
    /// Top of this task's kernel stack (for syscalls and ring-3 interrupts).
    kstack_top: u64,
}

struct Scheduler {
    tasks: Vec<Box<Task>>,
    current: usize,
    slice_start: u64,
}

static SCHED: IrqMutex<Option<Scheduler>> = IrqMutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let boot = Box::new(Task {
        id: 0,
        name: String::from("idle"),
        rsp: 0,
        state: State::Running,
        _stack: None,
        cpu_ticks: 0,
        pid: 0,
        cr3: vmm::kernel_pml4(),
        kstack_top: 0,
    });
    *SCHED.lock() = Some(Scheduler { tasks: alloc::vec![boot], current: 0, slice_start: 0 });
    STARTED.store(true, Ordering::Release);
    log!("sched", "scheduler online (quantum {} ms)", QUANTUM_MS);
}

/// Spawns a kernel thread running `f`.
pub fn spawn(name: &str, f: fn()) -> TaskId {
    fn call(f: u64) {
        let f: fn() = unsafe { core::mem::transmute(f as usize) };
        f();
    }
    let id = spawn_task(name, call, f as usize as u64, 0, vmm::kernel_pml4());
    log!("sched", "spawned task {} '{}'", id, name);
    id
}

/// Spawns a task running `f(arg)` in process `pid` with page tables `cr3`.
pub fn spawn_task(name: &str, f: fn(u64), arg: u64, pid: u32, cr3: u64) -> TaskId {
    let stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let top = (stack.as_ptr() as u64 + STACK_SIZE as u64) & !0xF;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let task = Box::new(Task {
        id,
        name: String::from(name),
        rsp: switch::init_stack(top, f as usize as u64, arg),
        state: State::Ready,
        _stack: Some(stack),
        cpu_ticks: 0,
        pid,
        cr3,
        kstack_top: top,
    });
    SCHED.lock().as_mut().unwrap().tasks.push(task);
    id
}

/// First code run by a new task (called from the asm trampoline).
pub extern "sysv64" fn task_entry(f: u64, arg: u64) -> ! {
    let f: fn(u64) = unsafe { core::mem::transmute(f as usize) };
    f(arg);
    exit();
}

pub fn exit() -> ! {
    interrupts::disable();
    if let Some(s) = SCHED.lock().as_mut() {
        let cur = s.current;
        s.tasks[cur].state = State::Dead;
    }
    schedule();
    unreachable!("dead task was rescheduled");
}

pub fn current_id() -> TaskId {
    SCHED.lock().as_ref().map(|s| s.tasks[s.current].id).unwrap_or(0)
}

/// Process owning the running task (0 for kernel threads).
pub fn current_pid() -> u32 {
    SCHED.lock().as_ref().map(|s| s.tasks[s.current].pid).unwrap_or(0)
}

/// Marks every task of `pid` except the caller as dead (they never run again).
pub fn kill_process_tasks(pid: u32) {
    if let Some(s) = SCHED.lock().as_mut() {
        let cur = s.current;
        for (i, t) in s.tasks.iter_mut().enumerate() {
            if t.pid == pid && i != cur {
                t.state = State::Dead;
            }
        }
    }
}

/// Picks the next runnable task and switches to it. Must be called with interrupts disabled.
fn schedule() {
    let now = time::ticks();
    let (save, load) = {
        let mut guard = SCHED.lock();
        let s = match guard.as_mut() {
            Some(s) => s,
            None => return,
        };
        let n = s.tasks.len();
        let cur = s.current;
        if s.tasks[cur].state == State::Running {
            s.tasks[cur].state = State::Ready;
        }
        // Round-robin over tasks 1..n; the idle task (0) only runs when nothing else can.
        let mut next = 0;
        for i in 1..=n {
            let idx = (cur + i) % n;
            if idx == 0 {
                continue;
            }
            let t = &mut s.tasks[idx];
            if let State::Sleeping(until) = t.state {
                if now >= until {
                    t.state = State::Ready;
                }
            }
            if t.state == State::Ready {
                next = idx;
                break;
            }
        }
        if next == 0 && s.tasks[0].state != State::Ready {
            s.tasks[0].state = State::Ready;
        }
        s.tasks[next].state = State::Running;
        s.slice_start = now;
        if next == cur {
            return;
        }
        s.current = next;
        crate::telemetry::switched(s.tasks[next].id);
        let save = &mut s.tasks[cur].rsp as *mut u64;
        let load = s.tasks[next].rsp;
        let (next_cr3, kstack) = (s.tasks[next].cr3, s.tasks[next].kstack_top);
        if next_cr3 != s.tasks[cur].cr3 {
            unsafe { vmm::load(next_cr3) };
        }
        if kstack != 0 {
            syscall::set_kernel_stack(kstack);
        }
        (save, load)
        // The lock guard drops here; interrupts stay off because the caller disabled them.
    };
    unsafe { switch::switch(save, load) };
    reap();
}

/// Frees the stacks of dead tasks (never the current one).
fn reap() {
    let dead: Vec<Box<Task>> = {
        let mut guard = SCHED.lock();
        let s = match guard.as_mut() {
            Some(s) => s,
            None => return,
        };
        let current_id = s.tasks[s.current].id;
        let mut dead = Vec::new();
        let mut i = 0;
        while i < s.tasks.len() {
            if s.tasks[i].state == State::Dead && s.tasks[i].id != current_id {
                dead.push(s.tasks.remove(i));
            } else {
                i += 1;
            }
        }
        s.current = s.tasks.iter().position(|t| t.id == current_id).unwrap();
        dead
    };
    drop(dead);
}

/// Called from the timer interrupt (interrupts already disabled).
pub fn on_timer_tick() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let now = time::ticks();
    let need = {
        let mut guard = SCHED.lock();
        let s = guard.as_mut().unwrap();
        let cur = s.current;
        s.tasks[cur].cpu_ticks += 1;
        let sleeper_due = s.tasks.iter().any(|t| matches!(t.state, State::Sleeping(u) if now >= u));
        let expired = now - s.slice_start >= QUANTUM_MS;
        sleeper_due && cur == 0 || expired
    };
    if need {
        schedule();
    }
}

pub fn yield_now() {
    interrupts::without_interrupts(schedule);
}

pub fn sleep_ms(ms: u64) {
    interrupts::without_interrupts(|| {
        let until = time::ticks().saturating_add(ms.saturating_mul(time::HZ) / 1000);
        if let Some(s) = SCHED.lock().as_mut() {
            let cur = s.current;
            s.tasks[cur].state = State::Sleeping(until);
        }
        schedule();
    });
}

/// Sleeps for at most `ms`, returning early if [`wake`] is called — unless
/// `ready()` already holds (checked with interrupts off, so no wakeup is lost).
pub fn wait_until(ms: u64, ready: impl Fn() -> bool) {
    interrupts::without_interrupts(|| {
        if ready() {
            return;
        }
        let until = time::ticks().saturating_add(ms.saturating_mul(time::HZ) / 1000);
        if let Some(s) = SCHED.lock().as_mut() {
            let cur = s.current;
            s.tasks[cur].state = State::Sleeping(until);
        }
        schedule();
    });
}

/// Makes a sleeping task runnable immediately (safe from interrupt handlers).
pub fn wake(id: TaskId) {
    if let Some(s) = SCHED.lock().as_mut() {
        if let Some(t) = s.tasks.iter_mut().find(|t| t.id == id) {
            if matches!(t.state, State::Sleeping(_)) {
                t.state = State::Ready;
            }
        }
    }
}

/// The idle loop run by the boot task once everything is up.
pub fn idle() -> ! {
    loop {
        interrupts::enable_and_hlt();
        yield_now();
    }
}

pub struct TaskInfo {
    pub id: TaskId,
    pub name: String,
    pub state: State,
    pub cpu_ticks: u64,
    pub pid: u32,
}

pub fn list() -> Vec<TaskInfo> {
    SCHED
        .lock()
        .as_ref()
        .map(|s| {
            s.tasks
                .iter()
                .map(|t| TaskInfo {
                    id: t.id,
                    name: t.name.clone(),
                    state: t.state,
                    cpu_ticks: t.cpu_ticks,
                    pid: t.pid,
                })
                .collect()
        })
        .unwrap_or_default()
}
