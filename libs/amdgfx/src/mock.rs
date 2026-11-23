//! A simulated AMD display engine (DCN or DCE, by the [`Info`] it is made
//! for): a register file with the behaviour the driver relies on, for host
//! tests and the kernel's self-test, which runs the real driver against it
//! as if it were the GPU's register BAR.
//!
//! It simulates time (`delay_us` advances a clock; a 60 Hz vblank bumps
//! every running timing generator's frame counter and completes pending
//! flips), the flip-pending bits a surface address write sets, hot-plug
//! sense, and the AUX engines with DisplayPort sinks (the Intel mock's
//! [`Sink`]: DPCD and EDID over I²C-over-AUX).
//!
//! [`Mock::firmware`] programs what a GOP leaves behind.

use crate::platform::{Block, Info, Layout};
use crate::regs::{self, Inst};
use crate::takeover;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
pub use aurora_igfx::mock::{test_edid, Sink};
use aurora_igfx::timing::Timings;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

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

/// What a register does when written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// The low half of a surface address: arms a flip on this pipe.
    Surface(u8),
    AuxArb(u8),
    AuxData(u8),
    AuxControl(u8),
    AuxInt(u8),
}

#[derive(Default)]
struct Aux {
    buf: [u8; 32],
    sink: Option<Sink>,
}

struct State {
    regs: BTreeMap<u32, u32>,
    roles: BTreeMap<u32, Role>,
    time_us: u64,
    next_vblank_us: u64,
    aux: [Aux; 6],
    writes: u64,
    /// Flips the simulated engine applied.
    flips: u32,
}

pub struct Mock {
    pub info: Info,
    s: Lock<State>,
}

fn at(i: &Inst, n: u8) -> u32 {
    i[n as usize]
}

impl Mock {
    pub fn new(info: Info) -> Mock {
        let mut roles = BTreeMap::new();
        for n in 0..6u8 {
            let (surf, arb, data, ctl, int) = match info.block.layout() {
                Layout::Dcn(l) => (
                    at(&l.addr, n),
                    at(&l.aux_arb_control, n),
                    at(&l.aux_sw_data, n),
                    at(&l.aux_sw_control, n),
                    at(&l.aux_interrupt_control, n),
                ),
                Layout::Dce(l) => (
                    at(&l.grph_addr, n),
                    at(&l.aux_arb_control, n),
                    at(&l.aux_sw_data, n),
                    at(&l.aux_sw_control, n),
                    at(&l.aux_interrupt_control, n),
                ),
            };
            for (r, role) in [
                (surf, Role::Surface(n)),
                (arb, Role::AuxArb(n)),
                (data, Role::AuxData(n)),
                (ctl, Role::AuxControl(n)),
                (int, Role::AuxInt(n)),
            ] {
                if r != 0 {
                    roles.insert(r, role);
                }
            }
        }
        let st = State {
            regs: BTreeMap::new(),
            roles,
            time_us: 0,
            next_vblank_us: 16_667,
            aux: Default::default(),
            writes: 0,
            flips: 0,
        };
        Mock { info, s: Lock { held: AtomicBool::new(false), v: UnsafeCell::new(st) } }
    }

    /// Sets a register without side effects (test setup).
    pub fn poke(&self, reg: u32, v: u32) {
        if reg != 0 {
            self.s.with(|s| s.regs.insert(reg, v));
        }
    }

    pub fn peek(&self, reg: u32) -> u32 {
        self.s.with(|s| s.regs.get(&reg).copied().unwrap_or(0))
    }

    pub fn writes(&self) -> u64 {
        self.s.with(|s| s.writes)
    }

    pub fn flips(&self) -> u32 {
        self.s.with(|s| s.flips)
    }

    pub fn now_us(&self) -> u64 {
        self.s.with(|s| s.time_us)
    }

    /// Advances simulated time past the next vblank.
    pub fn vblank(&self) {
        let (until, now) = self.s.with(|s| (s.next_vblank_us, s.time_us));
        crate::Mmio::delay_us(&self, (until - now) as u32 + 1);
    }

    /// Something plugged into HPD pin `pin` (0-based), or unplugged.
    pub fn plug(&self, pin: u8, on: bool) {
        let r = match self.info.block.layout() {
            Layout::Dcn(l) => at(&l.hpd_status, pin),
            Layout::Dce(l) => at(&l.hpd_status, pin),
        };
        let v = self.peek(r) & !regs::HPD_SENSE;
        self.poke(r, v | if on { regs::HPD_SENSE } else { 0 });
    }

    /// A DisplayPort sink on AUX channel `ch` (and its HPD pin sensed).
    pub fn attach_sink(&self, ch: u8, sink: Sink) {
        self.s.with(|s| s.aux[ch as usize].sink = Some(sink));
        self.plug(ch, true);
    }

    pub fn detach_sink(&self, ch: u8) {
        self.s.with(|s| s.aux[ch as usize].sink = None);
        self.plug(ch, false);
    }

    /// Programs what a GOP leaves running: timing generator `otg` with
    /// timings `t`, scanned by `pipes` (DCN HUBPs side by side; DCE uses
    /// DCP `otg`) from a linear ARGB8888 surface at GPU address `addr` with
    /// `pitch` pixels per row, scanning `src` (the active size unless scaled).
    pub fn firmware(&self, otg: u8, t: &Timings, pipes: &[u8], addr: u64, pitch: u32, src: (u32, u32)) {
        let (h, hb, hs) = takeover::encode((t.hactive, t.hsync_start, t.hsync_end, t.htotal));
        let (v, vb, vs) = takeover::encode((t.vactive, t.vsync_start, t.vsync_end, t.vtotal));
        let hpol = if t.hsync_positive { 0 } else { regs::SYNC_POL_NEGATIVE };
        let vpol = if t.vsync_positive { 0 } else { regs::SYNC_POL_NEGATIVE };
        match self.info.block.layout() {
            Layout::Dcn(l) => {
                for (r, val) in [
                    (&l.otg_h_total, h),
                    (&l.otg_h_blank, hb),
                    (&l.otg_h_sync, hs),
                    (&l.otg_h_sync_cntl, hpol),
                    (&l.otg_v_total, v),
                    (&l.otg_v_blank, vb),
                    (&l.otg_v_sync, vs),
                    (&l.otg_v_sync_cntl, vpol),
                    (&l.otg_control, regs::MASTER_EN),
                    (&l.otg_frame_count, 1000),
                ] {
                    self.poke(at(r, otg), val);
                }
                let w = src.0 / pipes.len() as u32;
                for (k, &p) in pipes.iter().enumerate() {
                    self.poke(at(&l.hubp_cntl, p), (otg as u32) << regs::HUBP_VTG_SEL_SHIFT);
                    self.poke(at(&l.surface_config, p), regs::PIXEL_FORMAT_ARGB8888);
                    self.poke(at(&l.viewport_start, p), k as u32 * w);
                    self.poke(at(&l.viewport_dim, p), src.1 << 16 | w);
                    self.poke(at(&l.pitch, p), pitch);
                    self.poke(at(&l.addr, p), addr as u32);
                    self.poke(at(&l.addr_high, p), (addr >> 32) as u32);
                }
            }
            Layout::Dce(l) => {
                for (r, val) in [
                    (&l.crtc_h_total, h),
                    (&l.crtc_h_blank, hb),
                    (&l.crtc_h_sync, hs),
                    (&l.crtc_h_sync_cntl, hpol),
                    (&l.crtc_v_total, v),
                    (&l.crtc_v_blank, vb),
                    (&l.crtc_v_sync, vs),
                    (&l.crtc_v_sync_cntl, vpol),
                    (&l.crtc_control, regs::MASTER_EN),
                    (&l.crtc_frame_count, 1000),
                    (&l.grph_enable, regs::GRPH_ENABLE),
                    (
                        &l.grph_control,
                        regs::GRPH_DEPTH_32BPP
                            | if self.info.block == Block::Dce112 { 1 << regs::GRPH_ARRAY_MODE_SHIFT } else { 0 },
                    ),
                    (&l.grph_pitch, pitch),
                    (&l.grph_x_end, src.0),
                    (&l.grph_y_end, src.1),
                    (&l.grph_addr, addr as u32),
                    (&l.grph_addr_high, (addr >> 32) as u32),
                ] {
                    self.poke(at(r, otg), val);
                }
            }
        }
    }

    fn read_reg(s: &mut State, reg: u32) -> u32 {
        s.regs.get(&reg).copied().unwrap_or(0)
    }

    fn step(s: &mut State, info: &Info) {
        while s.time_us >= s.next_vblank_us {
            s.next_vblank_us += 16_667;
            match info.block.layout() {
                Layout::Dcn(l) => {
                    for n in 0..6u8 {
                        let c = at(&l.otg_control, n);
                        if c != 0 && Self::read_reg(s, c) & regs::MASTER_EN != 0 {
                            let f = at(&l.otg_frame_count, n);
                            let v = Self::read_reg(s, f).wrapping_add(1) & regs::FRAME_COUNT_MASK;
                            s.regs.insert(f, v);
                        }
                        let fc = at(&l.flip_control, n);
                        let Block::Dcn(ver) = info.block else { continue };
                        let bit = regs::flip_pending_bit(ver);
                        if fc != 0 && Self::read_reg(s, fc) & bit != 0 {
                            let v = Self::read_reg(s, fc) & !bit;
                            s.regs.insert(fc, v);
                            s.flips += 1;
                        }
                    }
                }
                Layout::Dce(l) => {
                    for n in 0..6u8 {
                        let c = at(&l.crtc_control, n);
                        if c != 0 && Self::read_reg(s, c) & regs::MASTER_EN != 0 {
                            let f = at(&l.crtc_frame_count, n);
                            let v = Self::read_reg(s, f).wrapping_add(1) & regs::FRAME_COUNT_MASK;
                            s.regs.insert(f, v);
                        }
                        let u = at(&l.grph_update, n);
                        if u != 0 && Self::read_reg(s, u) & regs::GRPH_SURFACE_UPDATE_PENDING != 0 {
                            let v = Self::read_reg(s, u) & !regs::GRPH_SURFACE_UPDATE_PENDING;
                            s.regs.insert(u, v);
                            s.flips += 1;
                        }
                    }
                }
            }
        }
    }

    fn aux_regs(info: &Info, n: u8) -> (u32, u32) {
        match info.block.layout() {
            Layout::Dcn(l) => (at(&l.aux_sw_status, n), at(&l.aux_sw_data, n)),
            Layout::Dce(l) => (at(&l.aux_sw_status, n), at(&l.aux_sw_data, n)),
        }
    }

    fn write_reg(s: &mut State, info: &Info, reg: u32, v: u32) {
        s.writes += 1;
        let role = s.roles.get(&reg).copied();
        match role {
            Some(Role::Surface(p)) => {
                s.regs.insert(reg, v);
                match (info.block, info.block.layout()) {
                    (Block::Dcn(ver), Layout::Dcn(l)) => {
                        let fc = at(&l.flip_control, p);
                        let old = Self::read_reg(s, fc);
                        s.regs.insert(fc, old | regs::flip_pending_bit(ver));
                    }
                    (_, Layout::Dce(l)) => {
                        let u = at(&l.grph_update, p);
                        let old = Self::read_reg(s, u);
                        s.regs.insert(u, old | regs::GRPH_SURFACE_UPDATE_PENDING);
                    }
                    _ => {}
                }
            }
            Some(Role::AuxArb(_)) => {
                let mut x = v & regs::AUX_ARB_PRIORITY_MASK;
                if v & regs::AUX_SW_USE_AUX_REG_REQ != 0 {
                    x |= regs::AUX_REG_RW_CNTL_STATUS_SW;
                }
                s.regs.insert(reg, x);
            }
            Some(Role::AuxData(n)) => {
                let i = ((v >> regs::AUX_SW_INDEX_SHIFT) & 0x1F) as usize;
                let a = &mut s.aux[n as usize];
                let out = if v & regs::AUX_SW_DATA_RW != 0 {
                    (a.buf[i] as u32) << regs::AUX_SW_DATA_SHIFT
                } else {
                    a.buf[i] = (v >> regs::AUX_SW_DATA_SHIFT) as u8;
                    0
                };
                s.regs.insert(reg, out);
            }
            Some(Role::AuxControl(n)) => {
                s.regs.insert(reg, v & !regs::AUX_SW_GO);
                if v & regs::AUX_SW_GO == 0 {
                    return;
                }
                let (status, _) = Self::aux_regs(info, n);
                let len = ((v >> regs::AUX_SW_WR_BYTES_SHIFT) & 0x1F) as usize;
                let a = &mut s.aux[n as usize];
                let msg: Vec<u8> = a.buf[..len.min(20)].to_vec();
                let st = match a.sink.as_mut() {
                    Some(sink) => {
                        let reply = sink.handle(&msg);
                        for (i, b) in reply.iter().enumerate().take(20) {
                            a.buf[i] = *b;
                        }
                        regs::AUX_SW_DONE | (reply.len() as u32) << regs::AUX_SW_REPLY_BYTE_COUNT_SHIFT
                    }
                    None => regs::AUX_SW_DONE | regs::AUX_SW_RX_TIMEOUT,
                };
                s.regs.insert(status, st);
            }
            Some(Role::AuxInt(n)) => {
                if v & regs::AUX_SW_DONE_ACK != 0 {
                    let (status, _) = Self::aux_regs(info, n);
                    let old = Self::read_reg(s, status);
                    s.regs.insert(status, old & !regs::AUX_SW_DONE);
                }
                s.regs.insert(reg, v & !regs::AUX_SW_DONE_ACK);
            }
            None => {
                s.regs.insert(reg, v);
            }
        }
    }
}

impl crate::Mmio for Mock {
    fn read(&self, reg: u32) -> u32 {
        self.s.with(|s| Self::read_reg(s, reg))
    }

    fn write(&self, reg: u32, value: u32) {
        let info = self.info;
        self.s.with(|s| Self::write_reg(s, &info, reg, value));
    }

    fn delay_us(&self, us: u32) {
        let info = self.info;
        self.s.with(|s| {
            s.time_us += us as u64;
            Self::step(s, &info);
        });
    }
}
