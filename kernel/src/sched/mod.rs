//! The scheduler: preemptive, per-CPU run queues, for any number of CPUs.
//!
//! Every thread of execution is a task: kernel threads (pid 0) and the threads
//! of user processes. Each CPU has its own run queue and an idle task (the
//! context that brought the CPU up). A CPU whose queue runs dry steals from
//! the busiest one; a task woken for an idle CPU gets there by IPI (unless
//! that CPU is polling for work, see `power::idle`). There is no periodic
//! tick: a CPU's timer is armed for its next timer (`crate::timer`; every
//! sleep and timeout is one) and, only while other tasks wait for it, for
//! the end of the running task's `QUANTUM_MS` slice.
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
use crate::arch::interrupts;

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
    _stack: Option<crate::mm::kstack::KStack>,
    cpu_ns: u64,
    /// Owning process (0 = kernel).
    pid: u32,
    /// Page table root to run with.
    cr3: u64,
    /// Top of this task's kernel stack (for syscalls and ring-3 interrupts).
    kstack_top: u64,
    /// SIMD registers of user tasks (kernel threads never use them).
    fpu: Option<Box<FpuState>>,
    /// The thread pointer of a user task (`IA32_FS_BASE`: Linux TLS).
    fs_base: u64,
    last_cpu: usize,
    /// The CPU it is scheduled on right now, if any.
    running_on: Option<usize>,
    /// Its stack is in use by a CPU (cleared only once that CPU has switched away).
    on_cpu: AtomicBool,
    /// Idle tasks never go on a queue.
    idle: bool,
    /// Runs only on CPU 0 (suspend: firmware resumes on the boot CPU).
    bsp_only: bool,
    /// The timer that ends its sleep, while it sleeps with a timeout.
    timer: Option<crate::timer::Handle>,
    /// Counts its sleeps, so a timer that fires late can't end a later one.
    sleep_seq: u64,
    /// Sleeps that ended by timeout (the timer wakeups it causes).
    timer_wakeups: u64,
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
/// The application processors are being parked (before sleep): they run
/// nothing but their idle task, which halts.
static PARKING: AtomicBool = AtomicBool::new(false);
static PARKED: AtomicU64 = AtomicU64::new(0);
/// Per CPU: when the running task's slice ends (ns), if other tasks wait
/// for that CPU; 0 if none do (no preemption interrupt is needed then).
static TICK_AT: [AtomicU64; percpu::MAX_CPUS] = [const { AtomicU64::new(0) }; percpu::MAX_CPUS];
/// Per CPU: the `on_cpu` flag of the task it just switched away from, for
/// [`finish_switch`] to clear without taking the lock (0: that task exited,
/// and is freed under the lock).
static PREV_ON_CPU: [AtomicU64; percpu::MAX_CPUS] = [const { AtomicU64::new(0) }; percpu::MAX_CPUS];

fn new_task(
    name: &str,
    rsp: u64,
    stack: Option<crate::mm::kstack::KStack>,
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
        fs_base: 0,
        last_cpu: cpu,
        running_on: None,
        on_cpu: AtomicBool::new(false),
        idle: false,
        bsp_only: false,
        timer: None,
        sleep_seq: 0,
        timer_wakeups: 0,
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
    // A CPU restarted after sleep: its old idle context is gone.
    let old = core::mem::replace(&mut c.idle, id);
    c.current = id;
    c.prev = None;
    PREV_ON_CPU[cpu].store(0, Ordering::Relaxed);
    c.slice_start = time::now_ns();
    if old != id && old != 0 && s.tasks.get(&old).is_some_and(|t| t.idle) {
        s.tasks.remove(&old);
    }
    TICK_AT[cpu].store(0, Ordering::Relaxed);
    let pc = &percpu::CPUS[cpu];
    pc.idle.store(true, Ordering::Relaxed);
    pc.task.store(id, Ordering::Relaxed);
    pc.pid.store(0, Ordering::Relaxed);
    pc.simd.store(false, Ordering::Relaxed);
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

/// The CPU with the least work (queued tasks, plus one if busy); among
/// equals this one, which is awake and runs it when it next yields.
fn least_loaded(s: &Scheduler) -> usize {
    let me = percpu::cpu_id();
    percpu::online()
        .filter(|&c| c < s.cpus.len())
        .min_by_key(|&c| (s.cpus[c].queue.len() + (s.cpus[c].current != s.cpus[c].idle) as usize, c != me))
        .unwrap_or(0)
}

/// `cpu` is running its idle task with nothing queued.
fn is_idle(s: &Scheduler, cpu: usize) -> bool {
    cpu < s.cpus.len()
        && s.cpus[cpu].current == s.cpus[cpu].idle
        && s.cpus[cpu].queue.is_empty()
        && percpu::CPUS[cpu].online.load(Ordering::Acquire)
        && !(cpu != 0 && PARKING.load(Ordering::Acquire))
}

/// An idle CPU other than `except` to run more work, preferring one that is
/// awake (polling for work, so it needs no interrupt) over one asleep.
fn find_idle(s: &Scheduler, except: usize) -> Option<usize> {
    let mut asleep = None;
    for c in percpu::online().filter(|&c| c != except && is_idle(s, c)) {
        if crate::power::idle::polling(c) {
            return Some(c);
        }
        asleep = asleep.or(Some(c));
    }
    asleep
}

/// Queues a ready task, preferably on `cpu` (where it last ran); returns the
/// CPU to kick, if any.
///
/// Placement avoids waking sleeping CPUs: a task goes to its own CPU if
/// that is idle; else to this CPU if it is idle (it is awake already: this
/// is an interrupt handler or the idle loop, and runs the task next); else
/// to another idle CPU, awake ones first; else it waits on its own CPU.
fn enqueue(s: &mut Scheduler, id: TaskId, mut cpu: usize) -> Option<usize> {
    let bsp_only = s.tasks.get(&id).is_some_and(|t| t.bsp_only);
    if bsp_only || cpu >= s.cpus.len() || !percpu::CPUS[cpu].online.load(Ordering::Acquire) {
        cpu = 0;
    }
    let me = percpu::cpu_id();
    if !bsp_only && !is_idle(s, cpu) {
        if is_idle(s, me) {
            cpu = me;
        } else if let Some(idle) = find_idle(s, cpu) {
            cpu = idle;
        }
    }
    s.cpus[cpu].queue.push_back(id);
    crate::power::idle::set_need_resched(cpu);
    let c = &s.cpus[cpu];
    if c.current != c.idle {
        // A busy CPU must now end its task's slice on time.
        if TICK_AT[cpu].load(Ordering::Relaxed) == 0 {
            TICK_AT[cpu].store(c.slice_start + QUANTUM_MS * 1_000_000, Ordering::Relaxed);
            if cpu == me {
                crate::timer::reprogram();
                return None;
            }
            return Some(cpu); // to arm its timer
        }
        return None;
    }
    (cpu != me).then_some(cpu)
}

/// Gets `cpu` to look at its queue: an IPI, unless it is idle and polling
/// for work (then the flag [`enqueue`] set is enough).
fn kick(cpu: Option<usize>) {
    if let Some(c) = cpu {
        crate::power::idle::set_need_resched(c);
        if crate::power::idle::polling(c) {
            crate::power::idle::ipi_avoided(c);
            return;
        }
        crate::arch::irq::send_ipi(c, crate::arch::irq::Ipi::Reschedule);
    }
}

/// When `cpu`'s timer must end the running task's slice (`u64::MAX`: no
/// other task waits, so never).
pub fn tick_deadline(cpu: usize) -> u64 {
    match TICK_AT.get(cpu).map_or(0, |t| t.load(Ordering::Relaxed)) {
        0 => u64::MAX,
        t => t,
    }
}

/// Records whether `cpu` needs a preemption interrupt: only while it runs a
/// task and others are queued behind it.
fn set_tick(s: &Scheduler, cpu: usize) {
    let c = &s.cpus[cpu];
    let at = if c.current != c.idle && !c.queue.is_empty() { c.slice_start + QUANTUM_MS * 1_000_000 } else { 0 };
    TICK_AT[cpu].store(at, Ordering::Relaxed);
}

/// Spawns a kernel thread that only ever runs on CPU 0.
pub fn spawn_on_bsp(name: &str, f: fn()) -> TaskId {
    fn call(f: u64) {
        let f: fn() = unsafe { core::mem::transmute(f as usize) };
        f();
    }
    spawn_inner(name, call, f as usize as u64, 0, vmm::kernel_pml4(), true, false, 0)
}

/// Spawns a task running `f(arg)` in process `pid` with page tables `cr3`.
pub fn spawn_task(name: &str, f: fn(u64), arg: u64, pid: u32, cr3: u64) -> TaskId {
    spawn_inner(name, f, arg, pid, cr3, false, false, 0)
}

/// Like [`spawn_task`], with the thread pointer (FS base) the task starts with.
pub fn spawn_task_tls(name: &str, f: fn(u64), arg: u64, pid: u32, cr3: u64, fs_base: u64) -> TaskId {
    spawn_inner(name, f, arg, pid, cr3, false, false, fs_base)
}

/// Like [`spawn_task_tls`], with the SIMD registers the task starts with
/// (a forked Linux child continues with its parent's).
pub fn spawn_user(name: &str, f: fn(u64), arg: u64, pid: u32, cr3: u64, fs_base: u64, fpu: Box<FpuState>) -> TaskId {
    spawn_inner_fpu(name, f, arg, pid, cr3, false, Some(fpu), fs_base)
}

/// Makes the running task use page tables `cr3` from now on (Linux `exec`).
pub fn switch_address_space(cr3: u64) {
    interrupts::without_interrupts(|| {
        let cpu = percpu::cpu_id();
        if let Some(s) = SCHED.lock().as_mut() {
            let cur = s.cpus[cpu].current;
            if let Some(t) = s.tasks.get_mut(&cur) {
                t.cr3 = cr3;
            }
        }
        unsafe { vmm::load(cr3) };
        percpu::CPUS[cpu].cr3.store(cr3, Ordering::Relaxed);
    });
}

/// Renames the running task (shown in Pulse).
pub fn rename_current(name: &str) {
    interrupts::without_interrupts(|| {
        if let Some(s) = SCHED.lock().as_mut() {
            let cur = s.cpus[percpu::cpu_id()].current;
            if let Some(t) = s.tasks.get_mut(&cur) {
                t.name = String::from(name);
            }
        }
    });
}

/// Sets the running task's thread pointer (Linux `arch_prctl(ARCH_SET_FS)`).
pub fn set_fs_base(base: u64) {
    interrupts::without_interrupts(|| {
        if let Some(s) = SCHED.lock().as_mut() {
            let cur = s.cpus[percpu::cpu_id()].current;
            if let Some(t) = s.tasks.get_mut(&cur) {
                t.fs_base = base;
            }
        }
        write_fs_base(base);
    });
}

/// The running task's thread pointer.
pub fn fs_base() -> u64 {
    interrupts::without_interrupts(|| {
        SCHED
            .lock()
            .as_ref()
            .and_then(|s| s.tasks.get(&s.cpus[percpu::cpu_id()].current).map(|t| t.fs_base))
            .unwrap_or(0)
    })
}

fn write_fs_base(base: u64) {
    crate::arch::cpu::set_thread_pointer(base);
}

/// Spawns a kernel thread that keeps its own SIMD registers across switches,
/// so it may run SSE2 code (the compositor's drawing).
pub fn spawn_simd(name: &str, f: fn()) -> TaskId {
    fn call(f: u64) {
        let f: fn() = unsafe { core::mem::transmute(f as usize) };
        f();
    }
    let id = spawn_inner(name, call, f as usize as u64, 0, vmm::kernel_pml4(), false, true, 0);
    log!("sched", "spawned task {} '{}' (with SIMD state)", id, name);
    id
}

#[allow(clippy::too_many_arguments)] // the task's whole starting state
fn spawn_inner(
    name: &str,
    f: fn(u64),
    arg: u64,
    pid: u32,
    cr3: u64,
    bsp_only: bool,
    simd: bool,
    fs_base: u64,
) -> TaskId {
    let fpu = simd.then(|| Box::new(FpuState::new()));
    spawn_inner_fpu(name, f, arg, pid, cr3, bsp_only, fpu, fs_base)
}

#[allow(clippy::too_many_arguments)] // the task's whole starting state
fn spawn_inner_fpu(
    name: &str,
    f: fn(u64),
    arg: u64,
    pid: u32,
    cr3: u64,
    bsp_only: bool,
    fpu: Option<Box<FpuState>>,
    fs_base: u64,
) -> TaskId {
    // 128 KiB with an unmapped guard area below (see `mm::kstack`).
    let stack = crate::mm::kstack::KStack::new();
    let top = stack.top() & !0xF;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let rsp = switch::init_stack(top, f as usize as u64, arg);
    let target = interrupts::without_interrupts(|| {
        let mut guard = SCHED.lock();
        let s = guard.as_mut().unwrap();
        let cpu = if bsp_only { 0 } else { least_loaded(s) };
        let mut t = new_task(name, rsp, Some(stack), pid, cr3, top, cpu);
        t.bsp_only = bsp_only;
        t.fs_base = fs_base;
        if fpu.is_some() {
            t.fpu = fpu;
        }
        s.tasks.insert(id, t);
        enqueue(s, id, cpu)
    });
    kick(target);
    id
}

/// First code run by a new task (called from the asm trampoline).
pub extern "C" fn task_entry(f: u64, arg: u64) -> ! {
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

/// The process task `id` belongs to, if it exists.
pub fn task_pid(id: TaskId) -> Option<u32> {
    interrupts::without_interrupts(|| SCHED.lock().as_ref().and_then(|s| s.tasks.get(&id).map(|t| t.pid)))
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

/// Makes task `id`, if it is running on another CPU, leave user mode
/// briefly (a reschedule IPI), so it notices a signal at once.
pub fn interrupt(id: TaskId) {
    let cpu = SCHED.lock().as_ref().and_then(|s| s.tasks.get(&id).and_then(|t| t.running_on));
    interrupt_cpus(cpu.into_iter());
}

/// [`interrupt`] for every task of process `pid`.
pub fn interrupt_process(pid: u32) {
    let cpus: Vec<usize> = SCHED
        .lock()
        .as_ref()
        .map(|s| s.tasks.values().filter(|t| t.pid == pid).filter_map(|t| t.running_on).collect())
        .unwrap_or_default();
    interrupt_cpus(cpus.into_iter());
}

fn interrupt_cpus(cpus: impl Iterator<Item = usize>) {
    let me = percpu::cpu_id();
    for c in cpus.filter(|&c| c != me) {
        kick(Some(c));
    }
}

/// Takes the next task for `cpu`: its own queue first, then the busiest other queue.
fn pick_next(s: &mut Scheduler, cpu: usize) -> Option<TaskId> {
    if cpu != 0 && PARKING.load(Ordering::Acquire) {
        return None;
    }
    while let Some(id) = s.cpus[cpu].queue.pop_front() {
        if s.tasks.get(&id).is_some_and(|t| t.state == State::Ready && t.running_on.is_none()) {
            return Some(id);
        }
    }
    // Only from CPUs busy with another task: one that is idle has been
    // kicked and runs its queue as soon as it wakes (stealing from it only
    // races its wakeup, and leaves it with nothing).
    let victim = (0..s.cpus.len())
        .filter(|&c| c != cpu && s.cpus[c].current != s.cpus[c].idle)
        .max_by_key(|&c| s.cpus[c].queue.len())?;
    let q = &mut s.cpus[victim].queue;
    let pos = q.iter().position(|id| {
        s.tasks.get(id).is_some_and(|t| t.state == State::Ready && t.running_on.is_none() && (cpu == 0 || !t.bsp_only))
    })?;
    q.remove(pos)
}

/// Picks the next task for this CPU and switches to it. Interrupts must be off.
///
/// Only the choice is made under the scheduler's lock, which every CPU
/// takes: saving and loading registers and switching page tables (slow,
/// especially in an emulator) come after it.
/// Self-tests: when nothing was printed for 60 s while tasks keep being
/// switched (a livelock, which no timeout reports), every task's stack
/// goes to the log, once a minute at most.
#[cfg(feature = "ktest")]
fn hang_report(now: u64) {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let quiet = now.saturating_sub(crate::drivers::serial::last_output_ns());
    let last = LAST.load(Ordering::Relaxed);
    if quiet > 60_000_000_000
        && now > last + 60_000_000_000
        && LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    {
        crate::kprintln!("\nno output for {} s; tasks:", quiet / 1_000_000_000);
        dump_stacks();
        // The running task's own callers (it is the one switching).
        let mut rbp = crate::arch::cpu::frame_pointer();
        crate::kprint!("running task {}:", current_id());
        for _ in 0..24 {
            if rbp == 0 || !rbp.is_multiple_of(8) {
                break;
            }
            let (next, ret) = unsafe { (*(rbp as *const u64), *((rbp + 8) as *const u64)) };
            crate::kprint!(" {ret:#x}");
            if next <= rbp || next - rbp > 1 << 20 {
                break;
            }
            rbp = next;
        }
        crate::kprintln!();
    }
}

fn schedule() {
    if !STARTED.load(Ordering::Acquire) {
        return;
    }
    let cpu = percpu::cpu_id();
    let now = time::now_ns();
    #[cfg(feature = "ktest")]
    hang_report(now);
    let (save, load, next_flag, switch_to, kick_idle) = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else { return };
        if cpu >= s.cpus.len() {
            return;
        }
        crate::power::idle::clear_need_resched(cpu);
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
        // A parking AP gives up even a runnable task (it moves to CPU 0).
        let parking = cpu != 0 && PARKING.load(Ordering::Acquire);
        let next_id = match pick_next(s, cpu) {
            Some(id) => id,
            None if requeue && !parking => cur_id,
            None => s.cpus[cpu].idle,
        };
        if next_id == cur_id {
            s.tasks.get_mut(&cur_id).unwrap().state = State::Running;
            set_tick(s, cpu);
            crate::timer::reprogram();
            return;
        }
        let mut kick_idle = None;
        if requeue {
            // While the APs park, their tasks move to the boot CPU. A task
            // that had to give way moves to an idle CPU if there is one
            // (with no periodic tick, idle CPUs don't come looking).
            let bsp_only = s.tasks.get(&cur_id).is_some_and(|t| t.bsp_only);
            let home = if parking {
                0
            } else if let Some(idle) = find_idle(s, cpu).filter(|_| !bsp_only) {
                kick_idle = Some(idle);
                idle
            } else {
                cpu
            };
            s.cpus[home].queue.push_back(cur_id);
        }
        // Record the switch. The tasks' boxes stay put while their stacks
        // are in use (`on_cpu`), so pointers into them outlive the lock.
        let (cur_cr3, save, save_fpu) = {
            let cur = s.tasks.get_mut(&cur_id).unwrap();
            cur.running_on = None;
            let dead = cur.state == State::Dead;
            let flag = if dead { 0 } else { &cur.on_cpu as *const AtomicBool as u64 };
            PREV_ON_CPU[cpu].store(flag, Ordering::Relaxed);
            s.cpus[cpu].prev = dead.then_some(cur_id);
            let fpu = cur.fpu.as_mut().filter(|_| !dead).map(|f| &mut **f as *mut FpuState);
            (cur.cr3, &mut cur.rsp as *mut u64, fpu)
        };
        let next = s.tasks.get_mut(&next_id).unwrap();
        next.state = State::Running;
        next.running_on = Some(cpu);
        next.last_cpu = cpu;
        let switch_to = SwitchTo {
            save_fpu,
            restore_fpu: next.fpu.as_ref().map(|f| &**f as *const FpuState),
            fs_base: (next.pid != 0).then_some(next.fs_base),
            cr3: (next.cr3 != cur_cr3).then_some(next.cr3),
            kstack_top: next.kstack_top,
        };
        let pc = &percpu::CPUS[cpu];
        pc.task.store(next_id, Ordering::Relaxed);
        pc.pid.store(next.pid, Ordering::Relaxed);
        pc.cr3.store(next.cr3, Ordering::Relaxed);
        pc.idle.store(next.idle, Ordering::Relaxed);
        pc.simd.store(next.pid == 0 && next.fpu.is_some(), Ordering::Relaxed);
        // Its saved stack pointer is read only after the hand-off below: if
        // the task is still leaving another CPU, that CPU has yet to store it.
        let load = &next.rsp as *const u64;
        let flag = &next.on_cpu as *const AtomicBool;
        s.cpus[cpu].current = next_id;
        crate::telemetry::switched(next_id, cpu);
        set_tick(s, cpu);
        crate::timer::reprogram();
        (save, load, flag, switch_to, kick_idle)
        // The lock guard drops here; interrupts stay off because the caller disabled them.
    };
    kick(kick_idle);
    // The task we leave is ours until `finish_switch` on the other side.
    if let Some(f) = switch_to.save_fpu {
        unsafe { (*f).save() };
    }
    // The task may still be leaving another CPU: wait until its stack (and
    // its saved registers) are free.
    let flag = unsafe { &*next_flag };
    while flag.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }
    if let Some(f) = switch_to.restore_fpu {
        unsafe { (*f).restore() };
    }
    if let Some(base) = switch_to.fs_base {
        write_fs_base(base);
    }
    if let Some(cr3) = switch_to.cr3 {
        unsafe { vmm::load(cr3) };
    }
    if switch_to.kstack_top != 0 {
        syscall::set_kernel_stack(switch_to.kstack_top);
    }
    let load = unsafe { load.read_volatile() };
    unsafe { switch::switch(save, load) };
    finish_switch();
}

/// What [`schedule`] does after letting go of the lock.
struct SwitchTo {
    save_fpu: Option<*mut FpuState>,
    restore_fpu: Option<*const FpuState>,
    fs_base: Option<u64>,
    cr3: Option<u64>,
    kstack_top: u64,
}

/// Runs on a CPU right after it switched to a new stack: releases the task it
/// left and frees it if it had exited.
fn finish_switch() {
    let cpu = percpu::cpu_id();
    // Usually the task lives on: it may now run elsewhere.
    let flag = PREV_ON_CPU[cpu].swap(0, Ordering::Relaxed);
    if flag != 0 {
        unsafe { (*(flag as *const AtomicBool)).store(false, Ordering::Release) };
        return;
    }
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

/// After this CPU's timer interrupt (interrupts already disabled): ends the
/// running task's slice if it is over and others wait, and re-arms the timer.
pub fn on_timer_tick() {
    if !STARTED.load(Ordering::Acquire) {
        crate::timer::reprogram();
        return;
    }
    let cpu = percpu::cpu_id();
    let now = time::now_ns();
    let need = {
        let guard = SCHED.lock();
        let Some(s) = guard.as_ref() else { return };
        let Some(c) = s.cpus.get(cpu) else { return };
        let others = !c.queue.is_empty();
        let expired = now.saturating_sub(c.slice_start) >= QUANTUM_MS * 1_000_000;
        let parking = cpu != 0 && c.current != c.idle && PARKING.load(Ordering::Acquire);
        let need = (c.current == c.idle && others) || (expired && others) || parking;
        if !need {
            set_tick(s, cpu);
        }
        need
    };
    if need {
        schedule();
    } else {
        crate::timer::reprogram();
    }
}

/// Reschedule IPI: another CPU queued work for this one (or needs this CPU
/// to arm its slice timer, or to leave user mode for a signal).
pub fn on_reschedule_ipi() {
    crate::power::idle::ipi_received(percpu::cpu_id());
    on_timer_tick();
}

/// A sleep's timer fired (in the timer interrupt of the CPU it slept on).
pub fn timer_expired(id: TaskId, seq: u64) {
    let target = {
        let mut guard = SCHED.lock();
        let Some(s) = guard.as_mut() else { return };
        let Some(t) = s.tasks.get_mut(&id) else { return };
        if !matches!(t.state, State::Sleeping(_)) || t.sleep_seq != seq {
            return;
        }
        t.timer = None;
        t.timer_wakeups += 1;
        t.state = State::Ready;
        if t.running_on.is_some() {
            return;
        }
        let cpu = t.last_cpu;
        enqueue(s, id, cpu)
    };
    kick(target);
}

pub fn yield_now() {
    interrupts::without_interrupts(schedule);
}

/// Marks the running task asleep until millisecond tick `until` (or until
/// woken), with a timer on this CPU to wake it then.
fn mark_sleeping(until: u64) {
    let deadline = until.saturating_mul(1_000_000);
    if let Some(s) = SCHED.lock().as_mut() {
        let cur = s.cpus[percpu::cpu_id()].current;
        if let Some(t) = s.tasks.get_mut(&cur) {
            if !t.idle {
                t.state = State::Sleeping(until);
                t.sleep_seq += 1;
                if let Some(old) = t.timer.take() {
                    crate::timer::cancel(old);
                }
                if deadline != u64::MAX {
                    t.timer = Some(crate::timer::add(deadline, crate::timer::Action::Wake(cur, t.sleep_seq)));
                }
            }
        }
    }
}

/// The millisecond tick `ms` from now (sleeps end on millisecond
/// boundaries, so timers due close together share one interrupt).
fn tick_after(ms: u64) -> u64 {
    time::ticks().saturating_add(ms.saturating_mul(time::HZ) / 1000)
}

pub fn sleep_ms(ms: u64) {
    interrupts::without_interrupts(|| {
        mark_sleeping(tick_after(ms));
        schedule();
    });
}

/// Sleeps for at most `ms`, returning early if [`wake`] is called — unless
/// `ready()` already holds. The task is marked sleeping *before* `ready()` is
/// checked, so a wakeup from another CPU in between is never lost.
pub fn wait_until(ms: u64, ready: impl Fn() -> bool) {
    interrupts::without_interrupts(|| {
        mark_sleeping(tick_after(ms));
        if ready() {
            if let Some(s) = SCHED.lock().as_mut() {
                let cur = s.cpus[percpu::cpu_id()].current;
                if let Some(t) = s.tasks.get_mut(&cur) {
                    if matches!(t.state, State::Sleeping(_) | State::Ready) {
                        t.state = State::Running;
                    }
                    if let Some(h) = t.timer.take() {
                        crate::timer::cancel(h);
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
        if let Some(h) = t.timer.take() {
            crate::timer::cancel(h);
        }
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
        // Sleeps in an idle state until an interrupt, or returns at once if
        // work was queued here (interrupts are on afterwards).
        crate::power::idle::enter();
        yield_now();
        let cpu = percpu::cpu_id();
        if cpu != 0 && PARKING.load(Ordering::Acquire) {
            // Parked for sleep: nothing of ours is on this CPU any more.
            interrupts::disable();
            PARKED.fetch_or(1 << cpu, Ordering::AcqRel);
            loop {
                crate::arch::cpu::halt();
            }
        }
    }
}

/// Moves all work to CPU 0 and parks the other CPUs (halted, interrupts
/// off, running only their idle context). Returns once they are parked.
pub fn park_aps() -> bool {
    let aps: Vec<usize> = percpu::online().filter(|&c| c != 0).collect();
    if aps.is_empty() {
        return true;
    }
    PARKED.store(0, Ordering::Release);
    PARKING.store(true, Ordering::Release);
    interrupts::without_interrupts(|| {
        if let Some(s) = SCHED.lock().as_mut() {
            for &c in &aps {
                percpu::CPUS[c].online.store(false, Ordering::Release);
                if c < s.cpus.len() {
                    let moved: Vec<TaskId> = s.cpus[c].queue.drain(..).collect();
                    s.cpus[0].queue.extend(moved);
                }
            }
        }
    });
    for &c in &aps {
        kick(Some(c));
    }
    let want: u64 = aps.iter().map(|&c| 1u64 << c).sum();
    let deadline = time::uptime_ms() + 2000;
    while PARKED.load(Ordering::Acquire) & want != want {
        if time::uptime_ms() > deadline {
            log!("sched", "CPUs did not park (mask {:#x} of {:#x})", PARKED.load(Ordering::Acquire), want);
            return false;
        }
        yield_now();
    }
    // Their pending timers (sleeps that started there) move here.
    for &c in &aps {
        crate::timer::migrate(c, percpu::cpu_id());
        TICK_AT[c].store(0, Ordering::Relaxed);
    }
    percpu::set_count(1);
    true
}

/// After the APs were restarted: normal scheduling everywhere again.
pub fn unpark() {
    PARKING.store(false, Ordering::Release);
    PARKED.store(0, Ordering::Release);
}

/// Prints where every task that is not running stopped: its name, state and
/// the return addresses on its stack (resolve them with `addr2line -e` on the
/// kernel ELF). For the self-tests' panic report; it never waits for a lock.
pub fn dump_stacks() {
    dump_stacks_of(|_| true)
}

/// [`dump_stacks`] for the tasks of the processes `which` picks (0: the kernel).
pub fn dump_stacks_of(which: impl Fn(u32) -> bool) {
    let Some(guard) = SCHED.try_lock() else {
        crate::kprintln!("(scheduler busy: no task stacks)");
        return;
    };
    let Some(s) = guard.as_ref() else { return };
    // (Allocates nothing: the panic may be the heap running out.)
    for (&id, t) in &s.tasks {
        if t.idle || !which(t.pid) {
            continue;
        }
        if let Some(cpu) = t.running_on {
            crate::kprintln!("task {} '{}' (pid {}) running on CPU {}", id, t.name, t.pid, cpu);
            continue;
        }
        let Some(stack) = t._stack.as_ref() else { continue };
        let (lo, hi) = (stack.bottom(), stack.top());
        let read = |a: u64| -> Option<u64> { (a >= lo && a + 8 <= hi).then(|| unsafe { (a as *const u64).read() }) };
        // aurora_context_switch pushed rbp, rbx, r12–r15 below its return address.
        let mut frames = [0u64; 20];
        let mut n = 0;
        if let Some(ret) = read(t.rsp + switch::SAVED_RETURN) {
            frames[0] = ret;
            n = 1;
        }
        let mut rbp = read(t.rsp + switch::SAVED_FRAME_POINTER).unwrap_or(0);
        while n < frames.len() {
            let (Some(next), Some(ret)) = (read(rbp), read(rbp + 8)) else { break };
            frames[n] = ret;
            n += 1;
            if next <= rbp {
                break;
            }
            rbp = next;
        }
        crate::kprint!("task {} '{}' (pid {}) {:?}:", id, t.name, t.pid, t.state);
        for f in &frames[..n] {
            crate::kprint!(" {:#x}", f);
        }
        crate::kprintln!();
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

/// (tasks, of them user threads, of them sleeping), without waiting for
/// the lock (zeros if it is busy).
pub fn task_counts() -> (u64, u64, u64) {
    interrupts::without_interrupts(|| {
        let Some(g) = SCHED.try_lock() else { return (0, 0, 0) };
        let Some(s) = g.as_ref() else { return (0, 0, 0) };
        let user = s.tasks.values().filter(|t| t.pid != 0).count() as u64;
        let sleeping = s.tasks.values().filter(|t| t.pid != 0 && matches!(t.state, State::Sleeping(_))).count() as u64;
        (s.tasks.len() as u64, user, sleeping)
    })
}

/// Tasks by the timer wakeups their sleeps caused (timeouts that ran out),
/// most first: what keeps an idle system from being quiet.
pub fn timer_wakeups() -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = SCHED
        .lock()
        .as_ref()
        .map(|s| s.tasks.values().filter(|t| t.timer_wakeups > 0).map(|t| (t.name.clone(), t.timer_wakeups)).collect())
        .unwrap_or_default();
    for (name, n) in crate::timer::call_counts() {
        v.push((alloc::format!("timer:{name}"), n));
    }
    v.sort_by_key(|a| core::cmp::Reverse(a.1));
    v
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
