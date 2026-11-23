//! Sound: the device registry, the mixer, playback and capture streams.
//!
//! Output devices (Intel HDA's speakers and headphones, USB speakers and
//! headsets) and input devices (USB microphones) register here as they are
//! found or plugged in, and leave when unplugged. One output is *active* at
//! a time: the preferred one (`audio_output` in the preferences, by name)
//! when it is present, otherwise a built-in device, otherwise any. Inputs
//! have a default the same way (`audio_input`).
//!
//! Programs open playback streams (`AUDIO_OPEN`) and write 48 kHz, 16-bit
//! stereo samples to them. The `audio` kernel task mixes every stream,
//! applies the master volume and keeps the active device's ring filled a few
//! tens of milliseconds ahead of what it is playing; the device converts to
//! its own rate and format ([`convert`]) and wakes the mixer as it plays. A
//! device with its own volume control gets the system volume in decibels
//! ([`volume`]); the mixer makes up whatever it can't do.
//!
//! Capture streams ([`capture`], `AUDIO_CAPTURE`) read what an input device
//! records, in the same format.

pub mod capture;
pub mod convert;
pub mod hda;
pub mod volume;

pub use capture::{Capture, Input};
pub use convert::Format;

use crate::sched::{self, TaskId};
use crate::sync::{IrqMutex, WaitQueue};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const RATE: usize = aurora_abi::audio::RATE as usize;
/// How far ahead of the device the mixer keeps the ring filled, in frames (64 ms).
const LEAD: usize = 3072;
/// Samples a stream may buffer before its writer blocks (250 ms of stereo).
const STREAM_CAPACITY: usize = RATE / 4 * 2;

/// An output device that plays a ring of interleaved 48 kHz stereo frames
/// in a loop.
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
    /// The format on the wire (the device converts from the ring's).
    fn format(&self) -> Format {
        Format::MIXER
    }
    /// A USB device (rather than a built-in one).
    fn usb(&self) -> bool {
        false
    }
    /// The device's own volume control, (min, max) in 1/256 dB.
    fn volume_range(&self) -> Option<(i16, i16)> {
        None
    }
    /// Sets the device's volume control (within `volume_range`) and mute.
    fn set_volume(&self, _db256: i16, _muted: bool) {}
    /// The mixer starts playing to it.
    fn activate(&self) {}
    /// The mixer moved to another device: fall silent.
    fn deactivate(&self) {
        let zeros = vec![0i16; 1024 * 2];
        let mut f = 0;
        while f < self.frames() {
            let n = 1024.min(self.frames() - f);
            self.write(f, &zeros[..n * 2]);
            f += n;
        }
    }
}

/// The devices present and the choices among them.
pub(crate) struct Registry {
    next_id: u32,
    outputs: Vec<(u32, Arc<dyn Output>)>,
    inputs: Vec<(u32, Arc<dyn Input>)>,
    /// The output the mixer plays to (0: none).
    active: u32,
    pub(crate) default_input: u32,
    /// Chosen in this session (menu bar, tests) until the preference changes.
    chosen_output: Option<String>,
    /// The `audio_output` and `audio_input` preferences (device names).
    preferred: (Option<String>, Option<String>),
}

pub(crate) static REGISTRY: IrqMutex<Registry> = IrqMutex::new(Registry {
    next_id: 1,
    outputs: Vec::new(),
    inputs: Vec::new(),
    active: 0,
    default_input: 0,
    chosen_output: None,
    preferred: (None, None),
});
/// Changes whenever devices come or go or the choice changes (wakes the mixer).
static GENERATION: AtomicU64 = AtomicU64::new(0);
static MIXER_WAKE: WaitQueue = WaitQueue::new();

static STREAMS: IrqMutex<Vec<Arc<Stream>>> = IrqMutex::new(Vec::new());
static VOLUME: AtomicU32 = AtomicU32::new(60);
static MUTED: AtomicBool = AtomicBool::new(false);
/// Bumped by `set_volume`; the mixer applies the new level.
static VOLUME_GEN: AtomicU64 = AtomicU64::new(0);
/// The mixer's gain (16.16): all of the volume, or what the device can't do.
static GAIN: AtomicU32 = AtomicU32::new(0);
static HEADPHONES: AtomicBool = AtomicBool::new(false);
static UNDERRUNS: AtomicU64 = AtomicU64::new(0);
/// Frames the active devices have played since the mixer started.
static PLAYED: AtomicU64 = AtomicU64::new(0);
/// Loudest sample of the last mix (for tests and level meters).
static PEAK: AtomicU32 = AtomicU32::new(0);

// ------------------------------------------------------------------ devices

/// Registers an output device; returns its id.
pub fn add_output(dev: Arc<dyn Output>) -> u32 {
    let id = {
        let mut r = REGISTRY.lock();
        let id = r.next_id;
        r.next_id += 1;
        r.outputs.push((id, dev.clone()));
        id
    };
    log!("audio", "output {}: {}", id, dev.describe());
    choose();
    id
}

/// The output went away (unplugged): the mixer moves to another.
pub fn remove_output(id: u32) {
    let gone = {
        let mut r = REGISTRY.lock();
        let before = r.outputs.len();
        r.outputs.retain(|(i, _)| *i != id);
        before != r.outputs.len()
    };
    if gone {
        log!("audio", "output {} removed", id);
        choose();
    }
}

/// Registers an input device; returns its id (for [`capture::captured`]).
pub fn add_input(dev: Arc<dyn Input>) -> u32 {
    let id = {
        let mut r = REGISTRY.lock();
        let id = r.next_id;
        r.next_id += 1;
        r.inputs.push((id, dev.clone()));
        id
    };
    log!("audio", "input {}: {}", id, dev.describe());
    choose();
    id
}

pub fn remove_input(id: u32) {
    REGISTRY.lock().inputs.retain(|(i, _)| *i != id);
    capture::device_gone(id);
    choose();
}

pub(crate) fn input(id: u32) -> Option<Arc<dyn Input>> {
    REGISTRY.lock().inputs.iter().find(|(i, _)| *i == id).map(|(_, d)| d.clone())
}

fn active() -> Option<(u32, Arc<dyn Output>)> {
    let r = REGISTRY.lock();
    r.outputs.iter().find(|(i, _)| *i == r.active).cloned()
}

/// Picks the active output and the default input again.
fn choose() {
    let mut r = REGISTRY.lock();
    let (want_out, want_in) = r.preferred.clone();
    let named = |name: Option<&String>, list: &[(u32, String, bool)]| {
        name.and_then(|n| list.iter().find(|(_, d, _)| d == n)).map(|(i, _, _)| *i)
    };
    let outs: Vec<(u32, String, bool)> = r.outputs.iter().map(|(i, d)| (*i, d.describe(), d.usb())).collect();
    let ins: Vec<(u32, String, bool)> = r.inputs.iter().map(|(i, d)| (*i, d.describe(), d.usb())).collect();
    let pick = |want: Option<&String>, list: &[(u32, String, bool)]| {
        named(want, list)
            .or_else(|| list.iter().find(|(_, _, usb)| !usb).map(|(i, _, _)| *i))
            .or_else(|| list.first().map(|(i, _, _)| *i))
            .unwrap_or(0)
    };
    let chosen = r.chosen_output.clone();
    let out = named(chosen.as_ref(), &outs).unwrap_or_else(|| pick(want_out.as_ref(), &outs));
    let inp = pick(want_in.as_ref(), &ins);
    if (r.active, r.default_input) != (out, inp) {
        if r.active != out {
            if let Some((_, name, _)) = outs.iter().find(|(i, _, _)| *i == out) {
                log!("audio", "playing to {}", name);
            }
        }
        r.active = out;
        r.default_input = inp;
    }
    drop(r);
    GENERATION.fetch_add(1, Ordering::Relaxed);
    MIXER_WAKE.wake_all();
}

/// Plays to output `id` from now on (this session; the menu bar and
/// Settings also save the choice as `audio_output`).
pub fn select_output(id: u32) -> bool {
    {
        let mut r = REGISTRY.lock();
        let Some(name) = r.outputs.iter().find(|(i, _)| *i == id).map(|(_, d)| d.describe()) else { return false };
        r.chosen_output = Some(name);
    }
    choose();
    true
}

/// The `audio_output`/`audio_input` preference changed (or was loaded).
pub fn preference_changed() {
    // Preferences may read the disk: not under the registry lock.
    let out = crate::gui::prefs::get(aurora_abi::pref::AUDIO_OUTPUT).filter(|v| !v.is_empty());
    let inp = crate::gui::prefs::get(aurora_abi::pref::AUDIO_INPUT).filter(|v| !v.is_empty());
    {
        let mut r = REGISTRY.lock();
        r.chosen_output = None;
        r.preferred = (out, inp);
    }
    choose();
}

/// The active output's id (0: none).
#[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
pub fn active_output() -> u32 {
    REGISTRY.lock().active
}

/// Every device, for `AUDIO_DEVICES`, Settings and the menu bar.
pub fn devices() -> Vec<aurora_abi::audio::AudioDevice> {
    use aurora_abi::audio as abi;
    let r = REGISTRY.lock();
    let entry = |id: u32, name: String, f: Format, mut flags: u32| {
        let mut d = abi::AudioDevice::default();
        let n = name.len().min(d.name.len());
        d.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        d.name_len = n as u32;
        d.id = id;
        d.rate = f.rate;
        d.channels = f.channels as u16;
        d.bits = f.bits as u16;
        if id == r.active || id == r.default_input {
            flags |= abi::DEFAULT;
        }
        d.flags = flags;
        d
    };
    let mut all = Vec::new();
    for (id, d) in &r.outputs {
        let mut flags = abi::OUTPUT;
        if d.usb() {
            flags |= abi::USB;
        }
        if d.volume_range().is_some() {
            flags |= abi::HW_VOLUME;
        }
        all.push(entry(*id, d.describe(), d.format(), flags));
    }
    for (id, d) in &r.inputs {
        all.push(entry(*id, d.describe(), d.format(), abi::INPUT | if d.usb() { abi::USB } else { 0 }));
    }
    all
}

// ------------------------------------------------------------------ streams

/// A playback stream: a queue of samples the mixer drains.
pub struct Stream {
    inner: IrqMutex<StreamInner>,
}

struct StreamInner {
    buf: VecDeque<i16>,
    closed: bool,
    writer: Option<TaskId>,
    paused: bool,
    played: u64,
}

impl Stream {
    fn new() -> Arc<Stream> {
        Arc::new(Stream {
            inner: IrqMutex::new(StreamInner {
                buf: VecDeque::new(),
                closed: false,
                writer: None,
                paused: false,
                played: 0,
            }),
        })
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
                    for s in data[done * 2..(done + n) * 2].as_chunks::<2>().0 {
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

    /// Samples queued and not yet mixed.
    pub fn queued(&self) -> usize {
        self.inner.lock().buf.len()
    }

    /// Samples that can be written without blocking.
    pub fn room(&self) -> usize {
        STREAM_CAPACITY.saturating_sub(self.queued())
    }

    /// Waits until everything queued has been mixed (or a signal arrives).
    pub fn drain(&self) -> Result<(), isize> {
        while self.queued() > 0 {
            if crate::proc::interrupted() {
                return Err(EINTR);
            }
            self.inner.lock().writer = Some(sched::current_id());
            sched::wait_until(50, || self.queued() == 0);
        }
        Ok(())
    }

    /// The stream plays out what it has queued, then goes away.
    pub fn close(&self) {
        let mut g = self.inner.lock();
        g.closed = true;
        if g.paused {
            g.buf.clear();
        }
    }

    pub fn control(&self, op: u64) -> Result<u64, isize> {
        use aurora_abi::audio;
        let mut g = self.inner.lock();
        match op {
            audio::PAUSE => g.paused = true,
            audio::RESUME => g.paused = false,
            audio::FLUSH => {
                g.buf.clear();
                g.played = 0;
                if let Some(t) = g.writer.take() {
                    sched::wake(t);
                }
            }
            audio::POSITION => return Ok(g.played),
            _ => return Err(aurora_abi::err::EINVAL),
        }
        Ok(0)
    }
}

/// Opens a playback stream (`ENODEV` without an output device).
pub fn open() -> Result<Arc<Stream>, isize> {
    if !present() {
        return Err(ENODEV);
    }
    let s = Stream::new();
    STREAMS.lock().push(s.clone());
    Ok(s)
}

/// Plays samples (48 kHz interleaved stereo) without blocking.
pub fn play(samples: &[i16]) {
    if !present() {
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
    if !present() || MUTED.load(Ordering::Relaxed) {
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

// ------------------------------------------------------------------- volume

/// `audio_volume` state word (see `aurora_abi::audio`).
pub fn state() -> u64 {
    use aurora_abi::audio as abi;
    let mut s = VOLUME.load(Ordering::Relaxed) as u64;
    if MUTED.load(Ordering::Relaxed) {
        s |= abi::MUTED;
    }
    if present() {
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
    VOLUME_GEN.fetch_add(1, Ordering::Relaxed);
    MIXER_WAKE.wake_all();
}

/// Splits the volume between the device's control and the mixer.
fn apply_volume(out: &dyn Output) {
    let (v, muted) = (VOLUME.load(Ordering::Relaxed), MUTED.load(Ordering::Relaxed));
    let gain = match out.volume_range() {
        Some((min, max)) => {
            let (db, gain) = volume::split(v, min, max);
            out.set_volume(db, muted || v == 0);
            gain
        }
        None => volume::software_gain(v),
    };
    GAIN.store(if muted { 0 } else { gain }, Ordering::Relaxed);
}

/// An output device exists.
pub fn present() -> bool {
    !REGISTRY.lock().outputs.is_empty()
}

/// The active output's name.
pub fn describe() -> Option<String> {
    active().map(|(_, o)| o.describe())
}

/// (frames played, underruns, loudest recent sample) — for tests and diagnostics.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))] // used by the self-tests
pub fn counters() -> (u64, u64, u32) {
    (PLAYED.load(Ordering::Relaxed), UNDERRUNS.load(Ordering::Relaxed), PEAK.load(Ordering::Relaxed))
}

// -------------------------------------------------------------------- mixer

/// Sums every stream into `mix` (interleaved), dropping finished streams.
fn mix_streams(mix: &mut [i32]) {
    mix.fill(0);
    let streams = STREAMS.lock().clone();
    for s in &streams {
        let mut g = s.inner.lock();
        if g.paused {
            continue;
        }
        let n = mix.len().min(g.buf.len());
        g.played += (n / 2) as u64;
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

/// The mixer's gain and a soft knee instead of hard clipping.
fn finish(mix: &[i32], out: &mut [i16]) -> u32 {
    let gain = GAIN.load(Ordering::Relaxed) as i64;
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

/// The mixer task: plays to the active output, following it as it changes.
fn mixer() {
    let mut mix = vec![0i32; LEAD * 2];
    let mut samples = vec![0i16; LEAD * 2];
    loop {
        let Some((id, out)) = active() else {
            // No device: streams play into nothing at the real rate, so
            // their writers don't wait forever.
            let gen = GENERATION.load(Ordering::Relaxed);
            MIXER_WAKE.wait(20, || GENERATION.load(Ordering::Relaxed) != gen);
            mix_streams(&mut mix[..RATE / 50 * 2]);
            continue;
        };
        out.activate();
        play_to(id, &*out, &mut mix, &mut samples);
        out.deactivate();
        HEADPHONES.store(false, Ordering::Relaxed);
    }
}

/// Keeps `out`'s ring filled until another output becomes active.
fn play_to(id: u32, out: &dyn Output, mix: &mut [i32], samples: &mut [i16]) {
    let ring = out.frames();
    let base = out.position();
    let mut last = base;
    let mut played: u64 = 0;
    let mut written: u64 = 0;
    let mut next_jack = 0;
    let mut volume_gen = VOLUME_GEN.load(Ordering::Relaxed).wrapping_sub(1);
    let start = PLAYED.load(Ordering::Relaxed);
    while REGISTRY.lock().active == id {
        let gen = VOLUME_GEN.load(Ordering::Relaxed);
        if gen != volume_gen {
            volume_gen = gen;
            apply_volume(out);
        }
        let pos = out.position();
        played += ((pos + ring - last) % ring) as u64;
        last = pos;
        PLAYED.store(start + played, Ordering::Relaxed);
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

/// After sleep: built-in devices are restarted here; USB ones come back
/// with their bus.
pub fn resume() {
    hda::resume();
    VOLUME_GEN.fetch_add(1, Ordering::Relaxed);
}

/// Finds built-in sound devices and starts the mixer.
pub fn init() {
    for d in crate::drivers::pci::devices() {
        if d.class == 0x04 && d.subclass == 0x03 {
            if let Some(out) = hda::probe(&d) {
                add_output(out);
                break;
            }
        }
    }
    sched::spawn("audio", mixer);
}
