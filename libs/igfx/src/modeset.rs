//! Full mode sets on Skylake-class displays (display 9 big cores: Skylake,
//! Kaby, Coffee and Comet Lake). Used only with `intel=modeset` in
//! boot.conf; the default driver never calls this.
//!
//! What is covered, following the "Sequences for DisplayPort" and
//! "Sequences for HDMI and DVI" pages of the Skylake PRM (Volume 12):
//!
//! - **Disable**: backlight off (eDP), planes off, transcoder off (wait
//!   for its state), DDI function off, DDI buffer off (wait idle), DP
//!   training pattern off, the port's clock gated, its DPLL off (never
//!   DPLL 0, which also runs the CD clock). eDP panel power stays on.
//! - **Enable**: DPLL (HDMI mode with dividers from [`crate::dpll::hdmi`],
//!   or a DP link rate code), the port's clock routed, eDP panel power
//!   (with the power sequencer's delays), for DP the training pattern and
//!   DDI buffer then link training over AUX ([`crate::dp::train`]) and the
//!   idle pattern, timings, `PIPESRC`, DP M/N, the DDI function, the
//!   transcoder, the normal pattern, backlight (eDP).
//!
//! Limits, all checked by [`plan`]:
//!
//! - Only the pipe, transcoder, port and output type the firmware lit;
//!   the firmware's buffer translations (`DDI_BUF_TRANS`) are reused, not
//!   programmed, and no power wells are touched. Lighting a port the
//!   firmware didn't light needs the VBT and the PRM's translation tables:
//!   not done.
//! - Only modes whose pixel clock, line rate and width are no larger than
//!   the firmware's: the firmware's display buffer allocation and
//!   watermarks then remain sufficient (both watermark methods grow only
//!   with those), so they are kept as they are. Larger modes need the
//!   memory latencies from the PCU and a watermark computation: not done.
//! - The CD clock is never changed.
//! - eDP keeps the panel's timings (no pipe scaler).
//!
//! A failed enable is undone and the previous configuration enabled again
//! ([`switch`]); the kernel then keeps the previous framebuffer.

use crate::aux::{AuxCh, AuxError};
use crate::dp::{self, dpcd, Drive, Link, SinkCaps, Trainer, MN};
use crate::platform::{Info, Pch};
use crate::regs::{self, Transcoder};
use crate::takeover::{Output, Pipe};
use crate::timing::{self, Timings};
use crate::{dpll, Mmio};

/// Everything needed to light one pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub pipe: u8,
    pub trans: Transcoder,
    pub port: u8,
    pub output: Output,
    pub timings: Timings,
    pub clock_khz: u32,
    pub dpll: u8,
    /// HDMI mode: `DPLL_CFGCR1/2`.
    pub cfgcr: Option<(u32, u32)>,
    /// DP: the link and its M/N values.
    pub link: Option<Link>,
    pub mn: Option<MN>,
    pub enhanced_framing: bool,
    pub tps3: bool,
    pub rate_table_index: Option<u8>,
    /// `DDI_BUF_CTL` as the firmware had it (lane width, HDMI translation entry).
    pub ddi_buf_ctl: u32,
    /// `TRANS_DDI_FUNC_CTL` without the enable bit.
    pub ddi_func: u32,
    pub cr_interval_us: u32,
    pub eq_interval_us: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refuse {
    /// Not a Skylake-class display.
    Platform,
    /// The firmware's DPLL setting can't be read back.
    Unreadable,
    /// Bigger than the firmware's mode in pixel clock, line rate or width.
    LargerThanFirmware,
    /// eDP panels keep their timings.
    PanelTimings,
    /// No DPLL setting makes this clock.
    Clock,
    /// The link can't carry the mode.
    Bandwidth,
    /// The sink's capabilities couldn't be read.
    Sink,
}

/// Errors while enabling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fail {
    DpllLock,
    PanelPower,
    BufferIdle,
    Aux(AuxError),
    ClockRecovery,
    ChannelEq,
    TranscoderState,
}

fn dpll_of_port(m: &impl Mmio, port: u8) -> Option<u8> {
    let ctrl2 = m.read(regs::DPLL_CTRL2);
    (ctrl2 & regs::ddi_sel_override(port) != 0)
        .then(|| ((ctrl2 & regs::ddi_clk_sel_mask(port)) >> regs::ddi_clk_sel_shift(port)) as u8)
}

/// The DP sink's capabilities over AUX channel `ch`.
pub fn sink_caps(m: &impl Mmio, ch: u8) -> Result<SinkCaps, AuxError> {
    let aux = AuxCh { mmio: m, ch };
    let mut caps = [0u8; 16];
    aux.read(dpcd::REV, &mut caps)?;
    let mut table = [0u8; 16];
    let edp14 = aux.read_byte(dpcd::EDP_DPCD_REV).unwrap_or(0) >= 3;
    let has_table = edp14 && aux.read(dpcd::SUPPORTED_LINK_RATES, &mut table).is_ok() && table[0] | table[1] != 0;
    SinkCaps::parse(&caps, has_table.then_some(&table)).ok_or(AuxError::Short)
}

/// The firmware's configuration of a lit pipe, as a [`Config`].
pub fn snapshot(m: &impl Mmio, info: &Info, p: &Pipe, clock_khz: u32) -> Result<Config, Refuse> {
    if !info.skylake_class() {
        return Err(Refuse::Platform);
    }
    let id = dpll_of_port(m, p.port).ok_or(Refuse::Unreadable)?;
    let ctrl1 = m.read(regs::DPLL_CTRL1);
    let hdmi = ctrl1 & regs::dpll_ctrl1_hdmi(id) != 0;
    let (cfgcr, link, mn) = if hdmi {
        (Some((m.read(regs::dpll_cfgcr1(id)), m.read(regs::dpll_cfgcr2(id)))), None, None)
    } else {
        let code = (ctrl1 >> regs::dpll_ctrl1_link_rate_shift(id)) & 7;
        let rate = dpll::dp_rate_from_code(code).ok_or(Refuse::Unreadable)?;
        let mn = MN {
            data_m: m.read(regs::data_m1(p.trans)) & 0xFF_FFFF,
            data_n: m.read(regs::data_n1(p.trans)) & 0xFF_FFFF,
            link_m: m.read(regs::link_m1(p.trans)) & 0xFF_FFFF,
            link_n: m.read(regs::link_n1(p.trans)) & 0xFF_FFFF,
        };
        (None, Some(Link { rate, lanes: p.lanes }), Some(mn))
    };
    let tp = m.read(regs::dp_tp_ctl(p.port));
    Ok(Config {
        pipe: p.pipe,
        trans: p.trans,
        port: p.port,
        output: p.output,
        timings: p.timings,
        clock_khz,
        dpll: id,
        cfgcr,
        link,
        mn,
        enhanced_framing: tp & regs::DP_TP_ENHANCED_FRAMING != 0,
        tps3: false,
        rate_table_index: None,
        ddi_buf_ctl: m.read(regs::ddi_buf_ctl(p.port)),
        ddi_func: p.ddi_func & !regs::DDI_FUNC_ENABLE,
        cr_interval_us: 100,
        eq_interval_us: 400,
    })
}

/// A configuration for `timings` at `clock_khz` on the pipe and port of
/// `base` (the firmware's), or why not. `sink` is needed for DP.
pub fn plan(
    info: &Info,
    base: &Config,
    timings: &Timings,
    clock_khz: u32,
    sink: Option<&SinkCaps>,
) -> Result<Config, Refuse> {
    if !info.skylake_class() {
        return Err(Refuse::Platform);
    }
    let line_rate = |c: u32, t: &Timings| c as u64 * 1000 / t.htotal.max(1) as u64;
    if clock_khz > base.clock_khz
        || line_rate(clock_khz, timings) > line_rate(base.clock_khz, &base.timings)
        || timings.hactive > base.timings.hactive
    {
        return Err(Refuse::LargerThanFirmware);
    }
    // eDP: the panel's own timings only (re-enabling what the firmware lit).
    if base.output == Output::Edp {
        return if *timings == base.timings { Ok(*base) } else { Err(Refuse::PanelTimings) };
    }
    if !timings.sane() {
        return Err(Refuse::Clock);
    }
    let mut c = *base;
    c.timings = *timings;
    c.clock_khz = clock_khz;
    let mut func = base.ddi_func & !(regs::DDI_FUNC_HSYNC_HIGH | regs::DDI_FUNC_VSYNC_HIGH);
    if timings.hsync_positive {
        func |= regs::DDI_FUNC_HSYNC_HIGH;
    }
    if timings.vsync_positive {
        func |= regs::DDI_FUNC_VSYNC_HIGH;
    }
    match base.output {
        Output::Hdmi | Output::Dvi => {
            if base.dpll == 0 {
                return Err(Refuse::Clock);
            }
            let w = dpll::hdmi(clock_khz).ok_or(Refuse::Clock)?;
            c.cfgcr = Some((w.cfgcr1, w.cfgcr2));
        }
        Output::Dp | Output::Edp => {
            let sink = sink.ok_or(Refuse::Sink)?;
            // The firmware's colour depth (and so its dithering) stays.
            let bpp = 3 * crate::takeover::bpc(base.ddi_func) as u32;
            // DPLL 0 keeps its rate (it runs the CD clock); other DPLLs may change.
            let rates: alloc::vec::Vec<u32> = if base.dpll == 0 {
                alloc::vec![base.link.ok_or(Refuse::Unreadable)?.rate]
            } else {
                alloc::vec![162_000, 216_000, 270_000, 324_000, 432_000, 540_000]
            };
            let lanes = base.link.map_or(4, |l| l.lanes);
            let link = dp::choose_link(clock_khz, bpp, &rates, lanes, sink).ok_or(Refuse::Bandwidth)?;
            c.link = Some(link);
            c.mn = Some(dp::m_n(clock_khz, bpp, link));
            c.enhanced_framing = sink.enhanced_framing;
            c.tps3 = sink.tps3;
            c.rate_table_index =
                if sink.rate_table { sink.rates.iter().position(|&r| r == link.rate).map(|i| i as u8) } else { None };
            c.cr_interval_us = sink.cr_interval_us;
            c.eq_interval_us = sink.eq_interval_us;
            func &= !regs::DDI_FUNC_DP_WIDTH_MASK;
            func |= (link.lanes as u32 - 1).min(3) << regs::DDI_FUNC_DP_WIDTH_SHIFT;
        }
    }
    c.ddi_func = func;
    Ok(c)
}

// ------------------------------------------------------------ sequences

fn pp_write(m: &impl Mmio, info: &Info, clear: u32, set: u32) {
    let mut v = m.read(regs::PP_CONTROL);
    v = (v & !clear) | set;
    if info.pch == Pch::Spt {
        v = (v & 0xFFFF) | regs::PP_UNLOCK;
    }
    m.write(regs::PP_CONTROL, v);
}

/// The power sequencer's delays in µs: (power up T1+T3, backlight on T8,
/// backlight off T9, power down T10), all in 100 µs units in the registers.
pub fn pps_delays(m: &impl Mmio) -> (u32, u32, u32, u32) {
    let on = m.read(regs::PP_ON_DELAYS);
    let off = m.read(regs::PP_OFF_DELAYS);
    ((on >> 16 & 0x1FFF) * 100, (on & 0x1FFF) * 100, (off & 0x1FFF) * 100, (off >> 16 & 0x1FFF) * 100)
}

fn backlight_off(m: &impl Mmio, info: &Info) {
    pp_write(m, info, regs::PP_BLC_ENABLE, 0);
    m.delay_us(pps_delays(m).2);
}

fn backlight_on(m: &impl Mmio, info: &Info) {
    m.delay_us(pps_delays(m).1);
    pp_write(m, info, 0, regs::PP_BLC_ENABLE);
}

fn panel_on(m: &impl Mmio, info: &Info) -> Result<(), Fail> {
    if m.read(regs::PP_STATUS) & regs::PP_ON != 0 {
        return Ok(());
    }
    // A power cycle must finish before the panel may come on again.
    m.wait_for(regs::PP_STATUS, regs::PP_CYCLE_DELAY_ACTIVE, 0, 2_000_000);
    pp_write(m, info, 0, regs::PP_POWER_TARGET_ON | regs::PP_RESET);
    let wait = pps_delays(m).0 + 500_000;
    if !m.wait_for(regs::PP_STATUS, regs::PP_ON | regs::PP_SEQUENCE_MASK, regs::PP_ON, wait) {
        return Err(Fail::PanelPower);
    }
    Ok(())
}

/// Turns the pipe of `c` off (panel power stays on).
pub fn disable(m: &impl Mmio, info: &Info, c: &Config) {
    if c.output == Output::Edp {
        backlight_off(m, info);
    }
    m.write(regs::curcntr(c.pipe), 0);
    m.write(regs::curbase(c.pipe), m.read(regs::curbase(c.pipe)));
    m.write(regs::plane_ctl(c.pipe, 0), 0);
    m.write(regs::plane_surf(c.pipe, 0), m.read(regs::plane_surf(c.pipe, 0)));
    m.modify(regs::transconf(c.trans), regs::TRANSCONF_ENABLE, 0);
    m.wait_for(regs::transconf(c.trans), regs::TRANSCONF_STATE, 0, 100_000);
    m.modify(regs::trans_ddi_func_ctl(c.trans), regs::DDI_FUNC_ENABLE, 0);
    m.modify(regs::ddi_buf_ctl(c.port), regs::DDI_BUF_ENABLE, 0);
    m.wait_for(regs::ddi_buf_ctl(c.port), regs::DDI_BUF_IDLE, regs::DDI_BUF_IDLE, 10_000);
    if c.link.is_some() {
        m.modify(regs::dp_tp_ctl(c.port), regs::DP_TP_ENABLE, 0);
    }
    m.modify(regs::DPLL_CTRL2, 0, regs::ddi_clk_off(c.port));
    // The DPLL goes off unless another running port still uses it.
    let shared = (0..5u8).any(|p| p != c.port && running_on(m, p) == Some(c.dpll));
    if c.dpll != 0 && !shared {
        m.modify(regs::skl_dpll_enable(c.dpll), regs::DPLL_ENABLE, 0);
    }
}

/// The DPLL clocking `port`, if its clock is on.
fn running_on(m: &impl Mmio, port: u8) -> Option<u8> {
    let ctrl2 = m.read(regs::DPLL_CTRL2);
    (ctrl2 & regs::ddi_clk_off(port) == 0).then(|| dpll_of_port(m, port)).flatten()
}

struct SklTrainer<'a, M: Mmio> {
    m: &'a M,
    aux: AuxCh<'a, M>,
    port: u8,
}

impl<M: Mmio> Trainer for SklTrainer<'_, M> {
    type Error = AuxError;
    fn set_pattern(&mut self, tp: u8) -> Result<(), AuxError> {
        let src = match tp {
            1 => regs::DP_TP_PATTERN_1,
            2 => regs::DP_TP_PATTERN_2,
            3 => regs::DP_TP_PATTERN_3,
            _ => regs::DP_TP_IDLE,
        };
        self.m.modify(regs::dp_tp_ctl(self.port), regs::DP_TP_PATTERN_MASK, src);
        let v = if tp == 0 { 0 } else { tp | dpcd::SCRAMBLING_DISABLE };
        self.aux.write_byte(dpcd::TRAINING_PATTERN_SET, v)
    }
    fn set_drive(&mut self, d: Drive, lanes: u8) -> Result<(), AuxError> {
        self.m.modify(
            regs::ddi_buf_ctl(self.port),
            regs::DDI_BUF_TRANS_SELECT_MASK,
            d.skl_entry() << regs::DDI_BUF_TRANS_SELECT_SHIFT,
        );
        let set = [d.lane_set(); 4];
        self.aux.write(dpcd::TRAINING_LANE0_SET, &set[..lanes as usize])
    }
    fn status(&mut self) -> Result<[u8; 6], AuxError> {
        let mut s = [0u8; 6];
        self.aux.read(dpcd::LANE0_1_STATUS, &mut s)?;
        Ok(s)
    }
    fn delay_us(&mut self, us: u32) {
        self.m.delay_us(us)
    }
}

/// Lights the pipe of `c`.
pub fn enable(m: &impl Mmio, info: &Info, c: &Config) -> Result<(), Fail> {
    // 1. The DPLL.
    if c.dpll != 0 {
        let id = c.dpll;
        let group = regs::dpll_ctrl1_override(id)
            | match (c.cfgcr, c.link) {
                (Some(_), _) => regs::dpll_ctrl1_hdmi(id),
                (None, Some(l)) => dpll::dp_rate_code(l.rate).unwrap_or(1) << regs::dpll_ctrl1_link_rate_shift(id),
                _ => 0,
            };
        m.modify(regs::DPLL_CTRL1, regs::dpll_ctrl1_mask(id), group);
        m.read(regs::DPLL_CTRL1);
        if let Some((c1, c2)) = c.cfgcr {
            m.write(regs::dpll_cfgcr1(id), c1);
            m.write(regs::dpll_cfgcr2(id), c2);
            m.read(regs::dpll_cfgcr2(id));
        }
        m.modify(regs::skl_dpll_enable(id), 0, regs::DPLL_ENABLE);
    }
    if !m.wait_for(regs::DPLL_STATUS, regs::dpll_lock(c.dpll), regs::dpll_lock(c.dpll), 5_000) {
        return Err(Fail::DpllLock);
    }
    // 2. The port's clock from that DPLL.
    m.modify(
        regs::DPLL_CTRL2,
        regs::ddi_clk_sel_mask(c.port) | regs::ddi_clk_off(c.port),
        (c.dpll as u32) << regs::ddi_clk_sel_shift(c.port) | regs::ddi_sel_override(c.port),
    );
    // 3. Panel power.
    if c.output == Output::Edp {
        panel_on(m, info)?;
    }
    // 4. The DDI and, for DP, the link.
    let buf = c.ddi_buf_ctl & !(regs::DDI_BUF_ENABLE | regs::DDI_BUF_IDLE);
    if let Some(link) = c.link {
        let aux = AuxCh { mmio: m, ch: info.aux_ch(c.port) };
        aux.write_byte(dpcd::SET_POWER, 1).map_err(Fail::Aux)?;
        let mut tp = regs::DP_TP_ENABLE | regs::DP_TP_PATTERN_1;
        if c.enhanced_framing {
            tp |= regs::DP_TP_ENHANCED_FRAMING;
        }
        m.write(regs::dp_tp_ctl(c.port), tp);
        let width = (link.lanes as u32 - 1).min(3) << regs::DDI_BUF_WIDTH_SHIFT;
        let buf = (buf & !(regs::DDI_BUF_WIDTH_MASK | regs::DDI_BUF_TRANS_SELECT_MASK)) | width;
        m.write(regs::ddi_buf_ctl(c.port), buf | regs::DDI_BUF_ENABLE);
        m.read(regs::ddi_buf_ctl(c.port));
        m.delay_us(600);
        match c.rate_table_index {
            Some(i) => aux.write_byte(dpcd::LINK_RATE_SET, i).map_err(Fail::Aux)?,
            None => aux.write_byte(dpcd::LINK_BW_SET, dp::bw_code(link.rate)).map_err(Fail::Aux)?,
        }
        let ef = if c.enhanced_framing { 0x80 } else { 0 };
        aux.write_byte(dpcd::LANE_COUNT_SET, link.lanes | ef).map_err(Fail::Aux)?;
        let mut t = SklTrainer { m, aux, port: c.port };
        dp::train(&mut t, link.lanes, c.tps3, c.cr_interval_us, c.eq_interval_us).map_err(|e| match e {
            dp::TrainError::Io(a) => Fail::Aux(a),
            dp::TrainError::ClockRecovery => Fail::ClockRecovery,
            dp::TrainError::ChannelEq => Fail::ChannelEq,
        })?;
        m.wait_for(regs::dp_tp_status(c.port), regs::DP_TP_STATUS_IDLE_DONE, regs::DP_TP_STATUS_IDLE_DONE, 2_000);
    } else {
        m.write(regs::ddi_buf_ctl(c.port), buf | regs::DDI_BUF_ENABLE);
        m.read(regs::ddi_buf_ctl(c.port));
        m.delay_us(600);
    }
    // 5. The transcoder and pipe.
    timing::write(m, c.trans, &c.timings.to_regs());
    m.write(regs::pipesrc(c.pipe), timing::pipesrc(c.timings.hactive, c.timings.vactive));
    m.write(regs::plane_size(c.pipe, 0), timing::pack(c.timings.hactive, c.timings.vactive));
    m.write(regs::plane_pos(c.pipe, 0), 0);
    if let Some(mn) = c.mn {
        m.write(regs::data_m1(c.trans), 63 << regs::TU_SIZE_SHIFT | mn.data_m);
        m.write(regs::data_n1(c.trans), mn.data_n);
        m.write(regs::link_m1(c.trans), mn.link_m);
        m.write(regs::link_n1(c.trans), mn.link_n);
    }
    m.write(regs::trans_ddi_func_ctl(c.trans), c.ddi_func | regs::DDI_FUNC_ENABLE);
    m.modify(regs::transconf(c.trans), regs::TRANSCONF_INTERLACE_MASK, regs::TRANSCONF_ENABLE);
    if !m.wait_for(regs::transconf(c.trans), regs::TRANSCONF_STATE, regs::TRANSCONF_STATE, 100_000) {
        return Err(Fail::TranscoderState);
    }
    if c.link.is_some() {
        m.modify(regs::dp_tp_ctl(c.port), regs::DP_TP_PATTERN_MASK, regs::DP_TP_NORMAL);
    }
    if c.output == Output::Edp {
        backlight_on(m, info);
    }
    Ok(())
}

/// What [`switch`] ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    Done,
    /// `to` failed; `from` is back on.
    Restored(Fail),
    /// Both failed: the pipe is dark.
    Dark(Fail, Fail),
}

/// Disables `from`, enables `to`; if that fails, enables `from` again.
pub fn switch(m: &impl Mmio, info: &Info, from: &Config, to: &Config) -> Switch {
    disable(m, info, from);
    match enable(m, info, to) {
        Ok(()) => Switch::Done,
        Err(e) => {
            disable(m, info, to);
            match enable(m, info, from) {
                Ok(()) => Switch::Restored(e),
                Err(e2) => Switch::Dark(e, e2),
            }
        }
    }
}
