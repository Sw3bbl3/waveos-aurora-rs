//! Sound input: microphones and line inputs.
//!
//! An input device ([`Input`]) delivers what it records through
//! [`captured`], already converted to the system format (48 kHz, 16-bit
//! stereo). Programs read it from capture streams (`AUDIO_CAPTURE`): each
//! stream gets its own copy, buffering up to a second before the oldest
//! samples are dropped. A device records only while some stream is open on
//! it.

use super::convert::Format;
use super::{RATE, REGISTRY};
use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

/// Samples a capture stream holds before dropping the oldest (1 s of stereo).
const CAPACITY: usize = RATE * 2;

/// A device that records.
pub trait Input: Send + Sync {
    fn describe(&self) -> String;
    fn format(&self) -> Format;
    fn usb(&self) -> bool {
        false
    }
    /// Starts recording, delivering through [`captured`] with the id the
    /// device was registered under. False if the device couldn't start.
    fn start(&self) -> bool;
    fn stop(&self);
}

/// One program's view of an input.
pub struct Capture {
    pub device: u32,
    inner: IrqMutex<Inner>,
}

struct Inner {
    buf: VecDeque<i16>,
    /// The device went away: reads return what is left, then end.
    gone: bool,
    closed: bool,
    reader: Option<TaskId>,
    dropped: u64,
}

static CAPTURES: IrqMutex<Vec<Arc<Capture>>> = IrqMutex::new(Vec::new());

impl Capture {
    /// Reads little-endian `i16` samples (whole samples), blocking until
    /// some arrive. 0 means the device is gone.
    pub fn read(&self, out: &mut [u8]) -> Result<usize, isize> {
        // Taken away in Settings: a running app stops hearing at once.
        if !crate::sandbox::allows(aurora_abi::sandbox::MICROPHONE) {
            return Err(EACCES);
        }
        let want = out.len() / 2;
        if want == 0 {
            return Ok(0);
        }
        loop {
            {
                let mut g = self.inner.lock();
                if !g.buf.is_empty() {
                    let n = want.min(g.buf.len());
                    for (i, s) in g.buf.drain(..n).enumerate() {
                        out[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
                    }
                    return Ok(n * 2);
                }
                if g.gone || g.closed {
                    return Ok(0);
                }
                if crate::proc::interrupted() {
                    return Err(EINTR);
                }
                g.reader = Some(sched::current_id());
            }
            sched::wait_until(100, || {
                let g = self.inner.lock();
                !g.buf.is_empty() || g.gone
            });
        }
    }

    /// Takes up to `max` samples without blocking (for the kernel's own use).
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
    pub fn take(&self, max: usize) -> Vec<i16> {
        let mut g = self.inner.lock();
        let n = max.min(g.buf.len());
        g.buf.drain(..n).collect()
    }

    /// Samples waiting to be read.
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
    pub fn available(&self) -> usize {
        self.inner.lock().buf.len()
    }

    #[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
    pub fn gone(&self) -> bool {
        self.inner.lock().gone
    }

    /// Samples dropped because nobody read them in time.
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
    pub fn dropped(&self) -> u64 {
        self.inner.lock().dropped
    }

    /// The program is done with it (the fd closed): the device stops when
    /// no stream is left on it.
    pub fn close(self: &Arc<Self>) {
        {
            let mut g = self.inner.lock();
            if g.closed {
                return;
            }
            g.closed = true;
        }
        let last = {
            let mut all = CAPTURES.lock();
            all.retain(|c| !Arc::ptr_eq(c, self));
            !all.iter().any(|c| c.device == self.device)
        };
        if last {
            if let Some(dev) = super::input(self.device) {
                dev.stop();
            }
        }
        crate::gui::server::command(crate::gui::server::Command::Indicators);
    }
}

/// Whether anything records now (the menu bar shows it).
pub fn recording() -> bool {
    !CAPTURES.lock().is_empty()
}

/// Opens a capture stream on input `device` (0: the default input).
pub fn open(device: u32) -> Result<Arc<Capture>, isize> {
    let id = if device == 0 { REGISTRY.lock().default_input } else { device };
    let dev = super::input(id).ok_or(ENODEV)?;
    let c = Arc::new(Capture {
        device: id,
        inner: IrqMutex::new(Inner { buf: VecDeque::new(), gone: false, closed: false, reader: None, dropped: 0 }),
    });
    let first = {
        let mut all = CAPTURES.lock();
        let first = !all.iter().any(|c| c.device == id);
        all.push(c.clone());
        first
    };
    if first && !dev.start() {
        CAPTURES.lock().retain(|x| !Arc::ptr_eq(x, &c));
        return Err(EIO);
    }
    crate::gui::server::command(crate::gui::server::Command::Indicators);
    Ok(c)
}

/// An input device's recording, in the system format (interleaved stereo).
pub fn captured(device: u32, samples: &[i16]) {
    let streams: Vec<Arc<Capture>> = CAPTURES.lock().iter().filter(|c| c.device == device).cloned().collect();
    for c in streams {
        let mut g = c.inner.lock();
        g.buf.extend(samples.iter().copied());
        let over = g.buf.len().saturating_sub(CAPACITY);
        if over > 0 {
            g.buf.drain(..over);
            g.dropped += over as u64;
        }
        if let Some(t) = g.reader.take() {
            sched::wake(t);
        }
    }
}

/// The device went away: its streams end once drained.
pub(super) fn device_gone(device: u32) {
    let streams: Vec<Arc<Capture>> = CAPTURES.lock().iter().filter(|c| c.device == device).cloned().collect();
    for c in streams {
        let mut g = c.inner.lock();
        g.gone = true;
        if let Some(t) = g.reader.take() {
            sched::wake(t);
        }
    }
    CAPTURES.lock().retain(|c| c.device != device);
}
