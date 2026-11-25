//! Linux system calls about time, identity, limits, threads and the machine.

use crate::arch::syscall::SyscallFrame;
use crate::proc::{self, Process};
use crate::syscall::user;
use crate::{sched, time};
use aurora_abi::err::*;
use core::sync::atomic::Ordering;

/// The thread id Linux programs see: the process id for the main thread.
pub fn tid(p: &Process) -> u64 {
    let me = sched::current_id();
    if p.main_task.load(Ordering::Relaxed) == me {
        p.pid as u64
    } else {
        // Other threads: their task id, kept clear of process ids.
        me + 0x10_0000
    }
}

/// `exit`: ends the calling thread; the last one ends the process.
pub fn exit_thread(p: &Process, code: i64) -> ! {
    if let Some(l) = &p.linux {
        l.signals.thread_exited(sched::current_id());
        let addr = l.clear_tid.lock().remove(&sched::current_id()).unwrap_or(0);
        if addr != 0 {
            // Joiners wait for the thread id to become 0.
            let _ = user::write_u32(addr, 0);
            let _ = proc::futex::wake(addr, u64::MAX);
        }
    }
    if p.main_task.load(Ordering::Relaxed) == sched::current_id() {
        proc::exit_current(code & 0xFF);
    }
    proc::exit_thread();
}

/// `clone` with `CLONE_THREAD`: a new thread of this process.
pub fn clone_thread(
    p: &Process,
    f: &SyscallFrame,
    flags: u64,
    stack: u64,
    ptid: u64,
    ctid: u64,
    tls: u64,
) -> Result<u64, isize> {
    const CLONE_VM: u64 = 0x100;
    const CLONE_THREAD: u64 = 0x10000;
    const CLONE_SETTLS: u64 = 0x80000;
    const CLONE_PARENT_SETTID: u64 = 0x100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x200000;
    const CLONE_CHILD_SETTID: u64 = 0x1000000;
    if flags & (CLONE_VM | CLONE_THREAD) != CLONE_VM | CLONE_THREAD {
        return Err(ENOSYS);
    }
    if stack == 0 || stack >= aurora_abi::layout::USER_END || f.pc() >= aurora_abi::layout::USER_END {
        return Err(EINVAL);
    }
    // The child returns from the same call with 0, on its own stack.
    let mut child = *f;
    child.set_return(0);
    child.set_sp(stack);
    let fs = if flags & CLONE_SETTLS != 0 { tls } else { sched::fs_base() };
    let linux = p.linux.as_ref().ok_or(EINVAL)?;
    // The thread must not run before its bookkeeping is in place, so its
    // clear-tid word is recorded under the lock it takes when it exits.
    let mut clear = linux.clear_tid.lock();
    let set_tid = [
        if flags & CLONE_PARENT_SETTID != 0 { ptid } else { 0 },
        if flags & CLONE_CHILD_SETTID != 0 { ctid } else { 0 },
    ];
    let task = proc::spawn_linux_thread(child, fs, set_tid)?;
    linux.signals.thread_started(sched::current_id(), task);
    let tid_ = task + 0x10_0000;
    if flags & CLONE_CHILD_CLEARTID != 0 {
        clear.insert(task, ctid);
    }
    // The thread wrote its id already (before running); written here too so
    // it is there when we return, still under the lock, so that it can't
    // land after the thread has exited and cleared it (glibc uses one word
    // for both).
    for addr in set_tid.into_iter().filter(|&a| a != 0) {
        let _ = user::write_u32(addr, tid_ as u32);
    }
    drop(clear);
    Ok(tid_)
}

pub fn arch_prctl(code: u64, addr: u64) -> Result<u64, isize> {
    match code {
        // ARCH_SET_FS
        0x1002 => {
            if addr >= aurora_abi::layout::USER_END {
                return Err(EPERM);
            }
            sched::set_fs_base(addr);
            Ok(0)
        }
        // ARCH_GET_FS
        0x1003 => user::put(addr, &sched::fs_base()).map(|_| 0),
        _ => Err(EINVAL),
    }
}

pub fn prctl(p: &Process, option: u64, arg: u64) -> Result<u64, isize> {
    match option {
        // PR_SET_NAME, PR_GET_NAME
        15 => Ok(0),
        16 => {
            let mut name = [0u8; 16];
            let own = p.name();
            let n = own.len().min(15);
            name[..n].copy_from_slice(&own.as_bytes()[..n]);
            user::put(arg, &name).map(|_| 0)
        }
        // PR_SET_PDEATHSIG, PR_GET_DUMPABLE, PR_SET_DUMPABLE, PR_SET_NO_NEW_PRIVS
        1 | 3 | 4 | 38 => Ok(if option == 3 { 1 } else { 0 }),
        _ => Err(EINVAL),
    }
}

/// `futex`: waiting and waking (the bit-set forms behave like the plain
/// ones), requeueing and `WAKE_OP`. Without `FUTEX_PRIVATE_FLAG` a futex in
/// shared memory is the same for every process mapping it.
pub fn futex(addr: u64, op: u64, val: u64, timeout: u64, addr2: u64, val3: u64) -> Result<u64, isize> {
    const WAIT: u64 = 0;
    const WAKE: u64 = 1;
    const REQUEUE: u64 = 3;
    const CMP_REQUEUE: u64 = 4;
    const WAKE_OP: u64 = 5;
    const WAIT_BITSET: u64 = 9;
    const WAKE_BITSET: u64 = 10;
    const PRIVATE: u64 = 128;
    let shared = op & PRIVATE == 0;
    match op & 0x7F {
        WAIT | WAIT_BITSET => {
            let deadline = if timeout == 0 {
                u64::MAX
            } else {
                let [sec, nsec]: [i64; 2] = user::get(timeout)?;
                let t = (sec.max(0) as u64).saturating_mul(1_000_000_000).saturating_add(nsec.max(0) as u64);
                let now = time::now_ns();
                if op & 0x7F == WAIT_BITSET {
                    // An absolute deadline on CLOCK_MONOTONIC (or REALTIME).
                    let clock = if op & 256 != 0 { realtime_ns() } else { now };
                    now.saturating_add(t.saturating_sub(clock))
                } else {
                    now.saturating_add(t)
                }
            };
            match proc::futex::wait_until_ns(addr, val as u32, deadline, shared) {
                Err(EAGAIN) if proc::interrupted() => Err(EINTR),
                r => r,
            }
        }
        WAKE | WAKE_BITSET => proc::futex::wake_shared(addr, val, shared),
        // `timeout` holds the number to move, as Linux reads it.
        REQUEUE => proc::futex::requeue(addr, val, addr2, timeout, None, shared),
        CMP_REQUEUE => proc::futex::requeue(addr, val, addr2, timeout, Some(val3 as u32), shared),
        WAKE_OP => proc::futex::wake_op(addr, val, addr2, timeout, val3 as u32, shared),
        _ => Err(ENOSYS),
    }
}

/// `sched_getattr`: every thread runs under SCHED_OTHER at nice 0 (WaveOS
/// has one policy for programs), so that is what the `struct sched_attr`
/// says (VER1, 56 bytes; a smaller `size` gets its first part).
pub fn sched_getattr(p: &proc::Process, pid: u64, attr: u64, size: u64, flags: u64) -> Result<u64, isize> {
    const VER1: usize = 56;
    if attr == 0 || flags != 0 || (size as usize) < 48 {
        return Err(EINVAL);
    }
    if pid != 0 && pid as u32 != p.pid && super::signal::task_of(p, pid).is_none() {
        return Err(ESRCH);
    }
    let n = (size as usize).min(VER1);
    let mut a = [0u8; VER1];
    a[0..4].copy_from_slice(&(n as u32).to_le_bytes()); // size; policy, flags, nice, priority, runtime… 0
    a[52..56].copy_from_slice(&1024u32.to_le_bytes()); // sched_util_max: the whole CPU
    user::put_bytes(attr, &a[..n]).map(|_| 0)
}

/// `get_mempolicy`: one memory node and the default policy. With
/// MPOL_F_MEMS_ALLOWED the node mask is node 0; with MPOL_F_NODE |
/// MPOL_F_ADDR `mode` is the node of the address (0).
pub fn get_mempolicy(mode: u64, nodemask: u64, maxnode: u64, flags: u64) -> Result<u64, isize> {
    const MEMS_ALLOWED: u64 = 4;
    if flags & !7 != 0 {
        return Err(EINVAL);
    }
    if mode != 0 {
        user::put(mode, &0i32)?; // MPOL_DEFAULT, or node 0
    }
    if nodemask != 0 {
        if maxnode == 0 {
            return Err(EINVAL);
        }
        let words = maxnode.div_ceil(64).min(16);
        for i in 0..words {
            let bits: u64 = if i == 0 && flags & MEMS_ALLOWED != 0 { 1 } else { 0 };
            user::put(nodemask + i * 8, &bits)?;
        }
    }
    Ok(0)
}

pub fn sched_getaffinity(len: u64, ptr: u64) -> Result<u64, isize> {
    let n = crate::arch::percpu::count() as u64;
    let mask: u64 = if n >= 64 { u64::MAX } else { (1 << n) - 1 };
    if len < 8 {
        return Err(EINVAL);
    }
    user::put(ptr, &mask).map(|_| 8)
}

pub fn uname(ptr: u64) -> Result<u64, isize> {
    let mut b = [0u8; 65 * 6];
    let machine = super::abi::native::MACHINE;
    let fields = ["Linux", "aurora", "6.12.0-waveos", "#1 SMP WaveOS Aurora", machine, "(none)"];
    for (i, f) in fields.iter().enumerate() {
        b[i * 65..i * 65 + f.len()].copy_from_slice(f.as_bytes());
    }
    user::put(ptr, &b).map(|_| 0)
}

/// `sysinfo`: uptime, memory (free, and the caches as "buffers"), swap,
/// and the number of processes, in bytes (`mem_unit` 1).
pub fn sysinfo(ptr: u64) -> Result<u64, isize> {
    let m = crate::mm::kswapd::meminfo();
    let mut b = [0u8; 112];
    let mut w = |off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    w(0, time::uptime_ms() / 1000);
    w(32, m.total);
    w(40, m.free);
    w(56, crate::fs::cache::held_blocks() * 4096);
    w(64, m.swap_total);
    w(72, m.swap_total.saturating_sub(m.swap_used));
    b[80..82].copy_from_slice(&(proc::all().len() as u16).to_le_bytes());
    b[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit
    user::put(ptr, &b).map(|_| 0)
}

/// `prlimit64`/`getrlimit`: generous limits, changes accepted and ignored.
pub fn prlimit(resource: u64, _new: u64, old: u64) -> Result<u64, isize> {
    if old != 0 {
        let v: u64 = match resource {
            3 => 8 << 20,                     // RLIMIT_STACK
            7 => crate::proc::MAX_FDS as u64, // RLIMIT_NOFILE
            _ => u64::MAX,                    // RLIM_INFINITY
        };
        user::put(old, &[v, v])?;
    }
    Ok(0)
}

pub fn getrusage(ptr: u64) -> Result<u64, isize> {
    user::put(ptr, &[0u8; 144]).map(|_| 0)
}

pub fn times(ptr: u64) -> Result<u64, isize> {
    if ptr != 0 {
        user::put(ptr, &[0u64; 4])?;
    }
    Ok(time::uptime_ms() / 10)
}

pub fn getrandom(ptr: u64, len: u64) -> Result<u64, isize> {
    let mut out = user::slice_mut(ptr, len)?;
    crate::random::fill(&mut out);
    let n = out.len();
    out.commit(n)?;
    Ok(n as u64)
}

/// UTC real time. Unlike MONOTONIC, this follows clock corrections at once
/// (and the vDSO computes it the same way; see `time::realtime_ns`).
pub fn realtime_ns() -> u64 {
    time::realtime_ns()
}

fn clock_ns(clock: u64) -> Result<u64, isize> {
    match clock {
        // REALTIME, REALTIME_COARSE, TAI
        0 | 5 | 11 => Ok(realtime_ns()),
        // MONOTONIC (raw, coarse), BOOTTIME, and the CPU-time clocks (approximated).
        1 | 2 | 3 | 4 | 6 | 7 => Ok(time::now_ns()),
        c if (c as i64) < 0 => Ok(time::now_ns()), // per-thread CPU clocks
        _ => Err(EINVAL),
    }
}

fn put_timespec(ptr: u64, ns: u64) -> Result<(), isize> {
    user::put(ptr, &[(ns / 1_000_000_000) as i64, (ns % 1_000_000_000) as i64])
}

pub fn clock_gettime(clock: u64, ptr: u64) -> Result<u64, isize> {
    let ns = clock_ns(clock)?;
    put_timespec(ptr, ns).map(|_| 0)
}

pub fn clock_getres(ptr: u64) -> Result<u64, isize> {
    if ptr != 0 {
        put_timespec(ptr, 1)?;
    }
    Ok(0)
}

pub fn gettimeofday(ptr: u64) -> Result<u64, isize> {
    if ptr != 0 {
        let ns = realtime_ns();
        user::put(ptr, &[(ns / 1_000_000_000) as i64, ((ns / 1000) % 1_000_000) as i64])?;
    }
    Ok(0)
}

pub fn time(ptr: u64) -> Result<u64, isize> {
    let secs = realtime_ns() / 1_000_000_000;
    if ptr != 0 {
        user::put(ptr, &secs)?;
    }
    Ok(secs)
}

/// `nanosleep` and `clock_nanosleep` (relative, or until an absolute time).
pub fn nanosleep(req: u64, rem: u64, absolute: bool, clock: u64) -> Result<u64, isize> {
    let [sec, nsec]: [i64; 2] = user::get(req)?;
    if !(0..1_000_000_000).contains(&nsec) || sec < 0 {
        return Err(EINVAL);
    }
    let mut ns = sec as u64 * 1_000_000_000 + nsec as u64;
    if absolute {
        ns = ns.saturating_sub(clock_ns(clock)?);
    }
    let until = time::now_ns() + ns;
    loop {
        let now = time::now_ns();
        if now >= until {
            return Ok(0);
        }
        if proc::interrupted() {
            if rem != 0 && !absolute {
                put_timespec(rem, until - now)?;
            }
            return Err(EINTR);
        }
        // To the nanosecond (a frame paced by `nanosleep` wakes when it
        // asked, and the last part of a sleep isn't spent spinning).
        sched::wait_until_ns(until, proc::interrupted);
    }
}
