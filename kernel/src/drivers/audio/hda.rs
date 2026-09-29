//! Intel High Definition Audio: controller and codec driver (playback).
//!
//! The controller talks to codecs through command/response rings (CORB and
//! RIRB), falling back to the immediate command registers. In each codec's
//! audio function group we walk the widget graph from every output pin
//! (speaker, headphone, line out) back to a DAC, select that route, unmute
//! and power it, and bind every DAC to one output stream. The stream plays a
//! looping ring of `SEGMENTS` buffers and interrupts (MSI) after each one.

use super::Output;
use crate::drivers::block::{wait_for, Dma};
use crate::drivers::pci;
use crate::sync::{Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicBool, Ordering};

// Controller registers.
const GCAP: u64 = 0x00;
const GCTL: u64 = 0x08;
const STATESTS: u64 = 0x0E;
const INTCTL: u64 = 0x20;
const CORBLBASE: u64 = 0x40;
const CORBUBASE: u64 = 0x44;
const CORBWP: u64 = 0x48;
const CORBRP: u64 = 0x4A;
const CORBCTL: u64 = 0x4C;
const CORBSIZE: u64 = 0x4E;
const RIRBLBASE: u64 = 0x50;
const RIRBUBASE: u64 = 0x54;
const RIRBWP: u64 = 0x58;
const RINTCNT: u64 = 0x5A;
const RIRBCTL: u64 = 0x5C;
const RIRBSTS: u64 = 0x5D;
const RIRBSIZE: u64 = 0x5E;
const ICOI: u64 = 0x60;
const ICII: u64 = 0x64;
const ICIS: u64 = 0x68;

// Stream descriptor registers (from the descriptor's base).
const SD_CTL: u64 = 0x00;
const SD_CTL_STREAM: u64 = 0x02;
const SD_STS: u64 = 0x03;
const SD_LPIB: u64 = 0x04;
const SD_CBL: u64 = 0x08;
const SD_LVI: u64 = 0x0C;
const SD_FMT: u64 = 0x12;
const SD_BDPL: u64 = 0x18;
const SD_BDPU: u64 = 0x1C;
const SD_RESET: u8 = 1 << 0;
const SD_RUN: u8 = 1 << 1;
const SD_IOCE: u8 = 1 << 2;
/// Buffer completion, FIFO error, descriptor error (write 1 to clear).
const SD_STS_ALL: u8 = 0x1C;

// Codec verbs (12-bit, 8-bit payload) and 4-bit verbs (16-bit payload).
const GET_PARAM: u32 = 0xF00;
const GET_CONN_LIST: u32 = 0xF02;
const SET_CONN_SELECT: u32 = 0x701;
const SET_POWER: u32 = 0x705;
const SET_CHANNEL_STREAM: u32 = 0x706;
const SET_PIN_CTL: u32 = 0x707;
const GET_PIN_SENSE: u32 = 0xF09;
const SET_EAPD: u32 = 0x70C;
const GET_CONFIG: u32 = 0xF1C;
const SET_FORMAT4: u32 = 0x2;
const SET_AMP4: u32 = 0x3;

// Parameters.
const P_VENDOR: u32 = 0x00;
const P_NODES: u32 = 0x04;
const P_FG_TYPE: u32 = 0x05;
const P_WIDGET_CAP: u32 = 0x09;
const P_PIN_CAP: u32 = 0x0C;
const P_IN_AMP: u32 = 0x0D;
const P_CONN_LEN: u32 = 0x0E;
const P_OUT_AMP: u32 = 0x12;

// Widget types (bits 23:20 of the widget capabilities).
const W_DAC: u32 = 0;
const W_MIXER: u32 = 2;
const W_PIN: u32 = 4;

const PIN_OUT_EN: u32 = 1 << 6;
const PIN_HP_EN: u32 = 1 << 7;

// Default device (pin configuration bits 23:20).
const DEV_LINE_OUT: u32 = 0;
const DEV_SPEAKER: u32 = 1;
const DEV_HEADPHONE: u32 = 2;

/// 48 kHz, 16 bits, 2 channels.
const FORMAT: u16 = 0x0011;
const STREAM_TAG: u32 = 1;
const SEGMENTS: usize = 8;
const SEGMENT_FRAMES: usize = 1024;
const RING_FRAMES: usize = SEGMENTS * SEGMENT_FRAMES;

#[derive(Clone, Copy)]
struct Regs(u64);

impl Regs {
    fn r8(&self, o: u64) -> u8 {
        unsafe { ((self.0 + o) as *const u8).read_volatile() }
    }
    fn r16(&self, o: u64) -> u16 {
        unsafe { ((self.0 + o) as *const u16).read_volatile() }
    }
    fn r32(&self, o: u64) -> u32 {
        unsafe { ((self.0 + o) as *const u32).read_volatile() }
    }
    fn w8(&self, o: u64, v: u8) {
        unsafe { ((self.0 + o) as *mut u8).write_volatile(v) }
    }
    fn w16(&self, o: u64, v: u16) {
        unsafe { ((self.0 + o) as *mut u16).write_volatile(v) }
    }
    fn w32(&self, o: u64, v: u32) {
        unsafe { ((self.0 + o) as *mut u32).write_volatile(v) }
    }
}

fn delay_us(us: u64) {
    let end = crate::time::now_ns() + us * 1000;
    while crate::time::now_ns() < end {
        core::hint::spin_loop();
    }
}

/// The codec command channel.
struct Commands {
    regs: Regs,
    rings: Option<(Dma, Dma)>,
    corb_wp: usize,
    rirb_rp: usize,
}

impl Commands {
    fn send(&mut self, verb: u32) -> Option<u32> {
        if self.rings.is_some() {
            if let Some(r) = self.ring_command(verb) {
                return Some(r);
            }
            log!("hda", "no response on the command ring; using immediate commands");
            self.rings = None;
        }
        self.immediate(verb)
    }

    fn ring_command(&mut self, verb: u32) -> Option<u32> {
        let (corb, rirb) = self.rings.as_ref()?;
        let wp = (self.corb_wp + 1) % 256;
        corb.write::<u32>(wp * 4, verb);
        fence(Ordering::SeqCst);
        self.regs.w16(CORBWP, wp as u16);
        self.corb_wp = wp;
        let deadline = crate::time::now_ns() + 20_000_000;
        while crate::time::now_ns() < deadline {
            if self.regs.r16(RIRBWP) as usize & 0xFF != self.rirb_rp {
                self.rirb_rp = (self.rirb_rp + 1) % 256;
                let response = rirb.read::<u32>(self.rirb_rp * 8);
                let extra = rirb.read::<u32>(self.rirb_rp * 8 + 4);
                self.regs.w8(RIRBSTS, 0x5);
                if extra & 0x10 != 0 {
                    continue; // an unsolicited response, not ours
                }
                return Some(response);
            }
            core::hint::spin_loop();
        }
        None
    }

    fn immediate(&mut self, verb: u32) -> Option<u32> {
        let r = self.regs;
        wait_for(20, || r.r16(ICIS) & 1 == 0).ok()?;
        r.w16(ICIS, 0x2); // clear "result valid"
        r.w32(ICOI, verb);
        r.w16(ICIS, 0x1); // busy: send
        wait_for(20, || r.r16(ICIS) & 0x3 == 0x2).ok()?;
        Some(r.r32(ICII))
    }
}

fn verb(cad: u32, nid: u32, verb: u32, payload: u32) -> u32 {
    cad << 28 | nid << 20 | verb << 8 | (payload & 0xFF)
}

fn verb4(cad: u32, nid: u32, verb: u32, payload: u32) -> u32 {
    cad << 28 | nid << 20 | verb << 16 | (payload & 0xFFFF)
}

struct Widget {
    kind: u32,
    caps: u32,
    conns: Vec<u32>,
    pin_caps: u32,
    config: u32,
    in_amp: u32,
    out_amp: u32,
}

/// An output pin we drive.
#[derive(Clone, Copy)]
struct Pin {
    nid: u32,
    device: u32,
    /// Supports presence detection (a jack).
    sense: bool,
    /// Has an external amplifier enable (EAPD).
    eapd: bool,
}

pub struct Hda {
    regs: Regs,
    cmd: Mutex<Commands>,
    /// Register offset of our output stream descriptor (after the input ones).
    sd: u64,
    ring: Dma,
    bdl: Dma,
    irq: WaitQueue,
    has_irq: AtomicBool,
    cad: u32,
    pins: Vec<Pin>,
    headphones: AtomicBool,
    name: String,
}

/// MSI handler: acknowledges the stream's buffer-complete status and wakes the mixer.
fn interrupt(arg: usize) {
    let h = unsafe { &*(arg as *const Hda) };
    let sts = h.regs.r8(h.sd + SD_STS);
    h.regs.w8(h.sd + SD_STS, sts & SD_STS_ALL);
    h.irq.wake_all();
}

impl Hda {
    fn command(&self, v: u32) -> u32 {
        self.cmd.lock().send(v).unwrap_or(0)
    }

    fn param(&self, nid: u32, p: u32) -> u32 {
        self.command(verb(self.cad, nid, GET_PARAM, p))
    }

    fn connections(&self, nid: u32) -> Vec<u32> {
        let info = self.param(nid, P_CONN_LEN);
        let (len, long) = ((info & 0x7F) as usize, info & 0x80 != 0);
        let (per, bits) = if long { (2, 16) } else { (4, 8) };
        let mut out = Vec::new();
        let mut prev = 0;
        let mut i = 0;
        while i < len {
            let word = self.command(verb(self.cad, nid, GET_CONN_LIST, i as u32));
            for k in 0..per {
                if i + k >= len {
                    break;
                }
                let e = (word >> (k * bits)) & ((1 << bits) - 1);
                let range = e & (1 << (bits - 1)) != 0;
                let id = e & ((1 << (bits - 1)) - 1);
                if range && prev != 0 && id > prev {
                    out.extend(prev + 1..=id); // a range: every node after the previous one
                } else {
                    out.push(id);
                }
                prev = id;
            }
            i += per;
        }
        out
    }

    /// Route from `nid` back to a DAC: [(node, chosen input)], then the DAC.
    fn find_dac(&self, widgets: &BTreeMap<u32, Widget>, nid: u32, depth: usize) -> Option<(Vec<(u32, usize)>, u32)> {
        let w = widgets.get(&nid)?;
        if w.kind == W_DAC {
            return Some((Vec::new(), nid));
        }
        if depth >= 6 {
            return None;
        }
        for (i, &c) in w.conns.iter().enumerate() {
            if let Some((mut path, dac)) = self.find_dac(widgets, c, depth + 1) {
                path.insert(0, (nid, i));
                return Some((path, dac));
            }
        }
        None
    }

    /// The amp payload for 0 dB (or the loudest step below it), unmuted.
    fn unmuted(caps: u32) -> u32 {
        let offset = caps & 0x7F;
        let steps = (caps >> 8) & 0x7F;
        offset.min(steps)
    }

    fn set_up_route(&self, widgets: &BTreeMap<u32, Widget>, path: &[(u32, usize)], dac: u32) {
        let cad = self.cad;
        for &(nid, input) in path {
            let w = &widgets[&nid];
            self.command(verb(cad, nid, SET_POWER, 0));
            if w.kind != W_MIXER && w.conns.len() > 1 {
                self.command(verb(cad, nid, SET_CONN_SELECT, input as u32));
            }
            if w.caps & (1 << 1) != 0 {
                // Input amp: bit 14 input, 13/12 left/right, 11:8 index.
                let gain = Self::unmuted(w.in_amp);
                self.command(verb4(cad, nid, SET_AMP4, 0x7000 | (input as u32) << 8 | gain));
            }
            if w.caps & (1 << 2) != 0 {
                // Output amp: bit 15 output, 13/12 left/right.
                self.command(verb4(cad, nid, SET_AMP4, 0xB000 | Self::unmuted(w.out_amp)));
            }
        }
        let d = &widgets[&dac];
        self.command(verb(cad, dac, SET_POWER, 0));
        self.command(verb4(cad, dac, SET_FORMAT4, FORMAT as u32));
        self.command(verb(cad, dac, SET_CHANNEL_STREAM, STREAM_TAG << 4));
        if d.caps & (1 << 2) != 0 {
            self.command(verb4(cad, dac, SET_AMP4, 0xB000 | Self::unmuted(d.out_amp)));
        }
    }

    fn enable_pin(&self, pin: &Pin, on: bool) {
        let ctl = if !on {
            0
        } else if pin.device == DEV_HEADPHONE {
            PIN_OUT_EN | PIN_HP_EN
        } else {
            PIN_OUT_EN
        };
        self.command(verb(self.cad, pin.nid, SET_PIN_CTL, ctl));
        if pin.eapd {
            self.command(verb(self.cad, pin.nid, SET_EAPD, 0x2));
        }
    }

    fn headphone_plugged(&self) -> Option<bool> {
        let jacks: Vec<&Pin> = self.pins.iter().filter(|p| p.device == DEV_HEADPHONE && p.sense).collect();
        if jacks.is_empty() {
            return None;
        }
        Some(jacks.iter().any(|p| self.command(verb(self.cad, p.nid, GET_PIN_SENSE, 0)) & (1 << 31) != 0))
    }
}

impl Output for Hda {
    fn describe(&self) -> String {
        self.name.clone()
    }
    fn frames(&self) -> usize {
        RING_FRAMES
    }
    fn write(&self, frame: usize, samples: &[i16]) {
        let n = samples.len().min((RING_FRAMES - frame) * 2);
        unsafe {
            core::ptr::copy_nonoverlapping(samples.as_ptr(), (self.ring.virt() as *mut i16).add(frame * 2), n);
        }
        fence(Ordering::SeqCst);
    }
    fn position(&self) -> usize {
        (self.regs.r32(self.sd + SD_LPIB) as usize / 4) % RING_FRAMES
    }
    fn wait(&self, ms: u64) {
        let segment = self.position() / SEGMENT_FRAMES;
        if self.has_irq.load(Ordering::Relaxed) {
            self.irq.wait(ms, || self.position() / SEGMENT_FRAMES != segment);
        } else {
            crate::sched::sleep_ms(ms.min(10));
        }
    }
    fn poll_jack(&self) -> Option<bool> {
        let plugged = self.headphone_plugged()?;
        if self.headphones.swap(plugged, Ordering::Relaxed) != plugged {
            // Speakers go quiet while headphones are in.
            for p in self.pins.iter().filter(|p| p.device == DEV_SPEAKER) {
                self.enable_pin(p, !plugged);
            }
        }
        Some(plugged)
    }
}

/// Resets the controller and starts the command rings.
fn reset(regs: Regs) -> Option<Commands> {
    regs.w32(GCTL, regs.r32(GCTL) & !1);
    wait_for(100, || regs.r32(GCTL) & 1 == 0).ok()?;
    delay_us(200);
    regs.w32(GCTL, regs.r32(GCTL) | 1);
    wait_for(100, || regs.r32(GCTL) & 1 == 1).ok()?;
    delay_us(1000); // codecs announce themselves within 521 µs

    let mut cmd = Commands { regs, rings: None, corb_wp: 0, rirb_rp: 0 };
    let (Some(corb), Some(rirb)) = (Dma::new(1024), Dma::new(2048)) else { return Some(cmd) };
    // CORB: stop, 256 entries, base, reset the read pointer, run.
    regs.w8(CORBCTL, 0);
    let _ = wait_for(10, || regs.r8(CORBCTL) & 2 == 0);
    regs.w8(CORBSIZE, (regs.r8(CORBSIZE) & !3) | 2);
    regs.w32(CORBLBASE, corb.phys as u32);
    regs.w32(CORBUBASE, (corb.phys >> 32) as u32);
    regs.w16(CORBRP, 1 << 15);
    let _ = wait_for(10, || regs.r16(CORBRP) & (1 << 15) != 0);
    regs.w16(CORBRP, 0);
    let _ = wait_for(10, || regs.r16(CORBRP) & (1 << 15) == 0);
    regs.w16(CORBWP, 0);
    // RIRB: stop, 256 entries, base, reset the write pointer, run. The response
    // interrupt flag must be enabled: controllers stop taking commands after
    // RINTCNT responses until it is cleared (the controller-level interrupt,
    // INTCTL.CIE, stays off, so nothing is delivered).
    regs.w8(RIRBCTL, 0);
    let _ = wait_for(10, || regs.r8(RIRBCTL) & 2 == 0);
    regs.w8(RIRBSIZE, (regs.r8(RIRBSIZE) & !3) | 2);
    regs.w32(RIRBLBASE, rirb.phys as u32);
    regs.w32(RIRBUBASE, (rirb.phys >> 32) as u32);
    regs.w16(RIRBWP, 1 << 15);
    regs.w16(RINTCNT, 1);
    regs.w8(RIRBCTL, 0x3);
    regs.w8(CORBCTL, 2);
    cmd.rings = Some((corb, rirb));
    Some(cmd)
}

pub fn probe(dev: &pci::Device) -> Option<Arc<dyn Output>> {
    dev.enable();
    let regs = Regs(dev.map_bar(0)?);
    let gcap = regs.r16(GCAP) as u32;
    let (outputs, inputs) = ((gcap >> 12) & 0xF, (gcap >> 8) & 0xF);
    if outputs == 0 {
        log!("hda", "controller has no output streams");
        return None;
    }
    let cmd = reset(regs)?;
    let codecs = regs.r16(STATESTS);
    let (Some(ring), Some(bdl)) = (Dma::new(RING_FRAMES * 4), Dma::new(SEGMENTS * 16)) else { return None };
    let mut hda = Hda {
        regs,
        cmd: Mutex::new(cmd),
        sd: 0x80 + inputs as u64 * 0x20,
        ring,
        bdl: bdl,
        irq: WaitQueue::new(),
        has_irq: AtomicBool::new(false),
        cad: 0,
        pins: Vec::new(),
        headphones: AtomicBool::new(false),
        name: String::new(),
    };
    for cad in 0..15 {
        if codecs & (1 << cad) != 0 {
            hda.cad = cad;
            if set_up_codec(&mut hda) {
                break;
            }
        }
    }
    if hda.pins.is_empty() {
        log!("hda", "no codec with a usable output (codec mask {:#x})", codecs);
        return None;
    }
    start_stream(&hda);
    let hda: &'static Hda = Box::leak(Box::new(hda));
    if dev.enable_msi("hda", interrupt, hda as *const Hda as usize).is_some() {
        hda.has_irq.store(true, Ordering::Relaxed);
        regs.w32(INTCTL, 1 << 31 | 1 << inputs); // global + this stream
    }
    Some(Arc::new(HdaRef(hda)))
}

/// The mixer's handle to the controller (which lives as long as the system).
struct HdaRef(&'static Hda);

impl Output for HdaRef {
    fn describe(&self) -> String {
        self.0.describe()
    }
    fn frames(&self) -> usize {
        self.0.frames()
    }
    fn write(&self, frame: usize, samples: &[i16]) {
        self.0.write(frame, samples)
    }
    fn position(&self) -> usize {
        self.0.position()
    }
    fn wait(&self, ms: u64) {
        self.0.wait(ms)
    }
    fn poll_jack(&self) -> Option<bool> {
        self.0.poll_jack()
    }
}

/// Finds the audio function group, routes every output pin to a DAC.
fn set_up_codec(hda: &mut Hda) -> bool {
    let vendor = hda.param(0, P_VENDOR);
    if vendor == 0 || vendor == u32::MAX {
        return false;
    }
    let nodes = hda.param(0, P_NODES);
    let Some(afg) = ((nodes >> 16) & 0xFF..((nodes >> 16) & 0xFF) + (nodes & 0xFF))
        .find(|&n| hda.param(n, P_FG_TYPE) & 0xFF == 1)
    else {
        return false;
    };
    hda.command(verb(hda.cad, afg, SET_POWER, 0));
    delay_us(1000);
    let afg_in_amp = hda.param(afg, P_IN_AMP);
    let afg_out_amp = hda.param(afg, P_OUT_AMP);
    let sub = hda.param(afg, P_NODES);
    let first = (sub >> 16) & 0xFF;
    let mut widgets = BTreeMap::new();
    for nid in first..first + (sub & 0xFF) {
        let caps = hda.param(nid, P_WIDGET_CAP);
        let kind = (caps >> 20) & 0xF;
        let has_conns = caps & (1 << 8) != 0;
        // Bit 3: the widget has its own amp parameters.
        let own = caps & (1 << 3) != 0;
        widgets.insert(
            nid,
            Widget {
                kind,
                caps,
                conns: if has_conns { hda.connections(nid) } else { Vec::new() },
                pin_caps: if kind == W_PIN { hda.param(nid, P_PIN_CAP) } else { 0 },
                config: if kind == W_PIN { hda.command(verb(hda.cad, nid, GET_CONFIG, 0)) } else { 0 },
                in_amp: if own { hda.param(nid, P_IN_AMP) } else { afg_in_amp },
                out_amp: if own { hda.param(nid, P_OUT_AMP) } else { afg_out_amp },
            },
        );
    }
    // Output-capable pins that are wired up, speakers and headphones first.
    let mut pins: Vec<Pin> = widgets
        .iter()
        .filter(|(_, w)| w.kind == W_PIN && w.pin_caps & (1 << 4) != 0 && w.config >> 30 != 1)
        .map(|(&nid, w)| Pin {
            nid,
            device: (w.config >> 20) & 0xF,
            sense: w.pin_caps & (1 << 2) != 0,
            eapd: w.pin_caps & (1 << 16) != 0,
        })
        .filter(|p| matches!(p.device, DEV_LINE_OUT | DEV_SPEAKER | DEV_HEADPHONE))
        .collect();
    pins.sort_by_key(|p| match p.device {
        DEV_SPEAKER => 0,
        DEV_HEADPHONE => 1,
        _ => 2,
    });
    let mut used = Vec::new();
    let mut dacs = Vec::new();
    for pin in pins {
        let Some((path, dac)) = hda.find_dac(&widgets, pin.nid, 0) else { continue };
        hda.set_up_route(&widgets, &path, dac);
        hda.enable_pin(&pin, true);
        if !dacs.contains(&dac) {
            dacs.push(dac);
        }
        used.push(pin);
    }
    if used.is_empty() {
        return false;
    }
    let kinds: Vec<&str> = used
        .iter()
        .map(|p| match p.device {
            DEV_SPEAKER => "speaker",
            DEV_HEADPHONE => "headphones",
            _ => "line out",
        })
        .collect();
    hda.name = format!("Intel HD Audio, codec {:04x}:{:04x} ({})", vendor >> 16, vendor & 0xFFFF, kinds.join(", "));
    log!("hda", "codec {}: {} output pin(s) on {} DAC(s)", hda.cad, used.len(), dacs.len());
    hda.pins = used;
    true
}

/// Programs the output stream: a looping ring of segments, interrupt on each.
fn start_stream(hda: &Hda) {
    let (r, sd) = (hda.regs, hda.sd);
    r.w8(sd + SD_CTL, 0);
    let _ = wait_for(10, || r.r8(sd + SD_CTL) & SD_RUN == 0);
    r.w8(sd + SD_CTL, SD_RESET);
    let _ = wait_for(10, || r.r8(sd + SD_CTL) & SD_RESET != 0);
    r.w8(sd + SD_CTL, 0);
    let _ = wait_for(10, || r.r8(sd + SD_CTL) & SD_RESET == 0);
    let segment_bytes = SEGMENT_FRAMES * 4;
    for i in 0..SEGMENTS {
        let e = i * 16;
        hda.bdl.write::<u64>(e, hda.ring.phys + (i * segment_bytes) as u64);
        hda.bdl.write::<u32>(e + 8, segment_bytes as u32);
        hda.bdl.write::<u32>(e + 12, 1); // interrupt on completion
    }
    r.w32(sd + SD_BDPL, hda.bdl.phys as u32);
    r.w32(sd + SD_BDPU, (hda.bdl.phys >> 32) as u32);
    r.w32(sd + SD_CBL, (RING_FRAMES * 4) as u32);
    r.w16(sd + SD_LVI, (SEGMENTS - 1) as u16);
    r.w16(sd + SD_FMT, FORMAT);
    r.w8(sd + SD_CTL_STREAM, (STREAM_TAG << 4) as u8);
    r.w8(sd + SD_STS, SD_STS_ALL);
    r.w8(sd + SD_CTL, SD_RUN | SD_IOCE);
}
