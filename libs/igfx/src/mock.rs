//! A simulated display engine: a register file with the behaviour the
//! driver relies on, for host tests and the kernel's self-test (which runs
//! the real driver against it as if it were a GPU's BAR).
//!
//! It simulates time (`delay_us` advances a clock; a 60 Hz vblank bumps
//! `PIPE_FRMCOUNT` and latches `PIPE_VBLANK` in the pipe's IIR), flips
//! (`PLANE_SURF` is live at the next vblank, with the flip-done bit), the
//! status bits sequences wait on (transcoder state, DPLL lock, DDI idle,
//! panel power), GMBUS with an EDID on chosen pins, and AUX channels with
//! a DisplayPort sink (DPCD, EDID over I²C-over-AUX, link training that
//! succeeds once the drive reaches the swing the sink wants).
//!
//! [`Mock::firmware`] programs what a GOP leaves behind: one lit pipe.

use crate::aux;
use crate::dp::dpcd;
use crate::gmbus;
use crate::platform::Info;
use crate::regs::{self, Transcoder};
use crate::timing::{self, Timings};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// A spin lock, so the mock is `Sync` without dependencies.
struct Lock<T> {
    held: AtomicBool,
    v: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        while self.held.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        let r = f(unsafe { &mut *self.v.get() });
        self.held.store(false, Ordering::Release);
        r
    }
}

/// A DisplayPort sink on an AUX channel.
#[derive(Clone, Debug)]
pub struct Sink {
    pub dpcd: Vec<u8>,
    pub edid: Vec<u8>,
    /// Clock recovery succeeds once every lane is driven at least this swing.
    pub cr_swing: u8,
    /// Link trainings that reached channel equalization.
    pub trained: u32,
    i2c_offset: usize,
    segment: usize,
}

impl Sink {
    /// A sink with `max_rate_code` (`MAX_LINK_RATE`) and `lanes`.
    pub fn new(max_rate_code: u8, lanes: u8, edid: Vec<u8>) -> Sink {
        let mut dpcd = alloc::vec![0u8; 0x800];
        dpcd[0] = 0x12; // DPCD 1.2
        dpcd[1] = max_rate_code;
        dpcd[2] = lanes | dpcd::ENHANCED_FRAME_CAP;
        dpcd[dpcd::SINK_COUNT as usize] = 1;
        Sink { dpcd, edid, cr_swing: 1, trained: 0, i2c_offset: 0, segment: 0 }
    }
}

#[derive(Default)]
struct State {
    regs: BTreeMap<u32, u32>,
    time_us: u64,
    next_vblank_us: u64,
    /// GMBUS: EDIDs by pin, the bytes left to hand out, the selected pin.
    ddc: BTreeMap<u32, Vec<u8>>,
    gmbus_rx: VecDeque<u8>,
    gmbus_segment: usize,
    sinks: BTreeMap<u8, Sink>,
    /// Pending surfaces per pipe (live at the next vblank).
    pending: BTreeMap<u8, u32>,
    pending_cursor: BTreeMap<u8, u32>,
    gtt: BTreeMap<u64, u64>,
    gtt_flushes: u32,
    writes: u64,
    pipes: u8,
    /// DPLLs that never lock (bit per id).
    broken_dpll: u8,
}

pub struct Mock {
    s: Lock<State>,
}

impl Default for Mock {
    fn default() -> Self {
        Mock::new(3)
    }
}

impl Mock {
    pub fn new(pipes: u8) -> Mock {
        let st = State { pipes, next_vblank_us: 16_667, ..State::default() };
        Mock { s: Lock { held: AtomicBool::new(false), v: UnsafeCell::new(st) } }
    }

    /// Sets a register without side effects (test setup).
    pub fn poke(&self, reg: u32, v: u32) {
        self.s.with(|s| s.regs.insert(reg, v));
    }

    pub fn peek(&self, reg: u32) -> u32 {
        self.s.with(|s| s.regs.get(&reg).copied().unwrap_or(0))
    }

    pub fn now_us(&self) -> u64 {
        self.s.with(|s| s.time_us)
    }

    pub fn writes(&self) -> u64 {
        self.s.with(|s| s.writes)
    }

    /// An EDID on GMBUS pin `pin`.
    pub fn attach_ddc(&self, pin: u32, edid: Vec<u8>) {
        self.s.with(|s| s.ddc.insert(pin, edid));
    }

    pub fn detach_ddc(&self, pin: u32) {
        self.s.with(|s| s.ddc.remove(&pin));
    }

    /// A DisplayPort sink on AUX channel `ch`.
    pub fn attach_sink(&self, ch: u8, sink: Sink) {
        self.s.with(|s| s.sinks.insert(ch, sink));
    }

    pub fn sink(&self, ch: u8) -> Option<Sink> {
        self.s.with(|s| s.sinks.get(&ch).cloned())
    }

    /// DPLL `id` will never report lock (to test failed mode sets).
    pub fn set_dpll_broken(&self, id: u8) {
        self.s.with(|s| s.broken_dpll |= 1 << id);
    }

    pub fn gtt_entry(&self, index: u64) -> u64 {
        self.s.with(|s| s.gtt.get(&index).copied().unwrap_or(0))
    }

    pub fn gtt_flushes(&self) -> u32 {
        self.s.with(|s| s.gtt_flushes)
    }

    /// Advances simulated time to the next vblank.
    pub fn vblank(&self) {
        let until = self.s.with(|s| s.next_vblank_us);
        let now = self.now_us();
        crate::Mmio::delay_us(&self, (until - now) as u32 + 1);
    }

    /// Programs what a GOP leaves running: `trans` feeding `pipe` and DDI
    /// `port` in `mode` (`DDI_MODE_*`), with `t`, plane 1 scanning a linear
    /// XRGB8888 surface at GGTT `surf` with `stride` bytes per row, a DDB
    /// slice and watermarks, and (for eDP) the panel on with its PWM
    /// backlight at half.
    #[allow(clippy::too_many_arguments)]
    pub fn firmware(
        &self,
        info: &Info,
        trans: Transcoder,
        pipe: u8,
        port: u8,
        mode: u32,
        t: &Timings,
        surf: u32,
        stride: u32,
    ) {
        let r = t.to_regs();
        for (reg, v) in [
            (regs::htotal(trans), r.htotal),
            (regs::hblank(trans), r.hblank),
            (regs::hsync(trans), r.hsync),
            (regs::vtotal(trans), r.vtotal),
            (regs::vblank(trans), r.vblank),
            (regs::vsync(trans), r.vsync),
        ] {
            self.poke(reg, v);
        }
        self.poke(regs::pipesrc(pipe), timing::pipesrc(t.hactive, t.vactive));
        self.poke(regs::transconf(trans), regs::TRANSCONF_ENABLE | regs::TRANSCONF_STATE);
        let port_bits = if info.display >= 12 {
            (port as u32 + 1) << regs::DDI_FUNC_PORT_SHIFT_TGL
        } else {
            (port as u32) << regs::DDI_FUNC_PORT_SHIFT
        };
        let edp_input = if trans == Transcoder::Edp { [4, 5, 6, 7][pipe as usize] << 12 } else { 0 };
        let mut func = regs::DDI_FUNC_ENABLE | port_bits | mode << regs::DDI_FUNC_MODE_SHIFT | edp_input;
        if mode == regs::DDI_MODE_DP_SST {
            func |= 3 << regs::DDI_FUNC_DP_WIDTH_SHIFT;
        }
        if t.hsync_positive {
            func |= regs::DDI_FUNC_HSYNC_HIGH;
        }
        if t.vsync_positive {
            func |= regs::DDI_FUNC_VSYNC_HIGH;
        }
        self.poke(regs::trans_ddi_func_ctl(trans), func);
        self.poke(regs::ddi_buf_ctl(port), regs::DDI_BUF_ENABLE | 3 << regs::DDI_BUF_WIDTH_SHIFT);
        self.poke(regs::plane_ctl(pipe, 0), regs::PLANE_CTL_ENABLE | regs::PLANE_CTL_FORMAT_8888);
        self.poke(regs::plane_stride(pipe, 0), stride / regs::STRIDE_UNIT);
        self.poke(regs::plane_size(pipe, 0), timing::pack(t.hactive, t.vactive));
        self.poke(regs::plane_surf(pipe, 0), surf);
        self.poke(regs::plane_surflive(pipe, 0), surf);
        // The whole pipe's display buffer to plane 1 (blocks 0–859), and
        // levels 0–5 on, as the firmware typically leaves them.
        self.poke(regs::plane_buf_cfg(pipe, 0), 859 << 16);
        for (l, (lines, blocks)) in [(2, 30), (3, 60), (4, 90), (6, 150), (8, 240), (12, 400)].iter().enumerate() {
            self.poke(regs::plane_wm(pipe, 0, l as u8), 1 << 31 | lines << 14 | blocks);
        }
        if mode == regs::DDI_MODE_DP_SST && (trans == Transcoder::Edp || port == 0) {
            self.poke(regs::PP_STATUS, regs::PP_ON);
            self.poke(regs::PP_CONTROL, regs::PP_POWER_TARGET_ON | regs::PP_BLC_ENABLE);
            self.poke(regs::BLC_PWM_CTL1, regs::BLC_PWM_ENABLE);
            match info.backlight() {
                crate::platform::BacklightLayout::Split16 => self.poke(regs::BLC_PWM_CTL2, 1000 << 16 | 500),
                crate::platform::BacklightLayout::Bxt => {
                    self.poke(regs::BLC_PWM_CTL2, 1000);
                    self.poke(regs::BLC_PWM_DUTY, 500);
                }
            }
        }
        // CD clock 675 MHz on display 9–10 ((675 − 1) × 2).
        self.poke(regs::CDCLK_CTL, 1348);
    }

    // ------------------------------------------------------ behaviour

    fn tick(s: &mut State) {
        while s.time_us >= s.next_vblank_us {
            s.next_vblank_us += 16_667;
            for p in 0..s.pipes {
                let mut on = false;
                for t in [Transcoder::Pipe(p), Transcoder::Edp] {
                    let conf = s.regs.get(&regs::transconf(t)).copied().unwrap_or(0);
                    let func = s.regs.get(&regs::trans_ddi_func_ctl(t)).copied().unwrap_or(0);
                    let feeds = match t {
                        Transcoder::Edp => crate::takeover::edp_input_pipe(func) == p && func != 0,
                        _ => true,
                    };
                    on |= conf & regs::TRANSCONF_ENABLE != 0 && feeds;
                }
                if !on {
                    continue;
                }
                *s.regs.entry(regs::frame_count(p)).or_insert(0) += 1;
                let mut iir = regs::PIPE_VBLANK;
                if let Some(surf) = s.pending.remove(&p) {
                    s.regs.insert(regs::plane_surflive(p, 0), surf);
                    iir |= regs::PIPE_PLANE1_FLIP_DONE;
                }
                if let Some(base) = s.pending_cursor.remove(&p) {
                    s.regs.insert(regs::cur_surflive(p), base);
                }
                let imr = s.regs.get(&regs::de_pipe_imr(p)).copied().unwrap_or(0);
                *s.regs.entry(regs::de_pipe_iir(p)).or_insert(0) |= iir & !imr;
            }
        }
    }

    fn gmbus_write(s: &mut State, reg: u32, v: u32) {
        match reg {
            gmbus::GMBUS1 if v & gmbus::SW_RDY != 0 => {
                let pin = s.regs.get(&gmbus::GMBUS0).copied().unwrap_or(0) & 0x1F;
                let addr = (v >> 1) & 0x7F;
                let count = ((v >> gmbus::BYTE_COUNT_SHIFT) & 0x1FF) as usize;
                let mut status = gmbus::ACTIVE;
                if v & gmbus::CYCLE_STOP != 0 && count == 0 {
                    s.regs.insert(gmbus::GMBUS2, 0);
                    return;
                }
                let present = s.ddc.contains_key(&pin) && (addr == 0x50 || addr == 0x30);
                if !present {
                    status |= gmbus::SATOER;
                } else if addr == 0x30 {
                    s.gmbus_segment = (s.regs.get(&gmbus::GMBUS3).copied().unwrap_or(0) & 0xFF) as usize;
                    status |= gmbus::HW_WAIT_PHASE | gmbus::HW_RDY;
                } else if v & gmbus::READ != 0 {
                    let edid = &s.ddc[&pin];
                    let index = if v & gmbus::CYCLE_INDEX != 0 { (v >> 8 & 0xFF) as usize } else { 0 };
                    let start = s.gmbus_segment * 256 + index;
                    s.gmbus_segment = 0;
                    s.gmbus_rx = (0..count).map(|i| edid.get(start + i).copied().unwrap_or(0xFF)).collect();
                    status |= gmbus::HW_RDY;
                } else {
                    status |= gmbus::HW_WAIT_PHASE | gmbus::HW_RDY;
                }
                s.regs.insert(gmbus::GMBUS2, status);
            }
            gmbus::GMBUS1 if v & gmbus::SW_CLR_INT != 0 => {
                s.regs.insert(gmbus::GMBUS2, 0);
                s.gmbus_rx.clear();
            }
            _ => {}
        }
        s.regs.insert(reg, v);
    }

    fn gmbus_read(s: &mut State, reg: u32) -> u32 {
        if reg == gmbus::GMBUS3 && !s.gmbus_rx.is_empty() {
            let mut b = [0u8; 4];
            for x in b.iter_mut() {
                *x = s.gmbus_rx.pop_front().unwrap_or(0);
            }
            if s.gmbus_rx.is_empty() {
                let st = s.regs.entry(gmbus::GMBUS2).or_insert(0);
                *st = (*st & !gmbus::HW_RDY) | gmbus::HW_WAIT_PHASE;
            }
            return u32::from_le_bytes(b);
        }
        s.regs.get(&reg).copied().unwrap_or(0)
    }

    fn aux_write(s: &mut State, ch: u8, v: u32) {
        let c = aux::ctl(ch);
        if v & aux::SEND_BUSY == 0 {
            // Clearing status bits.
            let old = s.regs.get(&c).copied().unwrap_or(0);
            let w1c = aux::DONE | aux::TIME_OUT_ERROR | aux::RECEIVE_ERROR;
            s.regs.insert(c, old & !(v & w1c));
            return;
        }
        let len = ((v & aux::SIZE_MASK) >> aux::SIZE_SHIFT) as usize;
        let mut data = [0u32; 5];
        for (i, d) in data.iter_mut().enumerate() {
            *d = s.regs.get(&(c + 4 + i as u32 * 4)).copied().unwrap_or(0);
        }
        let mut msg = [0u8; 20];
        aux::unpack(&data, &mut msg[..len.min(20)]);
        let Some(sink) = s.sinks.get_mut(&ch) else {
            s.regs.insert(c, aux::TIME_OUT_ERROR | aux::DONE);
            return;
        };
        let reply = sink.handle(&msg[..len.min(20)]);
        let packed = aux::pack(&reply);
        for (i, d) in packed.iter().enumerate() {
            s.regs.insert(c + 4 + i as u32 * 4, *d);
        }
        s.regs.insert(c, aux::DONE | (reply.len() as u32) << aux::SIZE_SHIFT);
    }
}

impl Sink {
    /// The sink's reply to one AUX request (header and data): the reply
    /// byte, then any data. Shared by every simulated AUX controller.
    pub fn handle(&mut self, msg: &[u8]) -> Vec<u8> {
        let sink = self;
        let len = msg.len();
        let mut msg20 = [0u8; 20];
        msg20[..len.min(20)].copy_from_slice(&msg[..len.min(20)]);
        let msg = msg20;
        let cmd = msg[0] >> 4;
        let addr = ((msg[0] as u32 & 0xF) << 16 | (msg[1] as u32) << 8 | msg[2] as u32) as usize;
        let n = if len > 3 { msg[3] as usize + 1 } else { 0 };
        let mut reply: Vec<u8> = alloc::vec![0];
        match cmd {
            aux::NATIVE_READ => reply.extend((0..n).map(|i| sink.dpcd.get(addr + i).copied().unwrap_or(0))),
            aux::NATIVE_WRITE => {
                for i in 0..n {
                    if let Some(b) = sink.dpcd.get_mut(addr + i) {
                        *b = msg[4 + i];
                    }
                }
                Mock::train_step(sink);
            }
            c if c & !aux::I2C_MOT == aux::I2C_WRITE => match addr {
                0x50 if n > 0 => sink.i2c_offset = msg[4] as usize,
                0x30 if n > 0 => sink.segment = msg[4] as usize,
                0x50 | 0x30 => {}
                _ => reply[0] = 0x40, // I²C NACK
            },
            c if c & !aux::I2C_MOT == aux::I2C_READ => match addr {
                0x50 => {
                    let start = sink.segment * 256 + sink.i2c_offset;
                    reply.extend((0..n).map(|i| sink.edid.get(start + i).copied().unwrap_or(0xFF)));
                    sink.i2c_offset += n;
                    if c & aux::I2C_MOT == 0 {
                        sink.segment = 0;
                    }
                }
                _ => reply[0] = 0x40,
            },
            _ => reply[0] = 0x10, // native NACK
        }
        reply
    }
}

impl Mock {
    /// The sink's side of link training, after any DPCD write.
    fn train_step(sink: &mut Sink) {
        let lanes = (sink.dpcd[dpcd::LANE_COUNT_SET as usize] & 0x1F).max(1);
        let tp = sink.dpcd[dpcd::TRAINING_PATTERN_SET as usize] & 3;
        let swing = sink.dpcd[dpcd::TRAINING_LANE0_SET as usize] & 3;
        let (mut s01, mut s23, mut align) = (0u8, 0u8, 0u8);
        let lane_bits = |bits: u8| -> (u8, u8) {
            let mut a = 0u8;
            let mut b = 0u8;
            for l in 0..lanes {
                if l < 2 {
                    a |= bits << (4 * l);
                } else {
                    b |= bits << (4 * (l - 2));
                }
            }
            (a, b)
        };
        let cr = swing >= sink.cr_swing;
        if tp == 1 && cr {
            (s01, s23) = lane_bits(dpcd::CR_DONE);
        } else if (tp == 2 || tp == 3) && cr {
            (s01, s23) = lane_bits(dpcd::CR_DONE | dpcd::CHANNEL_EQ_DONE | dpcd::SYMBOL_LOCKED);
            align = dpcd::INTERLANE_ALIGN_DONE;
            if sink.dpcd[0x202] & dpcd::CHANNEL_EQ_DONE == 0 {
                sink.trained += 1;
            }
        }
        if tp == 0 {
            return;
        }
        sink.dpcd[0x202] = s01;
        sink.dpcd[0x203] = s23;
        sink.dpcd[0x204] = align;
        // Ask for the swing it wants on every lane.
        let want = sink.cr_swing & 3;
        let (a, b) = lane_bits(want);
        sink.dpcd[0x206] = a;
        sink.dpcd[0x207] = b;
    }

    fn write_reg(s: &mut State, reg: u32, v: u32) {
        s.writes += 1;
        if (gmbus::GMBUS0..=gmbus::GMBUS5).contains(&reg) {
            return Self::gmbus_write(s, reg, v);
        }
        for ch in 0..9u8 {
            if reg == aux::ctl(ch) {
                return Self::aux_write(s, ch, v);
            }
        }
        // Interrupt identity registers: write one to clear.
        for p in 0..4u8 {
            if reg == regs::de_pipe_iir(p) {
                let old = s.regs.get(&reg).copied().unwrap_or(0);
                s.regs.insert(reg, old & !v);
                return;
            }
        }
        if reg == regs::SDE_IIR || reg == regs::DE_PORT_IIR || reg == regs::DE_HPD_IIR {
            let old = s.regs.get(&reg).copied().unwrap_or(0);
            s.regs.insert(reg, old & !v);
            return;
        }
        s.regs.insert(reg, v);
        for p in 0..4u8 {
            if reg == regs::plane_surf(p, 0) {
                s.pending.insert(p, v & !0xFFF);
            }
            if reg == regs::curbase(p) {
                s.pending_cursor.insert(p, v);
            }
        }
        for t in [Transcoder::Pipe(0), Transcoder::Pipe(1), Transcoder::Pipe(2), Transcoder::Pipe(3), Transcoder::Edp] {
            if reg == regs::transconf(t) {
                let state = if v & regs::TRANSCONF_ENABLE != 0 { regs::TRANSCONF_STATE } else { 0 };
                s.regs.insert(reg, (v & !regs::TRANSCONF_STATE) | state);
            }
        }
        for port in 0..9u8 {
            if reg == regs::ddi_buf_ctl(port) {
                let idle = if v & regs::DDI_BUF_ENABLE != 0 { 0 } else { regs::DDI_BUF_IDLE };
                s.regs.insert(reg, (v & !regs::DDI_BUF_IDLE) | idle);
            }
            if reg == regs::dp_tp_ctl(port) {
                s.regs.insert(regs::dp_tp_status(port), regs::DP_TP_STATUS_IDLE_DONE);
            }
        }
        for id in 0..4u8 {
            if reg == regs::skl_dpll_enable(id) {
                let st = s.regs.entry(regs::DPLL_STATUS).or_insert(0);
                if v & regs::DPLL_ENABLE != 0 && s.broken_dpll & 1 << id == 0 {
                    *st |= regs::dpll_lock(id);
                } else {
                    *st &= !regs::dpll_lock(id);
                }
            }
        }
        if reg == regs::PP_CONTROL {
            let on = v & regs::PP_POWER_TARGET_ON != 0;
            s.regs.insert(regs::PP_STATUS, if on { regs::PP_ON } else { 0 });
        }
    }
}

impl crate::Mmio for Mock {
    fn read(&self, reg: u32) -> u32 {
        self.s.with(|s| {
            if (gmbus::GMBUS0..=gmbus::GMBUS5).contains(&reg) {
                return Self::gmbus_read(s, reg);
            }
            s.regs.get(&reg).copied().unwrap_or(0)
        })
    }

    fn write(&self, reg: u32, v: u32) {
        self.s.with(|s| Self::write_reg(s, reg, v))
    }

    fn delay_us(&self, us: u32) {
        self.s.with(|s| {
            s.time_us += us as u64;
            Self::tick(s);
        })
    }
}

impl crate::gtt::Table for Mock {
    fn set(&self, index: u64, pte: u64) {
        self.s.with(|s| {
            if pte == 0 {
                s.gtt.remove(&index);
            } else {
                s.gtt.insert(index, pte);
            }
        });
    }
    fn get(&self, index: u64) -> u64 {
        self.gtt_entry(index)
    }
    fn flush(&self) {
        self.s.with(|s| s.gtt_flushes += 1);
    }
}

/// A 128-byte EDID with one detailed timing (1920×1080 at 60 Hz, 148.5
/// MHz) and a monitor name, checksummed: what test sinks report.
pub fn test_edid(name: &[u8]) -> Vec<u8> {
    let mut e = alloc::vec![0u8; 128];
    e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    e[8] = 0x24; // "IGF"
    e[9] = 0xE6;
    e[18] = 1;
    e[19] = 4;
    e[20] = 0x80; // digital
    e[21] = 53;
    e[22] = 30;
    // Detailed timing 1: 148.5 MHz, 1920 + 280 blank, 1080 + 45 blank,
    // hsync at +88 for 44, vsync at +4 for 5, 531×299 mm, +h +v sync.
    let dtd =
        [0x02, 0x3A, 0x80, 0x18, 0x71, 0x38, 0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0x13, 0x2B, 0x21, 0x00, 0x00, 0x1E];
    e[54..72].copy_from_slice(&dtd);
    // Monitor name descriptor.
    e[72..77].copy_from_slice(&[0, 0, 0, 0xFC, 0]);
    let mut n = [b' '; 13];
    n[..name.len().min(12)].copy_from_slice(&name[..name.len().min(12)]);
    n[name.len().min(12)] = b'\n';
    e[77..90].copy_from_slice(&n);
    for d in [90usize, 108] {
        e[d..d + 5].copy_from_slice(&[0, 0, 0, 0x10, 0]);
    }
    let sum: u8 = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    e[127] = 0u8.wrapping_sub(sum);
    e
}
