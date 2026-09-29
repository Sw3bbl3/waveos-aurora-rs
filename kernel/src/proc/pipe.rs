//! Pipes: bounded byte queues connecting a writer handle to a reader handle.

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use aurora_abi::err::*;

const CAPACITY: usize = 64 * 1024;

struct Inner {
    buf: VecDeque<u8>,
    readers: u32,
    writers: u32,
    /// Tasks blocked in read/write, woken on state changes.
    waiting: [Option<TaskId>; 2],
}

pub struct Pipe {
    inner: IrqMutex<Inner>,
}

impl Pipe {
    pub fn new() -> Pipe {
        Pipe { inner: IrqMutex::new(Inner { buf: VecDeque::new(), readers: 1, writers: 1, waiting: [None, None] }) }
    }

    fn wake_all(inner: &mut Inner) {
        for t in inner.waiting.iter_mut().filter_map(|t| t.take()) {
            sched::wake(t);
        }
    }

    pub fn read(&self, out: &mut [u8], nonblocking: bool) -> Result<usize, isize> {
        loop {
            {
                let mut g = self.inner.lock();
                if !g.buf.is_empty() {
                    let n = out.len().min(g.buf.len());
                    for (o, b) in out.iter_mut().zip(g.buf.drain(..n)) {
                        *o = b;
                    }
                    Self::wake_all(&mut g);
                    return Ok(n);
                }
                if g.writers == 0 || out.is_empty() {
                    return Ok(0);
                }
                if nonblocking || super::interrupted() {
                    return Err(EAGAIN);
                }
                g.waiting[0] = Some(sched::current_id());
            }
            sched::wait_until(50, || {
                let g = self.inner.lock();
                !g.buf.is_empty() || g.writers == 0
            });
        }
    }

    pub fn write(&self, data: &[u8]) -> Result<usize, isize> {
        let mut done = 0;
        while done < data.len() {
            {
                let mut g = self.inner.lock();
                if g.readers == 0 {
                    return if done > 0 { Ok(done) } else { Err(EPIPE) };
                }
                if super::interrupted() {
                    return Err(EPIPE);
                }
                let room = CAPACITY - g.buf.len();
                if room > 0 {
                    let n = room.min(data.len() - done);
                    g.buf.extend(&data[done..done + n]);
                    done += n;
                    Self::wake_all(&mut g);
                    continue;
                }
                g.waiting[1] = Some(sched::current_id());
            }
            sched::wait_until(50, || {
                let g = self.inner.lock();
                g.buf.len() < CAPACITY || g.readers == 0
            });
        }
        Ok(done)
    }

    pub fn close_reader(&self) {
        let mut g = self.inner.lock();
        g.readers = g.readers.saturating_sub(1);
        Self::wake_all(&mut g);
    }

    pub fn close_writer(&self) {
        let mut g = self.inner.lock();
        g.writers = g.writers.saturating_sub(1);
        Self::wake_all(&mut g);
    }
}
