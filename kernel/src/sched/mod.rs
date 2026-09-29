//! The scheduler: preemptive, per-CPU run queues, for any number of CPUs.
//!
//! Every thread of execution is a task: kernel threads (pid 0) and the threads
//! of user processes. Each CPU has its own run queue and an idle task (the
//! context that brought the CPU up). A CPU whose queue runs dry steals from
//! the busiest one; a task woken for an idle CPU gets there by IPI. Timer
//! ticks drive preemption every `QUANTUM_MS`.
//!
//! One lock guards the task table and all queues. Switching is split in two:
//! under the lock a CPU decides and records the switch, then — lock released,
//! interrupts still off — it swaps stacks. A task leaving a CPU keeps its
//! `on_cpu` flag until the CPU is running something else (`finish_switch`),
//! so another CPU that already picked it waits instead of resuming a stack
//! that is still in use.

use crate::arch::fpu::FpuState;
use crate::arch::percpu;
use crate::arch::{switch, syscall};
use crate::mm::vmm;
use crate::sync::IrqMutex;
use crate::time;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
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
    /// Sleeping until the given tick (millisecond), or until woken.
    Sleeping(u64),
    Dead,
}

struct Task {
    name: String,
    rsp: u64,
    state: State,
    _stack: Option<Box<[u8]>>,
    cpu_ns: u64,
    /// Owning process (0 = kernel).
    pid: u32,
    /// Page table root to run with.
    cr3: u64,
    /// Top of this task's kernel stack (for syscalls and ring-3 interrupts).
    kstack_top: u64,
    /// SIMD registers of user tasks (kernel threads never use them).
    fpu: Option<Box<FpuState>>,
    last_cpu: usize,
    /// The CPU it is scheduled on right now, if any.
    running_on: Option<usize>,
    /// Its stack is in use by a CPU (cleared only once that CPU has switched away).
    on_cpu: AtomicBool,
    /// Idle tasks never go on a queue.
    idle: bool,
}

struct Cpu {
    current: TaskId,
    idle: TaskId,
    queue: VecDeque<TaskId>,
    /// When the current task started running (ns).
    slice_start: u64,
    /// The task switched away from, for `finish_switch`.
    prev: Option<TaskId>,
    busy_ns: u64,
    idle_ns: u64,
}

struct Scheduler {
    tasks: BTreeMap<TaskId, Box<Task>>,
    cpus: Vec<Cpu>,
}

static SCHED: IrqMutex<Option<Scheduler>> = IrqMutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);

fn new_task(
    name: &str,
    rsp: u64,
    stack: Option<Box<[u8]>>,
    pid: u32,
    cr3: u64,
    kstack_top: u64,
    cpu: usize,
) -> Box<Task> {
    Box::new(Task {
        name: String::from(name),
        rsp,
        state: State::Ready,
        _stack: stack,
        cpu_ns: 0,
        pid,
        cr3,
        kstack_top,
        fpu: (pid != 0).then(|| Box::new(FpuState::new())),
        last_cpu: cpu,
        running_on: None,
        on_cpu: AtomicBool::new(false),
        idle: false,
    })
}

/// Adopts the calling context as `cpu`'s idle task.
fn adopt_idle(s: &mut Scheduler, cpu: usize, id: TaskId) {
    let mut t = new_task("idle", 0, None, 0, vmm::kernel_pml4(), 0, cpu);
    t.state = State::Running;
    t.running_on = Some(cpu);
    t.on_cpu.store(true, Ordering::Relaxed);
    t.idle = true;
    s.tasks.insert(id, t);
    while s.cpus.len() <= cpu {
        s.cpus.push(Cpu {
            current: 0,
            idle: 0,
            queue: VecDeque::new(),
            slice_start: 0,
            prev: None,
            busy_ns: 0,
            idle_ns: 0,
        });
    }
    let c = &mut s.cpus[cpu];
    c.current = id;
    c.idle = id;
    c.slice_start = time::now_ns();
    let pc = &percpu::CPUS[cpu];
    pc.idle.store(true, Ordering::Relaxed);
    pc.task.store(id, Ordering::Relaxed);
    pc.pid.store(0, Ordering::Relaxed);
    pc.cr3.store(vmm::kernel_pml4(), Ordering::Relaxed);
}

/// The bootstrap context becomes CPU 0's idle task (id 0).
pub fn init() {
    let mut s = Scheduler { tasks: BTreeMap::new(), cpus: Vec::new() };
    adopt_idle(&mut s, 0, 0);
    *SCHED.lock() = Some(s);
    STARTED.store(true, Ordering::Release);
    log!("sched", "scheduler online (quantum {} ms)", QUANTUM_MS);
}

/// Called by each application processor to join scheduling.
pub fn init_ap(cpu: usize) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if let Some(s) = SCHED.lock().as_mut() {
        adopt_idle(s, cpu, id);
    }
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

/// The CPU with the least work (queued tasks, plus one if busy).
fn least_loaded(s: &Scheduler) -> usize {
    percpu::online()
        .filter(|&c| c < s.cpus.len())
        .min_by_key(|&c| s.cpus[c].queue.len() + (s.cpus[c].current != s.cpus[c].idle) as usize)
        .unwrap_or(0)
}

/// Queues a ready task on `cpu`; returns the CPU to kick with an IPI, if any.
fn enqueue(s: &mut Scheduler, id: TaskId, mut cpu: usize) -> Option<usize> {
    if cpu >= s.cpus.len() || !percpu::CPUS[cpu].online.load(Ordering::Acquire) {
        cpu = 0;
    }
    // Prefer an idle CPU over waiting behind a busy one.
    if s.cpus[cpu].current != s.cpus[cpu].idle || !s.cpus[cpu].queue.is_empty() {
        if let Some(idle) = percpu::online()
            .find(|&c| c < s.cpus.len() && s.cpus[c].current == s.cpus[c].idle && s.cpus[c].queue.is_empty())
        {
            cpu = idle;
        }
    }
    s.cpus[cpu].queue.push_back(id);
    (s.cpus[cpu].current == s.cpus[cpu].idle && cpu != percpu::cpu_id()).then_some(cpu)
}

fn kick(cpu: Option<usize>) {
    if let Some(c) = cpu {
        crate::arch::apic::send_ipi(
            percpu::CPUS[c].lapic_id.load(Ordering::Relaxed),
            crate::arch::idt::RESCHEDULE_VECTOR,
        );
    }
}

/// Spawns a task running `f(arg)` in process `pid` with page tables `cr3`.
pub fn spawn_task(name: &str, f: fn(u64), arg: u64, pid: u32, cr3: u64) -> TaskId {
    let stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let top = (stack.as_ptr() as u64 + STACK_SIZE as u64) & !0xF;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let rsp = switch::init_stack(top, f as usize as u64, arg);
    let target = interrupts::without_interrupts(|| {
        let mut guard = SCHED.lock();
        let s = guard.as_mut().unwrap();
        let cpu = least_loaded(s);
        s.tasks.insert(id, new_task(name, rsp, Some(stack), pid, cr3, top, cpu));
        enqueue(s, id, cpu)
    });
    kick(target);
    id
}

/// First code run by a new task (called from the asm trampoline).
pub extern "sysv64" fn task_entry(f: u64, arg: u64) -> ! {
    finish_switch();
    interrupts::enable();
    let f: fn(u64) = unsafe { core::mem::transmute(f as usize) };
    f(arg);
    exit();
}

pub fn exit() -> ! {
    interrupts::disable();
    if let Some(s) = SCHED.lock().as_mut() {
        let cur = s.cpus[percpu::cpu_id()].current;
        if let Some(t) = s.tasks.get_mut(&cur) {
            t.state = State::Dead;
        }
    }
    schedule();
    unreachable!("dead task was rescheduled");
}

pub fn current_id() -> TaskId {
    percpu::this().task.load(Ordering::Relaxed)
}

/// Process owning the running task (0 for kernel threads).
pub fn current_pid() -> u32 {
    percpu::this().pid.load(Ordering::Relaxed)
}

/// Wakes every sleeping task of `pid` (e.g. so they notice a kill request).
pub fn wake_process(pid: u32) {
    let ids: Vec<TaskId> = SCHED
        .lock()
        .as_ref()
        .map(|s| s.tasks.iter().filter(|(_, t)| t.pid == pid).map(|(&id, _)| id).collect())
        .unwrap_or_default();
    for id in ids {
        wake(id);
    }
}

/// Takes the next task for `cpu`: its own queue first, then the busiest other queue.
fn pick_next(s: &mut Scheduler, cpu: usize) -> Option<TaskId> {
    while let Some(id) = s.cpus[cpu].queue.pop_front() {
        if s.tasks.get(&id).is_some_and(|t| t.state == State::Ready && t.running_on.is_none()) {
            return Some(id);
        }
    }
    let victim = (0..s.cpus.len()).filter(|&c| c != cpu).max_by_key(|&c| s.cpus[c].queue.len())?;
    let q = &mut s.cpus[victim].queue;
    let pos =
        q.iter().position(|id| s.tasks.get(id).is_some_and(|t| t.state == State::Ready && t.running_on.is_none()))?;
    q.remove(pos)
}

/// Picks the next task for this CPU and switches to it. Interrupts must be off.
fn schedule() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let cpu = percpu::cpu_id();
    let now = time::now_ns();
    let (save, load, next_flag) = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else { return };
        if cpu >= s.cpus.len() {
            return;
        }
        let cur_id = s.cpus[cpu].current;
        let delta = now.saturating_sub(s.cpus[cpu].slice_start);
        let cur_is_idle = cur_id == s.cpus[cpu].idle;
        if cur_is_idle {
            s.cpus[cpu].idle_ns += delta;
        } else {
            s.cpus[cpu].busy_ns += delta;
        }
        s.cpus[cpu].slice_start = now;
        let requeue = {
            let cur = s.tasks.get_mut(&cur_id).unwrap();
            cur.cpu_ns += delta;
            match cur.state {
                State::Running | State::Ready => {
                    cur.state = State::Ready;
                    !cur.idle
                }
                _ => false,
            }
        };
        let next_id = match pick_next(s, cpu) {
            Some(id) => id,
            None if requeue => cur_id,
            None => s.cpus[cpu].idle,
        };
        if next_id == cur_id {
            s.tasks.get_mut(&cur_id).unwrap().state = State::Running;
            return;
        }
        if requeue {
            s.cpus[cpu].queue.push_back(cur_id);
        }
        // Record the switch.
        let (cur_cr3, save) = {
            let cur = s.tasks.get_mut(&cur_id).unwrap();
            cur.running_on = None;
            if let Some(f) = cur.fpu.as_mut() {
                f.save();
            }
            (cur.cr3, &mut cur.rsp as *mut u64)
        };
        let next = s.tasks.get_mut(&next_id).unwrap();
        next.state = State::Running;
        next.running_on = Some(cpu);
        next.last_cpu = cpu;
        if let Some(f) = next.fpu.as_ref() {
            f.restore();
        }
        if next.cr3 != cur_cr3 {
            unsafe { vmm::load(next.cr3) };
        }
        if next.kstack_top != 0 {
            syscall::set_kernel_stack(next.kstack_top);
        }
        let pc = &percpu::CPUS[cpu];
        pc.task.store(next_id, Ordering::Relaxed);
        pc.pid.store(next.pid, Ordering::Relaxed);
        pc.cr3.store(next.cr3, Ordering::Relaxed);
        pc.idle.store(next.idle, Ordering::Relaxed);
        let load = next.rsp;
        let flag = &next.on_cpu as *const AtomicBool;
        s.cpus[cpu].current = next_id;
        s.cpus[cpu].prev = Some(cur_id);
        crate::telemetry::switched(next_id, cpu);
        (save, load, flag)
        // The lock guard drops here; interrupts stay off because the caller disabled them.
    };
    // The task may still be leaving another CPU: wait until its stack is free.
    let flag = unsafe { &*next_flag };
    while flag.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }
    unsafe { switch::switch(save, load) };
    finish_switch();
}

/// Runs on a CPU right after it switched to a new stack: releases the task it
/// left and frees it if it had exited.
fn finish_switch() {
    let cpu = percpu::cpu_id();
    let dead = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else { return };
        let Some(prev) = s.cpus.get_mut(cpu).and_then(|c| c.prev.take()) else { return };
        let dead = s.tasks.get(&prev).is_some_and(|t| t.state == State::Dead);
        if dead {
            s.tasks.remove(&prev)
        } else {
            if let Some(t) = s.tasks.get(&prev) {
                t.on_cpu.store(false, Ordering::Release);
            }
            None
        }
    };
    // A user thread is gone only once no CPU runs on its stack or page tables.
    if let Some(t) = dead {
        let pid = t.pid;
        drop(t);
        if pid != 0 {
            crate::proc::thread_gone(pid);
        }
    }
}

/// Called from each CPU's timer interrupt (interrupts already disabled).
pub fn on_timer_tick() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let cpu = percpu::cpu_id();
    if cpu == 0 {
        time::tick();
        wake_sleepers();
    }
    let now = time::now_ns();
    let need = {
        let guard = SCHED.lock();
        let Some(s) = guard.as_ref() else { return };
        let Some(c) = s.cpus.get(cpu) else { return };
        let others = !c.queue.is_empty();
        let expired = now.saturating_sub(c.slice_start) >= QUANTUM_MS * 1_000_000;
        (c.current == c.idle && others) || (expired && others)
    };
    if need {
        schedule();
    }
}

/// Reschedule IPI: another CPU queued work for this one.
pub fn on_reschedule_ipi() {
    schedule();
}

/// Wakes tasks whose sleep has run out (CPU 0, every tick).
fn wake_sleepers() {
    let now = time::ticks();
    let due: Vec<TaskId> = match SCHED.lock().as_ref() {
        Some(s) => s
            .tasks
            .iter()
            .filter(|(_, t)| matches!(t.state, State::Sleeping(u) if now >= u))
            .map(|(&id, _)| id)
            .collect(),
        None => return,
    };
    for id in due {
        wake(id);
    }
}

pub fn yield_now() {
    interrupts::without_interrupts(schedule);
}

fn mark_sleeping(until: u64) {
    if let Some(s) = SCHED.lock().as_mut() {
        let cur = s.cpus[percpu::cpu_id()].current;
        if let Some(t) = s.tasks.get_mut(&cur) {
            if !t.idle {
                t.state = State::Sleeping(until);
            }
        }
    }
}

pub fn sleep_ms(ms: u64) {
    interrupts::without_interrupts(|| {
        mark_sleeping(time::ticks().saturating_add(ms.saturating_mul(time::HZ) / 1000));
        schedule();
    });
}

/// Sleeps for at most `ms`, returning early if [`wake`] is called — unless
/// `ready()` already holds. The task is marked sleeping *before* `ready()` is
/// checked, so a wakeup from another CPU in between is never lost.
pub fn wait_until(ms: u64, ready: impl Fn() -> bool) {
    interrupts::without_interrupts(|| {
        mark_sleeping(time::ticks().saturating_add(ms.saturating_mul(time::HZ) / 1000));
        if ready() {
            if let Some(s) = SCHED.lock().as_mut() {
                let cur = s.cpus[percpu::cpu_id()].current;
                if let Some(t) = s.tasks.get_mut(&cur) {
                    if matches!(t.state, State::Sleeping(_) | State::Ready) {
                        t.state = State::Running;
                    }
                }
            }
            return;
        }
        schedule();
    });
}

/// Makes a sleeping task runnable (safe from interrupt handlers and any CPU).
pub fn wake(id: TaskId) {
    let target = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else { return };
        let Some(t) = s.tasks.get_mut(&id) else { return };
        if !matches!(t.state, State::Sleeping(_)) {
            return;
        }
        t.state = State::Ready;
        // A task still on its CPU (it hasn't switched away yet) is requeued by that CPU.
        if t.running_on.is_some() {
            return;
        }
        let cpu = t.last_cpu;
        enqueue(s, id, cpu)
    };
    kick(target);
}

/// The idle loop run by each CPU's boot context once everything is up.
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
    /// CPU time in milliseconds.
    pub cpu_ticks: u64,
    pub pid: u32,
    pub cpu: Option<usize>,
    pub idle: bool,
}

pub fn list() -> Vec<TaskInfo> {
    SCHED
        .lock()
        .as_ref()
        .map(|s| {
            let now = time::now_ns();
            s.tasks
                .iter()
                .map(|(&id, t)| {
                    // Include the running slice so CPU time moves smoothly.
                    let running = t.running_on.map(|c| now.saturating_sub(s.cpus[c].slice_start)).unwrap_or(0);
                    TaskInfo {
                        id,
                        name: t.name.clone(),
                        state: t.state,
                        cpu_ticks: (t.cpu_ns + running) / 1_000_000,
                        pid: t.pid,
                        cpu: t.running_on,
                        idle: t.idle,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// (busy, idle) milliseconds of every CPU that has joined.
pub fn cpu_times() -> Vec<(u64, u64)> {
    SCHED
        .lock()
        .as_ref()
        .map(|s| {
            let now = time::now_ns();
            s.cpus
                .iter()
                .map(|c| {
                    let run = now.saturating_sub(c.slice_start);
                    let (b, i) =
                        if c.current == c.idle { (c.busy_ns, c.idle_ns + run) } else { (c.busy_ns + run, c.idle_ns) };
                    (b / 1_000_000, i / 1_000_000)
                })
                .collect()
        })
        .unwrap_or_default()
}
