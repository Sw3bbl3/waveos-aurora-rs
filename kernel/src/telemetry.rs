//! Live telemetry for the System Explorer (`cargo xtask run --monitor`).
//!
//! When a second UART (COM2) is present, the kernel streams newline-delimited
//! JSON on it: boot stages, notable events (processes, windows, mounts,
//! devices) and a snapshot four times a second (tasks and CPU time, the
//! scheduler's recent decisions, memory, syscall and interrupt counters, disk
//! I/O, filesystems, windows). Without COM2 every hook is a relaxed atomic
//! increment at most.

use crate::drivers::serial::Serial;
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static PORT: IrqMutex<Serial> = IrqMutex::new(Serial::new(0x2F8));

// ------------------------------------------------------------- counters

const SYSCALLS: usize = aurora_abi::nr::COUNT;
static SYSCALL_COUNT: [AtomicU64; SYSCALLS] = [const { AtomicU64::new(0) }; SYSCALLS];
static IRQ_TIMER: AtomicU64 = AtomicU64::new(0);
static IRQ_KEYBOARD: AtomicU64 = AtomicU64::new(0);
static IRQ_MOUSE: AtomicU64 = AtomicU64::new(0);
static SWITCHES: AtomicU64 = AtomicU64::new(0);
pub static FRAMES: AtomicU64 = AtomicU64::new(0);
pub static PIXELS: AtomicU64 = AtomicU64::new(0);
static STAGE: IrqMutex<&'static str> = IrqMutex::new("firmware");

/// Recent scheduling decisions: (uptime ms, task id), drained by each snapshot.
const SWITCH_LOG: usize = 512;
struct SwitchLog {
    entries: [(u32, u32); SWITCH_LOG],
    len: usize,
    dropped: u64,
}
static SWITCH_RING: IrqMutex<SwitchLog> =
    IrqMutex::new(SwitchLog { entries: [(0, 0); SWITCH_LOG], len: 0, dropped: 0 });

/// Totals for Activity Monitor: (interrupts, system calls, context switches).
pub fn totals() -> (u64, u64, u64) {
    let irqs =
        IRQ_TIMER.load(Ordering::Relaxed) + IRQ_KEYBOARD.load(Ordering::Relaxed) + IRQ_MOUSE.load(Ordering::Relaxed);
    let sys = SYSCALL_COUNT.iter().map(|c| c.load(Ordering::Relaxed)).sum();
    (irqs, sys, SWITCHES.load(Ordering::Relaxed))
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[inline]
pub fn syscall(nr: usize) {
    if let Some(c) = SYSCALL_COUNT.get(nr) {
        c.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
pub enum Irq {
    Timer,
    Keyboard,
    Mouse,
}

#[inline]
pub fn irq(kind: Irq) {
    match kind {
        Irq::Timer => &IRQ_TIMER,
        Irq::Keyboard => &IRQ_KEYBOARD,
        Irq::Mouse => &IRQ_MOUSE,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// Called by the scheduler on every context switch (interrupts are off).
#[inline]
pub fn switched(to_task: u64) {
    SWITCHES.fetch_add(1, Ordering::Relaxed);
    if !enabled() {
        return;
    }
    let mut ring = SWITCH_RING.lock();
    if ring.len < SWITCH_LOG {
        let i = ring.len;
        ring.entries[i] = (crate::time::uptime_ms() as u32, to_task as u32);
        ring.len += 1;
    } else {
        ring.dropped += 1;
    }
}

// ------------------------------------------------------------ writing

/// Writes to COM2 while its lock is held, so a whole line is emitted
/// atomically without allocating (events can fire before the heap exists).
struct Line<'a>(&'a mut Serial);

impl Write for Line<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.0.write_byte(b);
        }
        Ok(())
    }
}

fn write_line(args: fmt::Arguments) {
    let mut port = PORT.lock();
    let _ = Line(&mut port).write_fmt(args);
}

/// Writes `s` as a JSON string literal (with quotes).
struct Escaped<'a>(&'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_char('"')?;
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
                c => f.write_char(c)?,
            }
        }
        f.write_char('"')
    }
}

fn json_str(s: &str) -> Escaped<'_> {
    Escaped(s)
}

/// Emits one event line: `{"ev":kind,"t":ms,<fields>}`. `fields` is pre-rendered JSON members.
fn emit(kind: &str, fields: fmt::Arguments) {
    if !enabled() {
        return;
    }
    write_line(format_args!("{{\"ev\":\"{}\",\"t\":{},{}}}\n", kind, crate::time::uptime_ms(), fields));
}

// ------------------------------------------------------------- events

/// A boot stage has begun.
pub fn stage(id: &'static str, label: &str) {
    *STAGE.lock() = id;
    emit("stage", format_args!("\"id\":{},\"label\":{}", json_str(id), json_str(label)));
}

pub fn process(what: &str, pid: u32, name: &str, detail: &str) {
    emit(
        "proc",
        format_args!(
            "\"what\":{},\"pid\":{},\"name\":{},\"detail\":{}",
            json_str(what),
            pid,
            json_str(name),
            json_str(detail)
        ),
    );
}

pub fn window(what: &str, id: u32, pid: u32, title: &str) {
    emit(
        "win",
        format_args!("\"what\":{},\"id\":{},\"pid\":{},\"title\":{}", json_str(what), id, pid, json_str(title)),
    );
}

pub fn mount(path: &str, fs: &str) {
    emit("mount", format_args!("\"path\":{},\"fs\":{}", json_str(path), json_str(fs)));
}

pub fn device(name: &str, desc: &str, bytes: u64) {
    emit("dev", format_args!("\"name\":{},\"desc\":{},\"bytes\":{}", json_str(name), json_str(desc), bytes));
}

// ----------------------------------------------------------- snapshots

pub fn init() {
    let mut port = PORT.lock();
    if !port.present() {
        return;
    }
    port.init();
    drop(port);
    ENABLED.store(true, Ordering::Relaxed);
    emit("hello", format_args!("\"version\":{},\"syscalls\":{}", json_str(crate::VERSION), SYSCALLS));
}

/// Starts the snapshot task (only when a monitor is attached).
pub fn start() {
    if enabled() {
        crate::sched::spawn("telemetry", snapshot_task);
    }
}

fn snapshot_task() {
    loop {
        let s = snapshot();
        write_line(format_args!("{}", s));
        crate::sched::sleep_ms(250);
    }
}

fn snapshot() -> String {
    use crate::sched::State;
    let mut o = String::with_capacity(4096);
    let _ = write!(o, "{{\"ev\":\"snap\",\"t\":{},\"stage\":{}", crate::time::uptime_ms(), json_str(*STAGE.lock()));

    // Tasks and CPU time.
    let tasks = crate::sched::list();
    let _ = write!(o, ",\"switches\":{},\"tasks\":[", SWITCHES.load(Ordering::Relaxed));
    for (i, t) in tasks.iter().enumerate() {
        let st = match t.state {
            State::Running => "run",
            State::Ready => "ready",
            State::Sleeping(_) => "sleep",
            State::Dead => "dead",
        };
        let _ = write!(
            o,
            "{}{{\"id\":{},\"n\":{},\"pid\":{},\"st\":\"{}\",\"cpu\":{}}}",
            if i > 0 { "," } else { "" },
            t.id,
            json_str(&t.name),
            t.pid,
            st,
            t.cpu_ticks
        );
    }
    // Scheduling decisions since the last snapshot.
    let (log, dropped): (Vec<(u32, u32)>, u64) = {
        let mut ring = SWITCH_RING.lock();
        let v = ring.entries[..ring.len].to_vec();
        ring.len = 0;
        (v, core::mem::take(&mut ring.dropped))
    };
    let _ = write!(o, "],\"sched\":[");
    for (i, (t, id)) in log.iter().enumerate() {
        let _ = write!(o, "{}[{},{}]", if i > 0 { "," } else { "" }, t, id);
    }
    let _ = write!(o, "],\"schedDropped\":{}", dropped);

    // Processes.
    let _ = write!(o, ",\"procs\":[");
    for (i, p) in crate::proc::all().iter().enumerate() {
        let _ = write!(
            o,
            "{}{{\"pid\":{},\"n\":{},\"ppid\":{},\"path\":{},\"kib\":{},\"alive\":{}}}",
            if i > 0 { "," } else { "" },
            p.pid,
            json_str(&p.name),
            p.parent,
            json_str(&p.path),
            p.resident_kib.load(Ordering::Relaxed),
            p.exit_code().is_none()
        );
    }

    // Memory.
    let m = crate::mm::stats();
    let _ = write!(
        o,
        "],\"mem\":{{\"total\":{},\"used\":{},\"heap\":{},\"heapUsed\":{}}}",
        m.total_bytes, m.used_bytes, m.heap_size, m.heap_used
    );

    // Syscalls and interrupts.
    let _ = write!(o, ",\"sys\":[");
    for (i, c) in SYSCALL_COUNT.iter().enumerate() {
        let _ = write!(o, "{}{}", if i > 0 { "," } else { "" }, c.load(Ordering::Relaxed));
    }
    let _ = write!(
        o,
        "],\"irq\":{{\"timer\":{},\"kbd\":{},\"mouse\":{}}}",
        IRQ_TIMER.load(Ordering::Relaxed),
        IRQ_KEYBOARD.load(Ordering::Relaxed),
        IRQ_MOUSE.load(Ordering::Relaxed)
    );

    // Block devices.
    let _ = write!(o, ",\"blk\":[");
    for (i, d) in crate::drivers::block::stats().iter().enumerate() {
        let _ = write!(
            o,
            "{}{{\"n\":{},\"d\":{},\"r\":{},\"w\":{},\"rb\":{},\"wb\":{},\"f\":{},\"bytes\":{}}}",
            if i > 0 { "," } else { "" },
            json_str(&d.name),
            json_str(&d.desc),
            d.reads,
            d.writes,
            d.read_bytes,
            d.write_bytes,
            d.flushes,
            d.bytes
        );
    }

    // Filesystems.
    let _ = write!(o, "],\"fs\":[");
    for (i, (path, desc, space)) in crate::fs::mounts().iter().enumerate() {
        let (total, free) = space.unwrap_or((0, 0));
        let _ = write!(
            o,
            "{}{{\"p\":{},\"d\":{},\"total\":{},\"free\":{}}}",
            if i > 0 { "," } else { "" },
            json_str(path),
            json_str(desc),
            total,
            free
        );
    }

    // Window server.
    let _ = write!(
        o,
        "],\"gui\":{{\"frames\":{},\"px\":{},\"win\":[",
        FRAMES.load(Ordering::Relaxed),
        PIXELS.load(Ordering::Relaxed)
    );
    for (i, (id, pid, title)) in crate::gui::server::windows().iter().enumerate() {
        let _ = write!(
            o,
            "{}{{\"id\":{},\"pid\":{},\"title\":{}}}",
            if i > 0 { "," } else { "" },
            id,
            pid,
            json_str(title)
        );
    }
    let _ = write!(o, "]}}}}\n");
    o
}
