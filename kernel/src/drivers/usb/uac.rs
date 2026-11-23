//! USB Audio Class 1.0 and 2.0: speakers, headsets, microphones and DACs.
//!
//! An audio function is an AudioControl (AC) interface describing the
//! device's topology — terminals (USB streams, speakers, microphones) joined
//! by units (feature units carry volume and mute) and, in UAC 2, clock
//! entities — plus AudioStreaming (AS) interfaces. Each AS interface has a
//! silent alternate setting 0 and one or more settings with an isochronous
//! endpoint, one per format. Streaming starts by selecting such a setting
//! and stops by selecting 0 again.
//!
//! For each AS interface we pick the setting closest to the system's own
//! format (48 kHz, 16-bit stereo; 44.1 kHz and other rates and widths are
//! converted) and register it with the sound system as an output or input.
//! A device streams only while the mixer plays to it, or while a program
//! records from it.
//!
//! Playback packets are sized from the rate: 48 frames per millisecond at
//! 48 kHz, and 44 or 45 at 44.1 kHz. Synchronous and adaptive endpoints
//! follow the bus clock; an asynchronous endpoint has its own clock and
//! tells us its real rate through a feedback endpoint, which then sizes the
//! packets. The feature unit on the path to the speaker (or from the
//! microphone) gets the system volume in decibels.

use super::iso::{self, IsoHandler, PacketClock};
use super::{Bus, Endpoint, Interface, Setup, Speed, UsbDevice, DESC_ENDPOINT, DESC_INTERFACE};
use crate::drivers::audio::convert::{self, Format, Resampler};
use crate::drivers::audio::{self, Input, Output};
use crate::sync::{IrqMutex, Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;
const SS_COMPANION: u8 = 0x30;

// AudioControl descriptor subtypes (both versions, then UAC 2's own).
const AC_HEADER: u8 = 1;
const INPUT_TERMINAL: u8 = 2;
const OUTPUT_TERMINAL: u8 = 3;
const MIXER_UNIT: u8 = 4;
const SELECTOR_UNIT: u8 = 5;
const FEATURE_UNIT: u8 = 6;
const CLOCK_SOURCE: u8 = 0x0A;
const CLOCK_SELECTOR: u8 = 0x0B;
const CLOCK_MULTIPLIER: u8 = 0x0C;
// AudioStreaming subtypes.
const AS_GENERAL: u8 = 1;
const FORMAT_TYPE: u8 = 2;

// Class requests. UAC 1: SET_CUR, GET_CUR, GET_MIN, GET_MAX; UAC 2: CUR, RANGE.
const SET_CUR: u8 = 0x01;
const GET_MIN: u8 = 0x82;
const GET_MAX: u8 = 0x83;
const CUR: u8 = 0x01;
const RANGE: u8 = 0x02;
const MUTE_CONTROL: u8 = 1;
const VOLUME_CONTROL: u8 = 2;
const SAMPLING_FREQ: u8 = 1;

/// The playback ring the mixer fills: 48 kHz stereo frames (170 ms).
const RING: usize = 8192;
/// Milliseconds of transfers kept queued ahead of the device.
const QUEUE_MS: u32 = 8;

// ---------------------------------------------------------------- topology

/// A terminal, unit or clock entity of the AudioControl interface.
#[derive(Clone, Debug)]
struct Entity {
    id: u8,
    subtype: u8,
    /// Terminals: what they are (0x0101 USB stream, 0x0301 speaker, …).
    terminal: u16,
    /// Where its signal (or clock) comes from.
    sources: Vec<u8>,
    /// UAC 2 terminals: their clock entity.
    clock: u8,
    /// Feature units: (mute, volume) programmable, per channel from 0 (master).
    controls: Vec<(bool, bool)>,
}

/// One alternate setting of an AudioStreaming interface.
#[derive(Clone, Debug, Default)]
struct AltSetting {
    iface: u8,
    alt: u8,
    terminal: u8,
    pcm: bool,
    channels: u8,
    subslot: u8,
    bits: u8,
    /// UAC 1: the rates listed; (0, 0) pairs mark a continuous range (lo, hi).
    rates: Vec<u32>,
    continuous: Option<(u32, u32)>,
    endpoints: Vec<StreamEndpoint>,
}

#[derive(Clone, Copy, Debug)]
struct StreamEndpoint {
    ep: Endpoint,
    /// bmAttributes bits 3:2: 1 asynchronous, 2 adaptive, 3 synchronous.
    sync: u8,
    /// bmAttributes bits 5:4: 0 data, 1 feedback, 2 implicit feedback data.
    usage: u8,
    /// UAC 1 audio endpoints: the feedback endpoint's address.
    synch_address: u8,
    /// Class-specific endpoint attributes (bit 0: sampling frequency control).
    cs_attributes: u8,
}

struct Topology {
    v2: bool,
    /// UAC 1: the streaming interfaces the header lists.
    collection: Vec<u8>,
    entities: Vec<Entity>,
    alts: Vec<AltSetting>,
}

impl Topology {
    fn entity(&self, id: u8) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id && !matches!(e.subtype, CLOCK_SOURCE..=CLOCK_MULTIPLIER))
    }

    fn clock(&self, id: u8) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id && matches!(e.subtype, CLOCK_SOURCE..=CLOCK_MULTIPLIER))
    }

    /// The feature unit on a path from entity `from` back (towards the
    /// sources) to entity `to` (any input terminal when `to` is 0).
    fn feature_between(&self, from: u8, to: u8, depth: usize, seen: Option<u8>) -> Option<Option<u8>> {
        if depth > 16 {
            return None;
        }
        let e = self.entity(from)?;
        let seen = if e.subtype == FEATURE_UNIT && seen.is_none() { Some(e.id) } else { seen };
        if e.subtype == INPUT_TERMINAL {
            return (to == 0 || e.id == to).then_some(seen);
        }
        e.sources.iter().find_map(|&s| self.feature_between(s, to, depth + 1, seen))
    }

    /// Playback: the feature unit between the USB stream's input terminal
    /// and an output terminal that isn't a stream.
    fn playback_feature(&self, stream_terminal: u8) -> Option<u8> {
        self.entities
            .iter()
            // Not a USB terminal (0x01xx): a speaker, headphones, line out…
            .filter(|e| e.subtype == OUTPUT_TERMINAL && e.terminal >> 8 != 0x01)
            .find_map(|ot| self.feature_between(ot.id, stream_terminal, 0, None))
            .flatten()
    }

    /// Capture: the feature unit between an input and the USB stream's output terminal.
    fn capture_feature(&self, stream_terminal: u8) -> Option<u8> {
        self.feature_between(stream_terminal, 0, 0, None).flatten()
    }

    /// UAC 2: the clock source behind a terminal (selectors: their first input).
    fn clock_source(&self, terminal: u8) -> Option<u8> {
        let mut id = self
            .entities
            .iter()
            .find(|e| e.id == terminal && matches!(e.subtype, INPUT_TERMINAL | OUTPUT_TERMINAL))?
            .clock;
        for _ in 0..8 {
            let c = self.clock(id)?;
            if c.subtype == CLOCK_SOURCE {
                return Some(c.id);
            }
            id = *c.sources.first()?;
        }
        None
    }
}

fn u16_at(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}

fn u24_at(d: &[u8], i: usize) -> u32 {
    d[i] as u32 | (d[i + 1] as u32) << 8 | (d[i + 2] as u32) << 16
}

/// Reads the audio function's descriptors out of the whole configuration.
fn parse(raw: &[u8], ac_iface: u8, v2: bool) -> Topology {
    let mut t = Topology { v2, collection: Vec::new(), entities: Vec::new(), alts: Vec::new() };
    // The interface the descriptors belong to: (number, class, subclass).
    let mut cur: Option<(u8, u8, u8)> = None;
    let mut i = 0;
    while i + 2 <= raw.len() {
        let len = raw[i] as usize;
        if len < 2 || i + len > raw.len() {
            break;
        }
        let d = &raw[i..i + len];
        i += len;
        match d[1] {
            DESC_INTERFACE if len >= 9 => {
                cur = Some((d[2], d[5], d[6]));
                if d[5] == super::CLASS_AUDIO && d[6] == 2 {
                    t.alts.push(AltSetting { iface: d[2], alt: d[3], ..Default::default() });
                }
            }
            CS_INTERFACE if len >= 3 => match cur {
                Some((n, _, 1)) if n == ac_iface => parse_ac(&mut t, d),
                Some((_, super::CLASS_AUDIO, 2)) => {
                    if let Some(a) = t.alts.last_mut() {
                        parse_as(a, d, v2);
                    }
                }
                _ => {}
            },
            DESC_ENDPOINT if len >= 7 => {
                if let (Some((_, super::CLASS_AUDIO, 2)), Some(a)) = (cur, t.alts.last_mut()) {
                    let size = u16_at(d, 4);
                    let ep = Endpoint {
                        address: d[2],
                        kind: d[3] & 3,
                        max_packet: size & 0x7FF,
                        interval: d[6],
                        burst: ((size >> 11) & 3) as u8,
                    };
                    a.endpoints.push(StreamEndpoint {
                        ep,
                        sync: (d[3] >> 2) & 3,
                        usage: (d[3] >> 4) & 3,
                        synch_address: if len >= 9 { d[8] } else { 0 },
                        cs_attributes: 0,
                    });
                }
            }
            SS_COMPANION if len >= 6 => {
                if let (Some((_, super::CLASS_AUDIO, 2)), Some(e)) =
                    (cur, t.alts.last_mut().and_then(|a| a.endpoints.last_mut()))
                {
                    e.ep.burst = d[2];
                }
            }
            CS_ENDPOINT if len >= 4 => {
                if let Some(e) = t.alts.last_mut().and_then(|a| a.endpoints.last_mut()) {
                    e.cs_attributes = d[3];
                }
            }
            _ => {}
        }
    }
    t
}

fn parse_ac(t: &mut Topology, d: &[u8]) {
    let len = d.len();
    let v2 = t.v2;
    let mut e = Entity {
        id: d.get(3).copied().unwrap_or(0),
        subtype: d[2],
        terminal: 0,
        sources: Vec::new(),
        clock: 0,
        controls: Vec::new(),
    };
    // `n` input pins listed from byte `at`.
    let pins = |at: usize| -> Vec<u8> {
        let n = d.get(at).copied().unwrap_or(0) as usize;
        d.get(at + 1..(at + 1 + n).min(len)).unwrap_or(&[]).to_vec()
    };
    match d[2] {
        AC_HEADER if !v2 && len >= 8 => {
            let n = d[7] as usize;
            t.collection = d[8..(8 + n).min(len)].to_vec();
            return;
        }
        INPUT_TERMINAL if len >= 8 => {
            e.terminal = u16_at(d, 4);
            if v2 {
                e.clock = d[7];
            }
        }
        OUTPUT_TERMINAL if len >= 8 => {
            e.terminal = u16_at(d, 4);
            e.sources = vec![d[7]];
            if v2 && len >= 9 {
                e.clock = d[8];
            }
        }
        MIXER_UNIT | SELECTOR_UNIT if len >= 5 => e.sources = pins(4),
        FEATURE_UNIT if len >= 6 => {
            e.sources = vec![d[4]];
            if v2 {
                // Four bytes per channel: two bits each for mute and volume.
                for c in d[5..len.saturating_sub(1)].as_chunks::<4>().0 {
                    let bits = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                    e.controls.push((bits & 3 == 3, (bits >> 2) & 3 == 3));
                }
            } else {
                let size = (d[5] as usize).max(1);
                for c in d[6..len.saturating_sub(1)].chunks_exact(size) {
                    e.controls.push((c[0] & 1 != 0, c[0] & 2 != 0));
                }
            }
        }
        // UAC 1 processing (7) and extension (8) units; UAC 2 effect (7),
        // processing (8) and extension (9) units.
        7 if v2 && len >= 7 => e.sources = vec![d[6]],
        7..=9 if len >= 7 => e.sources = pins(6),
        CLOCK_SOURCE if v2 => {}
        CLOCK_SELECTOR if v2 && len >= 5 => e.sources = pins(4),
        CLOCK_MULTIPLIER if v2 && len >= 5 => e.sources = vec![d[4]],
        _ => return,
    }
    t.entities.push(e);
}

fn parse_as(a: &mut AltSetting, d: &[u8], v2: bool) {
    let len = d.len();
    match d[2] {
        AS_GENERAL if !v2 && len >= 7 => {
            a.terminal = d[3];
            a.pcm = u16_at(d, 5) == 0x0001;
        }
        AS_GENERAL if v2 && len >= 11 => {
            a.terminal = d[3];
            a.pcm = d[5] == 1 && d[6] & 1 != 0;
            a.channels = d[10];
        }
        FORMAT_TYPE if !v2 && len >= 8 && d[3] == 1 => {
            a.channels = d[4];
            a.subslot = d[5];
            a.bits = d[6];
            let kinds = d[7] as usize;
            if kinds == 0 && len >= 14 {
                a.continuous = Some((u24_at(d, 8), u24_at(d, 11)));
            } else {
                a.rates = (0..kinds).filter(|k| 8 + k * 3 + 3 <= len).map(|k| u24_at(d, 8 + k * 3)).collect();
            }
        }
        FORMAT_TYPE if v2 && len >= 6 && d[3] == 1 => {
            a.subslot = d[4];
            a.bits = d[5];
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ choices

/// Preference for a rate: 48 kHz, then 44.1 kHz, then the nearest to 48 kHz.
fn rate_score(r: u32) -> u32 {
    match r {
        48_000 => 0,
        44_100 => 1,
        8_000..=192_000 => 2 + r.abs_diff(48_000) / 100,
        _ => u32::MAX,
    }
}

/// The best rate among those listed or in a continuous range.
fn best_rate(rates: &[u32], range: Option<(u32, u32)>) -> Option<u32> {
    let mut all: Vec<u32> = rates.to_vec();
    if let Some((lo, hi)) = range {
        all.extend([48_000, 44_100, lo, hi].into_iter().filter(|r| (lo..=hi).contains(r)));
    }
    all.into_iter().filter(|&r| rate_score(r) != u32::MAX).min_by_key(|&r| rate_score(r))
}

/// The data endpoint of a setting, and its explicit feedback endpoint.
fn endpoints(a: &AltSetting) -> Option<(StreamEndpoint, Option<StreamEndpoint>)> {
    let data = *a.endpoints.iter().find(|e| e.ep.kind == 1 && e.usage != 1)?;
    let fb = a
        .endpoints
        .iter()
        .find(|e| {
            e.ep.kind == 1
                && e.ep.is_in()
                && (e.usage == 1 || (data.synch_address != 0 && e.ep.address == data.synch_address))
        })
        .copied()
        .filter(|_| !data.ep.is_in() && data.sync == 1);
    Some((data, fb))
}

/// Picks interface `iface`'s best setting: PCM nearest 16-bit stereo at
/// 48 kHz. UAC 2 rates come from the clock (`clock_rates`).
fn choose(t: &Topology, iface: u8, clock_rates: &dyn Fn(u8) -> Vec<u32>) -> Option<(AltSetting, u32)> {
    let mut best: Option<(u32, AltSetting, u32)> = None;
    for a in t.alts.iter().filter(|a| a.iface == iface && a.alt != 0 && a.pcm) {
        if !(2..=4).contains(&a.subslot) || a.channels == 0 || endpoints(a).is_none() {
            continue;
        }
        let rate = if t.v2 {
            let clock = t.clock_source(a.terminal);
            best_rate(&clock.map(clock_rates).unwrap_or_default(), None)
        } else {
            best_rate(&a.rates, a.continuous)
        };
        let Some(rate) = rate else { continue };
        let channels = match a.channels {
            2 => 0,
            1 => 1,
            _ => 2,
        };
        let width = match a.subslot {
            2 => 0,
            3 => 1,
            _ => 2,
        };
        let score = rate_score(rate) * 16 + channels * 4 + width;
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, a.clone(), rate));
        }
    }
    best.map(|(_, a, r)| (a, r))
}

// ---------------------------------------------------------------- function

/// The USB device as the streams share it.
struct Function {
    bus: &'static Bus,
    dev: u8,
    speed: Speed,
    ac_iface: u8,
    v2: bool,
    name: String,
    /// Unplugged: nothing more goes to the hardware.
    gone: AtomicBool,
}

impl Function {
    fn request(&self, request_type: u8, request: u8, value: u16, index: u16, data: &mut [u8]) -> Result<usize, u8> {
        if self.gone.load(Ordering::Acquire) {
            return Err(super::ERROR);
        }
        let setup = Setup { request_type, request, value, index, length: data.len() as u16 };
        self.bus.control(self.dev, setup, if data.is_empty() { None } else { Some(data) })
    }

    /// A class request to an entity of the AudioControl interface.
    fn entity_get(&self, request: u8, control: u8, channel: u8, entity: u8, data: &mut [u8]) -> Result<usize, u8> {
        let index = (entity as u16) << 8 | self.ac_iface as u16;
        self.request(0xA1, request, (control as u16) << 8 | channel as u16, index, data)
    }

    fn entity_set(&self, request: u8, control: u8, channel: u8, entity: u8, data: &[u8]) -> Result<usize, u8> {
        let index = (entity as u16) << 8 | self.ac_iface as u16;
        let mut buf = data.to_vec();
        self.request(0x21, request, (control as u16) << 8 | channel as u16, index, &mut buf)
    }

    /// UAC 2: the rates a clock source supports (from its RANGE).
    fn clock_rates(&self, clock: u8) -> Vec<u32> {
        let mut head = [0u8; 2];
        if self.entity_get(RANGE, SAMPLING_FREQ, 0, clock, &mut head).is_err() {
            return Vec::new();
        }
        let n = (u16::from_le_bytes(head) as usize).min(32);
        let mut buf = vec![0u8; 2 + n * 12];
        let Ok(got) = self.entity_get(RANGE, SAMPLING_FREQ, 0, clock, &mut buf) else { return Vec::new() };
        let mut rates = Vec::new();
        for r in buf[2..got.max(2)].as_chunks::<12>().0 {
            let w = |i: usize| u32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]);
            let (lo, hi, step) = (w(0), w(4), w(8));
            rates.push(lo);
            for want in [48_000, 44_100] {
                if lo < want && want <= hi && (step == 0 || (want - lo) % step == 0) {
                    rates.push(want);
                }
            }
        }
        rates
    }
}

/// A feature unit's volume and mute, on the channels that have them.
#[derive(Clone)]
struct VolumeControl {
    unit: u8,
    mute: Vec<u8>,
    volume: Vec<u8>,
    min: i16,
    max: i16,
}

impl VolumeControl {
    fn find(f: &Function, t: &Topology, unit: Option<u8>, channels: u8) -> Option<VolumeControl> {
        let e = t.entity(unit?)?;
        let pick = |want: fn(&(bool, bool)) -> bool| -> Vec<u8> {
            if e.controls.first().is_some_and(want) {
                vec![0]
            } else {
                (1..=channels).filter(|&c| e.controls.get(c as usize).is_some_and(want)).collect()
            }
        };
        let mute = pick(|c| c.0);
        let volume = pick(|c| c.1);
        let (mut min, mut max) = (0i16, 0i16);
        if let Some(&ch) = volume.first() {
            if f.v2 {
                let mut b = [0u8; 2 + 6 * 4];
                if let Ok(n) = f.entity_get(RANGE, VOLUME_CONTROL, ch, e.id, &mut b) {
                    let ranges = (u16::from_le_bytes([b[0], b[1]]) as usize).clamp(1, 4).min((n.max(8) - 2) / 6);
                    min = i16::from_le_bytes([b[2], b[3]]);
                    let last = 2 + (ranges - 1) * 6;
                    max = i16::from_le_bytes([b[last + 2], b[last + 3]]);
                }
            } else {
                let mut b = [0u8; 2];
                if f.entity_get(GET_MIN, VOLUME_CONTROL, ch, e.id, &mut b).is_ok() {
                    min = i16::from_le_bytes(b);
                }
                if f.entity_get(GET_MAX, VOLUME_CONTROL, ch, e.id, &mut b).is_ok() {
                    max = i16::from_le_bytes(b);
                }
            }
        }
        let volume = if min < max { volume } else { Vec::new() };
        if mute.is_empty() && volume.is_empty() {
            return None;
        }
        Some(VolumeControl { unit: e.id, mute, volume, min, max })
    }

    fn range(&self) -> Option<(i16, i16)> {
        (!self.volume.is_empty()).then_some((self.min, self.max))
    }

    fn set(&self, f: &Function, db256: i16, muted: bool) {
        let request = if f.v2 { CUR } else { SET_CUR };
        for &ch in &self.volume {
            let _ =
                f.entity_set(request, VOLUME_CONTROL, ch, self.unit, &db256.clamp(self.min, self.max).to_le_bytes());
        }
        for &ch in &self.mute {
            let _ = f.entity_set(request, MUTE_CONTROL, ch, self.unit, &[muted as u8]);
        }
    }
}

/// What one streaming interface was set up as.
struct StreamSetup {
    iface: u8,
    alt: u8,
    data: StreamEndpoint,
    feedback: Option<StreamEndpoint>,
    format: Format,
    clock: Option<u8>,
    volume: Option<VolumeControl>,
}

impl StreamSetup {
    fn endpoints(&self) -> Vec<Endpoint> {
        let mut v = vec![self.data.ep];
        v.extend(self.feedback.map(|f| f.ep));
        v
    }

    /// Selects the streaming setting at our rate.
    fn start(&self, f: &Function) -> bool {
        if let Some(clock) = self.clock {
            let _ = f.entity_set(CUR, SAMPLING_FREQ, 0, clock, &self.format.rate.to_le_bytes());
        }
        if f.gone.load(Ordering::Acquire)
            || !iso::set_interface(f.bus, f.dev, self.iface, self.alt, &[], &self.endpoints())
        {
            log!("uac", "{}: cannot select streaming setting {}.{}", f.name, self.iface, self.alt);
            return false;
        }
        if !f.v2 && self.data.cs_attributes & 1 != 0 {
            // UAC 1: the rate is an endpoint control.
            let r = self.format.rate.to_le_bytes();
            let mut b = [r[0], r[1], r[2]];
            let _ = f.request(0x22, SET_CUR, (SAMPLING_FREQ as u16) << 8, self.data.ep.address as u16, &mut b);
        }
        true
    }

    /// Back to the silent setting. Once the device is gone (unplugged, or
    /// the controller reset after sleep) the controller ends the streams
    /// itself, so nothing is sent.
    fn stop(&self, f: &Function) {
        if f.gone.load(Ordering::Acquire) {
            return;
        }
        for ep in self.endpoints() {
            f.bus.close_iso(f.dev, &ep);
        }
        iso::set_interface(f.bus, f.dev, self.iface, 0, &self.endpoints(), &[]);
    }
}

// ------------------------------------------------------------------ output

/// A USB speaker or headset, as the mixer sees it.
struct UsbOutput {
    f: Arc<Function>,
    s: StreamSetup,
    /// Serializes starting and stopping.
    control: Mutex<()>,
    streaming: AtomicBool,
    ring: IrqMutex<OutRing>,
    /// The ring frame being sent now.
    pos: AtomicUsize,
    /// Device frames per data interval, 16.16 (feedback adjusts it).
    per_interval: AtomicU32,
    nominal: u32,
    interval_us: u32,
    wake: WaitQueue,
}

struct OutRing {
    samples: Vec<i16>,
    read: usize,
    /// Between `read` and the next frame, 16.16 (resampling).
    phase: u32,
    /// Ring frames per device frame, 16.16.
    step: u32,
    clock: PacketClock,
    since_wake: usize,
}

impl UsbOutput {
    /// One interval's packet: ring frames resampled and packed in the
    /// device's format; what was taken is zeroed, so the device plays
    /// silence if the mixer stops filling.
    fn fill(&self, buf: &mut [u8]) -> usize {
        let fmt = self.s.format;
        let fb = fmt.frame_bytes();
        let per = self.per_interval.load(Ordering::Relaxed);
        let mut wake = false;
        let n = {
            let mut r = self.ring.lock();
            let n = r.clock.next(per).min(buf.len() / fb);
            for k in 0..n {
                let i = r.read;
                let j = (i + 1) % RING;
                let a = [r.samples[i * 2], r.samples[i * 2 + 1]];
                let frame = if r.phase == 0 {
                    a
                } else {
                    let b = [r.samples[j * 2], r.samples[j * 2 + 1]];
                    let p = r.phase as i32;
                    [0, 1].map(|c| (a[c] as i32 + (((b[c] as i32 - a[c] as i32) * p) >> 16)) as i16)
                };
                convert::pack(frame, &fmt, &mut buf[k * fb..(k + 1) * fb]);
                r.phase += r.step;
                while r.phase >= 0x1_0000 {
                    let at = r.read;
                    r.samples[at * 2] = 0;
                    r.samples[at * 2 + 1] = 0;
                    r.read = (at + 1) % RING;
                    r.phase -= 0x1_0000;
                }
            }
            r.since_wake += n;
            if r.since_wake >= 240 {
                r.since_wake = 0;
                wake = true;
            }
            self.pos.store(r.read, Ordering::Release);
            n
        };
        if wake {
            self.wake.wake_all();
        }
        n * fb
    }

    /// Feedback from an asynchronous endpoint: frames per (micro)frame.
    fn feedback(&self, data: &[u8]) {
        if data.len() < 3 {
            return;
        }
        let raw =
            if data.len() >= 4 { u32::from_le_bytes([data[0], data[1], data[2], data[3]]) } else { u24_at(data, 0) };
        let frame_us = if matches!(self.f.speed, Speed::Low | Speed::Full) { 1000 } else { 125 };
        let nominal = iso::frames_per_interval_q16(self.s.format.rate, frame_us);
        // Full speed sends 10.14 in three bytes, high speed 16.16 in four;
        // some devices mix them up, so take whichever is plausible.
        let plausible = |v: u32| v.abs_diff(nominal) <= nominal / 8;
        let Some(per_frame) = [raw << 2, raw, raw >> 2].into_iter().find(|&v| plausible(v)) else { return };
        let per_interval = (per_frame as u64 * self.interval_us as u64 / frame_us as u64) as u32;
        self.per_interval.store(per_interval, Ordering::Relaxed);
    }

    fn start(self: &Arc<Self>) {
        let _g = self.control.lock();
        if self.streaming.load(Ordering::Acquire) || self.f.gone.load(Ordering::Acquire) {
            return;
        }
        {
            let mut r = self.ring.lock();
            r.samples.fill(0);
            r.read = 0;
            r.phase = 0;
            r.clock = PacketClock::new();
        }
        self.pos.store(0, Ordering::Release);
        self.per_interval.store(self.nominal, Ordering::Relaxed);
        if !self.s.start(&self.f) {
            return;
        }
        let depth = iso::depth_for(&self.s.data.ep, self.f.speed, QUEUE_MS);
        self.streaming.store(true, Ordering::Release);
        if let Err(e) = self.f.bus.open_iso(self.f.dev, &self.s.data.ep, depth, Arc::new(OutPump(self.clone()))) {
            log!("uac", "{}: {}", self.f.name, e.name());
            self.streaming.store(false, Ordering::Release);
            self.s.stop(&self.f);
            return;
        }
        if let Some(fb) = self.s.feedback {
            let depth = iso::depth_for(&fb.ep, self.f.speed, QUEUE_MS).min(4);
            let _ = self.f.bus.open_iso(self.f.dev, &fb.ep, depth, Arc::new(FeedbackPump(self.clone())));
        }
    }

    fn stop(&self) {
        let _g = self.control.lock();
        if let Some(stats) = self.f.bus.iso_stats(self.f.dev, &self.s.data.ep) {
            let (done, missed, dry, errors) = stats.snapshot();
            log!(
                "uac",
                "{}: played {} intervals ({} missed, ran dry {} times, {} errors)",
                self.f.name,
                done,
                missed,
                dry,
                errors
            );
        }
        if self.streaming.swap(false, Ordering::AcqRel) {
            self.s.stop(&self.f);
        }
        self.ring.lock().samples.fill(0);
    }
}

/// The mixer's handle (an `Arc`, so the pumps can share the device).
struct OutputRef(Arc<UsbOutput>);

impl Output for OutputRef {
    fn describe(&self) -> String {
        self.0.f.name.clone()
    }
    fn frames(&self) -> usize {
        RING
    }
    fn write(&self, frame: usize, samples: &[i16]) {
        let mut r = self.0.ring.lock();
        let n = samples.len().min((RING - frame) * 2);
        r.samples[frame * 2..frame * 2 + n].copy_from_slice(&samples[..n]);
    }
    fn position(&self) -> usize {
        self.0.pos.load(Ordering::Acquire)
    }
    fn wait(&self, ms: u64) {
        let p = self.position();
        self.0.wake.wait(ms, || self.position() != p || !self.0.streaming.load(Ordering::Relaxed));
        if !self.0.streaming.load(Ordering::Relaxed) {
            // Not playing (failed to start, or gone): don't spin.
            crate::sched::sleep_ms(ms.min(10));
        }
    }
    fn format(&self) -> Format {
        self.0.s.format
    }
    fn usb(&self) -> bool {
        true
    }
    fn volume_range(&self) -> Option<(i16, i16)> {
        self.0.s.volume.as_ref().and_then(|v| v.range())
    }
    fn set_volume(&self, db256: i16, muted: bool) {
        if let Some(v) = &self.0.s.volume {
            v.set(&self.0.f, db256, muted);
        }
    }
    fn activate(&self) {
        self.0.start();
    }
    fn deactivate(&self) {
        self.0.stop();
    }
}

struct OutPump(Arc<UsbOutput>);

impl IsoHandler for OutPump {
    fn fill(&self, buf: &mut [u8]) -> usize {
        self.0.fill(buf)
    }
    fn stopped(&self) {
        self.0.streaming.store(false, Ordering::Release);
        self.0.wake.wake_all();
    }
}

struct FeedbackPump(Arc<UsbOutput>);

impl IsoHandler for FeedbackPump {
    fn data(&self, data: &[u8], status: u8) {
        if status == super::OK {
            self.0.feedback(data);
        }
    }
}

// ------------------------------------------------------------------- input

/// A USB microphone (or a headset's).
struct UsbInput {
    f: Arc<Function>,
    s: StreamSetup,
    id: AtomicU32,
    control: Mutex<()>,
    streaming: AtomicBool,
    resampler: IrqMutex<Resampler>,
}

impl UsbInput {
    fn data(&self, data: &[u8]) {
        let fmt = self.s.format;
        let fb = fmt.frame_bytes();
        let mut out = Vec::with_capacity(data.len() / fb * 2 * 2 + 4);
        {
            let mut rs = self.resampler.lock();
            for frame in data.chunks_exact(fb) {
                rs.push(convert::unpack(frame, &fmt), &mut out);
            }
        }
        if !out.is_empty() {
            audio::capture::captured(self.id.load(Ordering::Relaxed), &out);
        }
    }
}

struct InputRef(Arc<UsbInput>);

impl Input for InputRef {
    fn describe(&self) -> String {
        self.0.f.name.clone()
    }
    fn format(&self) -> Format {
        self.0.s.format
    }
    fn usb(&self) -> bool {
        true
    }
    fn start(&self) -> bool {
        let me = &self.0;
        let _g = me.control.lock();
        if me.streaming.load(Ordering::Acquire) {
            return true;
        }
        if me.f.gone.load(Ordering::Acquire) || !me.s.start(&me.f) {
            return false;
        }
        // Recording level: as loud as the device goes without boost, unmuted.
        if let Some(v) = &me.s.volume {
            v.set(&me.f, v.max.min(0).max(v.min), false);
        }
        *me.resampler.lock() = Resampler::new(me.s.format.rate, audio::RATE as u32);
        let depth = iso::depth_for(&me.s.data.ep, me.f.speed, QUEUE_MS);
        me.streaming.store(true, Ordering::Release);
        if let Err(e) = me.f.bus.open_iso(me.f.dev, &me.s.data.ep, depth, Arc::new(InPump(me.clone()))) {
            log!("uac", "{}: {}", me.f.name, e.name());
            me.streaming.store(false, Ordering::Release);
            me.s.stop(&me.f);
            return false;
        }
        true
    }
    fn stop(&self) {
        let me = &self.0;
        let _g = me.control.lock();
        if me.streaming.swap(false, Ordering::AcqRel) {
            me.s.stop(&me.f);
        }
    }
}

struct InPump(Arc<UsbInput>);

impl IsoHandler for InPump {
    fn data(&self, data: &[u8], status: u8) {
        if status == super::OK && !data.is_empty() {
            self.0.data(data);
        }
    }
    fn stopped(&self) {
        self.0.streaming.store(false, Ordering::Release);
    }
}

// ------------------------------------------------------------------ attach

/// Takes an audio function (from its AudioControl interface).
pub fn attach(dev: &UsbDevice, ac: &Interface) -> Option<&'static str> {
    let v2 = match ac.protocol {
        0x00 => false,
        0x20 => true,
        p => {
            log!("uac", "{}: USB Audio protocol {:#x} is not supported", dev.name, p);
            return None;
        }
    };
    let raw = dev.raw_configuration()?;
    let t = parse(&raw, ac.number, v2);
    let f = Arc::new(Function {
        bus: dev.bus,
        dev: dev.dev,
        speed: dev.speed(),
        ac_iface: ac.number,
        v2,
        name: dev.name.clone(),
        gone: AtomicBool::new(false),
    });
    let mut ifaces: Vec<u8> = t.alts.iter().map(|a| a.iface).collect();
    ifaces.dedup();
    if !v2 && !t.collection.is_empty() {
        ifaces.retain(|i| t.collection.contains(i));
    }
    let mut outputs: Vec<Arc<UsbOutput>> = Vec::new();
    let mut inputs: Vec<Arc<UsbInput>> = Vec::new();
    let mut probed = false;
    for iface in ifaces {
        let Some((a, rate)) = choose(&t, iface, &|clock| f.clock_rates(clock)) else {
            log!("uac", "{}: interface {} has no PCM format we can use", f.name, iface);
            continue;
        };
        let (data, feedback) = endpoints(&a).unwrap();
        if !probed {
            // Only some controllers can stream: ask before setting anything up.
            probed = true;
            struct Nothing;
            impl IsoHandler for Nothing {}
            match dev.bus.open_iso(dev.dev, &data.ep, 1, Arc::new(Nothing)) {
                Err(iso::IsoError::Unsupported) => {
                    log!("uac", "{}: isochronous transfers are not supported on {}", f.name, dev.bus.hcd.kind());
                    return Some("audio (unsupported controller)");
                }
                Ok(()) => dev.bus.close_iso(dev.dev, &data.ep),
                Err(_) => {}
            }
        }
        let format = Format { rate, channels: a.channels, subslot: a.subslot, bits: a.bits };
        let unit = if data.ep.is_in() { t.capture_feature(a.terminal) } else { t.playback_feature(a.terminal) };
        let s = StreamSetup {
            iface,
            alt: a.alt,
            data,
            feedback,
            format,
            clock: if v2 { t.clock_source(a.terminal) } else { None },
            volume: VolumeControl::find(&f, &t, unit, a.channels),
        };
        let interval_us = iso::interval_us(&data.ep, f.speed);
        log!(
            "uac",
            "{}: {} on interface {}.{}: {} Hz, {} channel(s), {}-bit, {}{}{}",
            f.name,
            if data.ep.is_in() { "input" } else { "output" },
            iface,
            a.alt,
            rate,
            a.channels,
            a.bits,
            match data.sync {
                1 => "asynchronous",
                2 => "adaptive",
                3 => "synchronous",
                _ => "unsynchronized",
            },
            if feedback.is_some() { " with feedback" } else { "" },
            if s.volume.as_ref().and_then(|v| v.range()).is_some() { ", volume control" } else { "" }
        );
        if data.ep.is_in() {
            inputs.push(Arc::new(UsbInput {
                f: f.clone(),
                s,
                id: AtomicU32::new(0),
                control: Mutex::new(()),
                streaming: AtomicBool::new(false),
                resampler: IrqMutex::new(Resampler::new(rate, audio::RATE as u32)),
            }));
        } else {
            let nominal = iso::frames_per_interval_q16(rate, interval_us);
            outputs.push(Arc::new(UsbOutput {
                f: f.clone(),
                s,
                control: Mutex::new(()),
                streaming: AtomicBool::new(false),
                ring: IrqMutex::new(OutRing {
                    samples: vec![0; RING * 2],
                    read: 0,
                    phase: 0,
                    step: ((audio::RATE as u64) << 16).div_euclid(rate as u64) as u32,
                    clock: PacketClock::new(),
                    since_wake: 0,
                }),
                pos: AtomicUsize::new(0),
                per_interval: AtomicU32::new(nominal),
                nominal,
                interval_us,
                wake: WaitQueue::new(),
            }));
        }
    }
    if outputs.is_empty() && inputs.is_empty() {
        return None;
    }
    let out_ids: Vec<u32> = outputs.iter().map(|o| audio::add_output(Arc::new(OutputRef(o.clone())))).collect();
    let in_ids: Vec<u32> = inputs
        .iter()
        .map(|i| {
            let id = audio::add_input(Arc::new(InputRef(i.clone())));
            i.id.store(id, Ordering::Relaxed);
            id
        })
        .collect();
    let hook_f = f.clone();
    dev.bus.on_detach(
        dev.dev,
        Box::new(move || {
            // Mid-stream: the streams end without another word to the device.
            hook_f.gone.store(true, Ordering::Release);
            for &id in &out_ids {
                audio::remove_output(id);
            }
            for &id in &in_ids {
                audio::remove_input(id);
            }
        }),
    );
    Some(match (outputs.is_empty(), inputs.is_empty()) {
        (false, false) => "audio: speaker, microphone",
        (false, true) => "audio: speaker",
        _ => "audio: microphone",
    })
}
