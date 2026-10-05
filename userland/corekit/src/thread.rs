//! Threads: extra flows of execution sharing the process's memory.
//!
//! ```ignore
//! let h = corekit::thread::spawn(|| heavy_work());
//! let result = h.join();
//! ```

use crate::abi::nr;
use crate::sync::{futex_wait, Mutex};
use crate::sys::call;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

const STACK_SIZE: u64 = 256 * 1024;

struct Packet<T> {
    /// Written to 1 by the kernel after the thread has left its stack.
    done: AtomicU32,
    result: Mutex<Option<T>>,
    stack: u64,
}

pub struct JoinHandle<T> {
    packet: Arc<Packet<T>>,
    tid: u64,
}

struct Start<T> {
    f: Box<dyn FnOnce() -> T + Send>,
    packet: Arc<Packet<T>>,
}

extern "C" fn thread_main<T: Send + 'static>(raw: u64) -> ! {
    let start = unsafe { Box::from_raw(raw as *mut Start<T>) };
    let Start { f, packet } = *start;
    let value = f();
    *packet.result.lock() = Some(value);
    let done = packet.done.as_ptr() as u64;
    // The joiner keeps `packet` alive until `done` is set, which the kernel
    // does only once this thread no longer runs on its stack.
    drop(packet);
    let _ = call(nr::THREAD_EXIT, &[done]);
    unreachable!()
}

/// Runs `f` on a new thread.
pub fn spawn<F, T>(f: F) -> crate::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let stack = call(nr::MMAP, &[STACK_SIZE])?;
    let packet = Arc::new(Packet { done: AtomicU32::new(0), result: Mutex::new(None), stack });
    let start = Box::new(Start::<T> { f: Box::new(f), packet: packet.clone() });
    let raw = Box::into_raw(start);
    match call(nr::THREAD_SPAWN, &[thread_main::<T> as *const () as u64, stack + STACK_SIZE, raw as u64]) {
        Ok(tid) => Ok(JoinHandle { packet, tid }),
        Err(e) => {
            drop(unsafe { Box::from_raw(raw) });
            let _ = call(nr::MUNMAP, &[stack, STACK_SIZE]);
            Err(e)
        }
    }
}

impl<T> JoinHandle<T> {
    pub fn id(&self) -> u64 {
        self.tid
    }

    pub fn is_finished(&self) -> bool {
        self.packet.done.load(Ordering::Acquire) != 0
    }

    /// Waits for the thread and returns its result.
    pub fn join(self) -> T {
        while self.packet.done.load(Ordering::Acquire) == 0 {
            futex_wait(&self.packet.done, 0, None);
        }
        let _ = call(nr::MUNMAP, &[self.packet.stack, STACK_SIZE]);
        self.packet.result.lock().take().expect("thread produced no result")
    }
}

/// Ends the calling thread.
pub fn exit() -> ! {
    let _ = call(nr::THREAD_EXIT, &[0]);
    unreachable!()
}
