//! Sound output.
//!
//! Programs open playback streams (`AUDIO_OPEN`) and write 48 kHz, 16-bit
//! stereo samples to them. The `audio` kernel task mixes every stream, applies
//! the master volume and keeps the output device's DMA ring filled a few tens
//! of milliseconds ahead of what it is playing. The device (Intel HDA) wakes
//! the mixer with an interrupt each time it finishes a segment of the ring.

pub mod hda;

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use spin::Once;

pub const RATE: usize = aurora_abi::audio::RATE as usize;
/// How far ahead of the device the mixer keeps the ring filled, in frames (64 ms).
const LEAD: usize = 3072;
/// Samples a stream may buffer before its writer blocks (250 ms of stereo).
const STREAM_CAPACITY: usize = RATE / 4 * 2;

/// An output device that plays a ring of interleaved stereo frames in a loop.
pub trait Output: Send + Sync {
    fn describe(&self) -> String;
    /// Length of the ring in frames.
    fn frames(&self) -> usize;
    /// Copies interleaved samples into the ring starting at `frame` (no wrap).
    fn write(&self, frame: usize, samples: &[i16]);
    /// The frame the device is playing now.
    fn position(&self) -> usize;
    /// Sleeps until the device has played further into the ring, or `ms` pass.
    fn wait(&self, ms: u64);
    /// Re-checks the headphone jack (muting speakers while it is in use);
    /// `Some(plugged)` if the device can tell.
    fn poll_jack(&self) -> Option<bool> {
        None
    }
}

static OUTPUT: Once<Arc<dyn Output>> = Once::new();
static STREAMS: IrqMutex<Vec<Arc<Stream>>> = IrqMutex::new(Vec::new());
static VOLUME: AtomicU32 = AtomicU32::new(60);
static MUTED: AtomicBool = AtomicBool::new(false);
static HEADPHONES: AtomicBool = AtomicBool::new(false);
static UNDERRUNS: AtomicU64 = AtomicU64::new(0);
/// Frames the device has played since the mixer started.
static PLAYED: AtomicU64 = AtomicU64::new(0);
/// Loudest sample of the last mix (for tests and level meters).
static PEAK: AtomicU32 = AtomicU32::new(0);

/// A playback stream: a queue of samples the mixer drains.
pub struct Stream {
    inner: IrqMutex<StreamInner>,
}

struct StreamInner {
    buf: VecDeque<i16>,
    closed: bool,
    writer: Option<TaskId>,
}

impl Stream {
    fn new() -> Arc<Stream> {
        Arc::new(Stream { inner: IrqMutex::new(StreamInner { buf: VecDeque::new(), closed: false, writer: None }) })
    }

    /// Queues little-endian `i16` samples, blocking while the stream is full.
    /// Returns the bytes consumed (whole samples only).
    pub fn write(&self, data: &[u8]) -> Result<usize, isize> {
        let samples = data.len() / 2;
        let mut done = 0;
        while done < samples {
            {
                let mut g = self.inner.lock();
                let room = STREAM_CAPACITY.saturating_sub(g.buf.len());
                if room > 0 {
                    let n = room.min(samples - done);
                    for s in data[done * 2..(done + n) * 2].chunks_exact(2) {
                        g.buf.push_back(i16::from_le_bytes([s[0], s[1]]));
                    }
                    done += n;
                    continue;
                }
                if crate::proc::interrupted() {
                    return if done > 0 { Ok(done * 2) } else { Err(EINTR) };
                }
                g.writer = Some(sched::current_id());
            }
            sched::wait_until(50, || self.inner.lock().buf.len() < STREAM_CAPACITY);
        }
        Ok(done * 2)
    }

    /// The stream plays out what it has queued, then goes away.
    pub fn close(&self) {
        self.inner.lock().closed = true;
    }
}

/// Opens a playback stream (`ENODEV` without an output device).
pub fn open() -> Result<Arc<Stream>, isize> {
    if OUTPUT.get().is_none() {
        return Err(ENODEV);
    }
    let s = Stream::new();
    STREAMS.lock().push(s.clone());
    Ok(s)
}

/// Plays samples (48 kHz interleaved stereo) without blocking.
pub fn play(samples: &[i16]) {
    if OUTPUT.get().is_none() {
        return;
    }
    let s = Stream::new();
    {
        let mut g = s.inner.lock();
        g.buf.extend(samples.iter().copied());
        g.closed = true;
    }
    STREAMS.lock().push(s);
}

static SOUNDS: IrqMutex<BTreeMap<String, Arc<Vec<i16>>>> = IrqMutex::new(BTreeMap::new());

/// Plays `/System/Sounds/<name>.wav` (cached after the first use).
pub fn play_sound(name: &str) {
    if OUTPUT.get().is_none() || MUTED.load(Ordering::Relaxed) {
        return;
    }
    let cached = SOUNDS.lock().get(name).cloned();
    let samples = match cached {
        Some(s) => s,
        None => {
            let path = alloc::format!("/System/Sounds/{}.wav", name);
            let Some(pcm) = crate::fs::read_all(&path).ok().and_then(|b| aurora_wav::decode(&b, RATE)) else {
                log!("audio", "cannot play {}", path);
                return;
            };
            let pcm = Arc::new(pcm);
            SOUNDS.lock().insert(String::from(name), pcm.clone());
            pcm
        }
    };
    play(&samples);
}

/// `audio_volume` state word (see `aurora_abi::audio`).
pub fn state() -> u64 {
    use aurora_abi::audio as abi;
    let mut s = VOLUME.load(Ordering::Relaxed) as u64;
    if MUTED.load(Ordering::Relaxed) {
        s |= abi::MUTED;
    }
    if OUTPUT.get().is_some() {
        s |= abi::PRESENT;
    }
    if HEADPHONES.load(Ordering::Relaxed) {
        s |= abi::HEADPHONES;
    }
    s
}

pub fn set_volume(volume: u32, muted: bool) {
    VOLUME.store(volume.min(100), Ordering::Relaxed);
    MUTED.store(muted, Ordering::Relaxed);
}

pub fn present() -> bool {
    OUTPUT.get().is_some()
}

pub fn describe() -> Option<String> {
    OUTPUT.get().map(|o| o.describe())
}

/// (frames played, underruns, loudest recent sample) — for tests and diagnostics.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
pub fn counters() -> (u64, u64, u32) {
    (PLAYED.load(Ordering::Relaxed), UNDERRUNS.load(Ordering::Relaxed), PEAK.load(Ordering::Relaxed))
}

/// Sums every stream into `mix` (interleaved), dropping finished streams.
fn mix_streams(mix: &mut [i32]) {
    mix.fill(0);
    let streams = STREAMS.lock().clone();
    for s in &streams {
        let mut g = s.inner.lock();
        let n = mix.len().min(g.buf.len());
        for (m, v) in mix[..n].iter_mut().zip(g.buf.drain(..n)) {
            *m += v as i32;
        }
        if let Some(t) = g.writer.take() {
            sched::wake(t);
        }
    }
    STREAMS.lock().retain(|s| {
        let g = s.inner.lock();
        !(g.closed && g.buf.is_empty())
    });
}

/// Master volume (a perceptual square curve) and a soft knee instead of hard clipping.
fn finish(mix: &[i32], out: &mut [i16]) -> u32 {
    let v = VOLUME.load(Ordering::Relaxed) as i64;
    let gain = if MUTED.load(Ordering::Relaxed) { 0 } else { v * v * 65536 / 10_000 };
    let mut peak = 0;
    for (o, &m) in out.iter_mut().zip(mix) {
        let mut x = (m as i64 * gain) >> 16;
        const KNEE: i64 = 24_576;
        if x.abs() > KNEE {
            x = x.signum() * (KNEE + (x.abs() - KNEE) / 4);
        }
        let x = x.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        peak = peak.max(x.unsigned_abs() as u32);
        *o = x;
    }
    peak
}

/// The mixer task.
fn mixer() {
    let Some(out) = OUTPUT.get().cloned() else { return };
    let ring = out.frames();
    let base = out.position();
    let mut last = base;
    let mut played: u64 = 0;
    let mut written: u64 = 0;
    let mut mix = vec![0i32; LEAD * 2];
    let mut samples = vec![0i16; LEAD * 2];
    let mut next_jack = 0;
    loop {
        let pos = out.position();
        played += ((pos + ring - last) % ring) as u64;
        last = pos;
        PLAYED.store(played, Ordering::Relaxed);
        if written < played {
            // We fell behind the device: skip ahead instead of writing the past.
            UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            written = played;
        }
        let target = played + LEAD as u64;
        if written < target {
            let n = (target - written) as usize;
            mix_streams(&mut mix[..n * 2]);
            let peak = finish(&mix[..n * 2], &mut samples[..n * 2]);
            PEAK.store(peak, Ordering::Relaxed);
            let start = ((base as u64 + written) % ring as u64) as usize;
            let first = n.min(ring - start);
            out.write(start, &samples[..first * 2]);
            if first < n {
                out.write(0, &samples[first * 2..n * 2]);
            }
            written += n as u64;
        }
        let now = crate::time::uptime_ms();
        if now >= next_jack {
            next_jack = now + 500;
            if let Some(hp) = out.poll_jack() {
                if HEADPHONES.swap(hp, Ordering::Relaxed) != hp {
                    log!("audio", "headphones {}", if hp { "plugged in" } else { "unplugged" });
                }
            }
        }
        out.wait(25);
    }
}

/// Finds a sound device and starts the mixer.
pub fn init() {
    for d in crate::drivers::pci::devices() {
        if d.class == 0x04 && d.subclass == 0x03 {
            if let Some(out) = hda::probe(&d) {
                log!("audio", "output: {}", out.describe());
                OUTPUT.call_once(|| out);
                sched::spawn("audio", mixer);
                return;
            }
        }
    }
}
