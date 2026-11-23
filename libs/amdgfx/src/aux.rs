//! The DisplayPort AUX engine of DCE and DCN (`DP_AUXn_*`), as an
//! [`aurora_igfx::aux::Transport`]: the DisplayPort side (DEFER retries,
//! DPCD reads, I²C-over-AUX, EDID) is the Intel driver's, shared.
//!
//! One transaction: request the engine's registers from the arbiter
//! (`AUX_ARB_CONTROL`; the VBIOS's microcontroller may use the engine too),
//! write the message into the engine's buffer byte by byte (`AUX_SW_DATA`,
//! each byte at an explicit index), start it (`AUX_SW_CONTROL`: byte count
//! and go), poll `AUX_SW_STATUS` for done, read the reply back the same
//! way, acknowledge done and hand the registers back.
//!
//! The field positions are from AMD's headers (identical on every listed
//! block); the arbitration state value for "software owns it" and the
//! explicit-index access are UNCONFIRMED.

use crate::platform::{Info, Layout};
use crate::regs;
use crate::Mmio;
use aurora_igfx::aux::{AuxError, Transport};

/// AUX channel `ch` (0-based, A = 0) of the display block.
pub struct Engine<'a, M: Mmio> {
    pub mmio: &'a M,
    pub info: Info,
    pub ch: u8,
}

struct Regs {
    control: u32,
    sw_control: u32,
    arb: u32,
    int: u32,
    status: u32,
    data: u32,
}

impl<M: Mmio> Engine<'_, M> {
    fn regs(&self) -> Option<Regs> {
        let i = self.ch as usize;
        let (c, s, a, n, st, d) = match self.info.block.layout() {
            Layout::Dcn(l) => (
                l.aux_control,
                l.aux_sw_control,
                l.aux_arb_control,
                l.aux_interrupt_control,
                l.aux_sw_status,
                l.aux_sw_data,
            ),
            Layout::Dce(l) => (
                l.aux_control,
                l.aux_sw_control,
                l.aux_arb_control,
                l.aux_interrupt_control,
                l.aux_sw_status,
                l.aux_sw_data,
            ),
        };
        let r = Regs {
            control: *c.get(i)?,
            sw_control: *s.get(i)?,
            arb: *a.get(i)?,
            int: *n.get(i)?,
            status: *st.get(i)?,
            data: *d.get(i)?,
        };
        (r.control != 0 && r.data != 0).then_some(r)
    }

    /// One transfer with the engine held; the caller releases it.
    fn once(&self, r: &Regs, send: &[u8], recv: &mut [u8; 20]) -> Result<usize, AuxError> {
        let m = self.mmio;
        m.write(r.int, m.read(r.int) | regs::AUX_SW_DONE_ACK);
        for (i, b) in send.iter().enumerate() {
            m.write(
                r.data,
                regs::AUX_SW_AUTOINCREMENT_DISABLE
                    | (i as u32) << regs::AUX_SW_INDEX_SHIFT
                    | (*b as u32) << regs::AUX_SW_DATA_SHIFT,
            );
        }
        m.write(r.sw_control, (send.len() as u32) << regs::AUX_SW_WR_BYTES_SHIFT | regs::AUX_SW_GO);
        if !m.wait_for(r.status, regs::AUX_SW_DONE, regs::AUX_SW_DONE, 10_000) {
            return Err(AuxError::Busy);
        }
        let status = m.read(r.status);
        m.write(r.int, m.read(r.int) | regs::AUX_SW_DONE_ACK);
        if status & (regs::AUX_SW_RX_TIMEOUT | regs::AUX_SW_HPD_DISCON | regs::AUX_SW_NON_AUX_MODE) != 0 {
            return Err(AuxError::Timeout);
        }
        if status & regs::AUX_SW_RX_ERRORS != 0 {
            return Err(AuxError::Receive);
        }
        let n = ((status & regs::AUX_SW_REPLY_BYTE_COUNT_MASK) >> regs::AUX_SW_REPLY_BYTE_COUNT_SHIFT) as usize;
        if n == 0 || n > 20 {
            return Err(AuxError::Receive);
        }
        for (i, out) in recv.iter_mut().enumerate().take(n) {
            m.write(
                r.data,
                regs::AUX_SW_AUTOINCREMENT_DISABLE | (i as u32) << regs::AUX_SW_INDEX_SHIFT | regs::AUX_SW_DATA_RW,
            );
            *out = (m.read(r.data) >> regs::AUX_SW_DATA_SHIFT) as u8;
        }
        Ok(n)
    }
}

impl<M: Mmio> Transport for Engine<'_, M> {
    /// Retries receive errors three times (as the specification asks),
    /// not timeouts.
    fn raw(&self, send: &[u8], recv: &mut [u8; 20]) -> Result<usize, AuxError> {
        let r = self.regs().ok_or(AuxError::Timeout)?;
        let m = self.mmio;
        if m.read(r.control) & regs::AUX_EN == 0 {
            m.write(r.control, m.read(r.control) | regs::AUX_EN);
        }
        m.write(r.arb, m.read(r.arb) & regs::AUX_ARB_PRIORITY_MASK | regs::AUX_SW_USE_AUX_REG_REQ);
        if !m.wait_for(r.arb, regs::AUX_REG_RW_CNTL_STATUS_MASK, regs::AUX_REG_RW_CNTL_STATUS_SW, 10_000) {
            return Err(AuxError::Busy);
        }
        let mut result = Err(AuxError::Receive);
        for _ in 0..4 {
            result = self.once(&r, send, recv);
            if result != Err(AuxError::Receive) {
                break;
            }
            m.delay_us(400);
        }
        m.write(r.arb, m.read(r.arb) & regs::AUX_ARB_PRIORITY_MASK | regs::AUX_SW_DONE_USING_AUX_REG);
        result
    }

    fn delay_us(&self, us: u32) {
        self.mmio.delay_us(us)
    }
}
