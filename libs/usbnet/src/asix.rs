//! ASIX's USB Ethernet chips: the AX88772 family (USB 2.0, 100 Mb/s: the
//! classic cheap adapter, Apple's USB Ethernet Adapter) and the AX88179 /
//! AX88178A (USB 3.0, gigabit). Both are driven with vendor control
//! requests; frames on the bulk endpoints carry ASIX's own headers.

use crate::{le32, put32, Error, MAX_FRAME, MIN_FRAME};
use alloc::vec::Vec;

/// Vendor request types: device-to-host and host-to-device.
pub const READ: u8 = 0xC0;
pub const WRITE: u8 = 0x40;

/// AX88772 commands (bRequest).
pub mod ax772 {
    pub const SET_SW_MII: u8 = 0x06;
    pub const READ_MII_REG: u8 = 0x07;
    pub const WRITE_MII_REG: u8 = 0x08;
    pub const SET_HW_MII: u8 = 0x0A;
    pub const WRITE_RX_CTL: u8 = 0x10;
    pub const WRITE_IPG0: u8 = 0x12;
    pub const READ_NODE_ID: u8 = 0x13;
    pub const WRITE_NODE_ID: u8 = 0x14;
    pub const READ_PHY_ID: u8 = 0x19;
    pub const WRITE_MEDIUM_MODE: u8 = 0x1B;
    pub const WRITE_GPIOS: u8 = 0x1F;
    pub const SW_RESET: u8 = 0x20;
    pub const SW_PHY_SELECT: u8 = 0x22;

    // RX control: start operation, broadcast, all multicast.
    pub const RX_CTL_SO: u16 = 0x0080;
    pub const RX_CTL_AB: u16 = 0x0008;
    pub const RX_CTL_AMALL: u16 = 0x0002;

    // Medium mode.
    pub const MEDIUM_FD: u16 = 0x0002;
    pub const MEDIUM_AC: u16 = 0x0004;
    pub const MEDIUM_RFC: u16 = 0x0010;
    pub const MEDIUM_TFC: u16 = 0x0020;
    pub const MEDIUM_RE: u16 = 0x0100;
    pub const MEDIUM_PS: u16 = 0x0200;
    pub const MEDIUM_DEFAULT: u16 = MEDIUM_FD | MEDIUM_RFC | MEDIUM_TFC | MEDIUM_PS | MEDIUM_AC | MEDIUM_RE;

    pub const GPIO_GPO2EN: u16 = 0x10;
    pub const GPIO_GPO_2: u16 = 0x20;
    pub const GPIO_RSE: u16 = 0x80;

    pub const SWRESET_CLEAR: u16 = 0x00;
    pub const SWRESET_PRL: u16 = 0x08;
    pub const SWRESET_IPRL: u16 = 0x20;
    pub const SWRESET_IPPD: u16 = 0x40;

    /// Inter-packet gaps (IPG0 | IPG1 << 8, and IPG2 in wIndex).
    pub const IPG01: u16 = 0x15 | 0x0C << 8;
    pub const IPG2: u16 = 0x12;

    /// The embedded PHY's address.
    pub const EMBEDDED_PHY: u8 = 0x10;
}

/// AX88179 / AX88178A: registers behind two "commands" (MAC and PHY access).
pub mod ax179 {
    pub const ACCESS_MAC: u8 = 0x01;
    pub const ACCESS_PHY: u8 = 0x02;
    pub const PHY_ID: u16 = 0x03;

    pub const RX_CTL: u16 = 0x0B;
    pub const NODE_ID: u16 = 0x10;
    pub const MULTI_FILTER: u16 = 0x16;
    pub const MEDIUM_STATUS_MODE: u16 = 0x22;
    pub const MONITOR_MODE: u16 = 0x24;
    pub const PHYPWR_RSTCTL: u16 = 0x26;
    pub const RX_BULKIN_QCTRL: u16 = 0x2E;
    pub const CLK_SELECT: u16 = 0x33;
    pub const RXCOE_CTL: u16 = 0x34;
    pub const TXCOE_CTL: u16 = 0x35;
    pub const PAUSE_WATERLVL_HIGH: u16 = 0x54;
    pub const PAUSE_WATERLVL_LOW: u16 = 0x55;

    pub const PHYPWR_RSTCTL_IPRL: u16 = 0x0020;
    pub const CLK_SELECT_BCS: u8 = 0x01;
    pub const CLK_SELECT_ACS: u8 = 0x02;

    pub const RX_CTL_AMALL: u16 = 0x0002;
    pub const RX_CTL_AB: u16 = 0x0008;
    pub const RX_CTL_AP: u16 = 0x0020;
    pub const RX_CTL_START: u16 = 0x0080;
    pub const RX_CTL_DROPCRCERR: u16 = 0x0100;
    pub const RX_CTL_IPE: u16 = 0x0200;

    pub const MEDIUM_GIGAMODE: u16 = 0x0001;
    pub const MEDIUM_FULL_DUPLEX: u16 = 0x0002;
    pub const MEDIUM_RXFLOW_CTRLEN: u16 = 0x0010;
    pub const MEDIUM_TXFLOW_CTRLEN: u16 = 0x0020;
    pub const MEDIUM_RECEIVE_EN: u16 = 0x0100;
    pub const MEDIUM_PS: u16 = 0x0200;

    pub const MONITOR_PMETYPE: u8 = 0x40;
    pub const MONITOR_PMEPOL: u8 = 0x20;
    pub const MONITOR_RWMP: u8 = 0x04;

    /// The PHY's specific status register and its bits.
    pub const PHY_PHYSR: u16 = 0x11;
    pub const PHYSR_GIGA: u16 = 0x8000;
    pub const PHYSR_100: u16 = 0x4000;
    pub const PHYSR_FULL: u16 = 0x2000;
    pub const PHYSR_LINK: u16 = 0x0400;

    /// Bulk-in aggregation (control, timer low/high, size, gap): by link
    /// and bus speed, as ASIX's reference driver sets it.
    pub const BULKIN_GIGA_USB3: [u8; 5] = [7, 0x4F, 0, 0x12, 0xFF];
    pub const BULKIN_GIGA_USB2: [u8; 5] = [7, 0x20, 3, 0x16, 0xFF];
    pub const BULKIN_100_FAST_BUS: [u8; 5] = [7, 0xAE, 7, 0x18, 0xFF];
    pub const BULKIN_SLOW: [u8; 5] = [7, 0xCC, 0x4C, 0x18, 8];
}

/// The standard MII registers both families' PHYs have.
pub mod mii {
    pub const BMCR: u16 = 0x00;
    pub const BMSR: u16 = 0x01;
    pub const ADVERTISE: u16 = 0x04;
    pub const LPA: u16 = 0x05;
    pub const BMCR_ANENABLE: u16 = 0x1000;
    pub const BMCR_ANRESTART: u16 = 0x0200;
    pub const BMCR_RESET: u16 = 0x8000;
    pub const BMSR_LSTATUS: u16 = 0x0004;
    /// 10/100, half and full, and the 802.3 selector.
    pub const ADVERTISE_ALL: u16 = 0x01E1;
    pub const LPA_100FULL: u16 = 0x0100;
    pub const LPA_100HALF: u16 = 0x0080;
    pub const LPA_10FULL: u16 = 0x0040;

    /// Speed (Mb/s) and full duplex agreed by autonegotiation.
    pub fn negotiated(advertise: u16, partner: u16) -> (u32, bool) {
        let common = advertise & partner;
        if common & LPA_100FULL != 0 {
            (100, true)
        } else if common & LPA_100HALF != 0 {
            (100, false)
        } else {
            (10, common & LPA_10FULL != 0)
        }
    }
}

/// The AX88772's interrupt endpoint report: is the link up?
pub fn ax772_link(report: &[u8]) -> Option<bool> {
    (report.len() >= 8).then(|| report[2] & 1 != 0)
}

/// The AX88179's: the same, in bit 16 of the first word.
pub fn ax179_link(report: &[u8]) -> Option<bool> {
    (report.len() >= 8).then(|| le32(report, 0).unwrap_or(0) & 1 << 16 != 0)
}

/// AX88772 transmit: a 4-byte header (length and its complement), and 4
/// bytes of padding when the result would fill whole USB packets (the chip
/// wants a short packet but no zero-length one).
pub fn ax772_tx_len(frame_len: usize, max_packet: usize) -> usize {
    let n = 4 + frame_len;
    if max_packet > 0 && n.is_multiple_of(max_packet) {
        n + 4
    } else {
        n
    }
}

/// Writes an AX88772 transmit packet into `buf`; returns its length.
pub fn ax772_tx(buf: &mut [u8], frame: &[u8], max_packet: usize) -> usize {
    let len = frame.len() as u32 & 0x7FF;
    put32(buf, 0, (!len & 0xFFFF) << 16 | len);
    buf[4..4 + frame.len()].copy_from_slice(frame);
    let total = ax772_tx_len(frame.len(), max_packet);
    if total > 4 + frame.len() {
        put32(buf, 4 + frame.len(), 0xFFFF_0000);
    }
    total
}

/// AX88772 receive: frames behind 4-byte headers, padded to an even
/// length, on a stream that runs across transfers.
#[derive(Default)]
pub struct Ax772Receiver {
    carry: Vec<u8>,
    /// A frame's padding byte still to come (the frame ended a transfer).
    skip: bool,
    pub errors: u64,
}

impl Ax772Receiver {
    pub fn feed(&mut self, data: &[u8], out: &mut dyn FnMut(&[u8])) {
        let mut buf = core::mem::take(&mut self.carry);
        let data = if core::mem::take(&mut self.skip) { data.get(1..).unwrap_or(&[]) } else { data };
        buf.extend_from_slice(data);
        let mut at = 0;
        while at + 4 <= buf.len() {
            let h = le32(&buf, at).unwrap_or(0);
            let len = (h & 0x7FF) as usize;
            if len != ((!h >> 16) & 0x7FF) as usize || len > MAX_FRAME {
                // Lost the frame boundaries: start over with the next transfer.
                self.errors += 1;
                at = buf.len();
                break;
            }
            if buf.len() - at < 4 + len {
                break;
            }
            if len >= MIN_FRAME {
                out(&buf[at + 4..at + 4 + len]);
            }
            at = (at + 4 + len + 1) & !1;
        }
        self.skip = at > buf.len();
        buf.drain(..at.min(buf.len()));
        self.carry = buf;
    }
}

/// AX88179 transmit header (8 bytes): the length, and a flag asking the
/// chip to pad when the transfer would fill whole USB packets.
pub fn ax179_tx(buf: &mut [u8], frame: &[u8], max_packet: usize) -> usize {
    put32(buf, 0, frame.len() as u32);
    let pad = max_packet > 0 && (8 + frame.len()).is_multiple_of(max_packet);
    put32(buf, 4, if pad { 0x8000_8000 } else { 0 });
    buf[8..8 + frame.len()].copy_from_slice(frame);
    8 + frame.len()
}

/// AX88179 receive header bits.
const RXHDR_CRC_ERR: u32 = 1 << 29;
const RXHDR_DROP_ERR: u32 = 1 << 31;

/// AX88179 receive: one transfer holds several frames; its last 4 bytes
/// count them and give the offset of a table of 4-byte headers. Each frame
/// starts with 2 bytes of padding (for IP alignment) and is padded to 8.
pub fn ax179_rx(b: &[u8], out: &mut dyn FnMut(&[u8])) -> Result<usize, Error> {
    if b.len() < 4 {
        return Err(Error::Truncated);
    }
    let end = b.len() - 4;
    let trailer = le32(b, end).ok_or(Error::Truncated)?;
    let count = (trailer & 0xFFFF) as usize;
    let table = (trailer >> 16) as usize;
    if count == 0 {
        return Ok(0);
    }
    if table > end || count > (end - table) / 4 {
        return Err(Error::Truncated);
    }
    let mut at = 0;
    let mut frames = 0;
    for k in 0..count {
        let h = le32(b, table + 4 * k).ok_or(Error::Truncated)?;
        let len = ((h >> 16) & 0x1FFF) as usize;
        let padded = (len + 7) & !7;
        if len == 0 {
            continue;
        }
        if at + padded > table && at + len > table {
            return Err(Error::Truncated);
        }
        if h & (RXHDR_CRC_ERR | RXHDR_DROP_ERR) == 0 && (2 + MIN_FRAME..=2 + MAX_FRAME).contains(&len) {
            out(&b[at + 2..at + len]);
            frames += 1;
        }
        at += padded;
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn ax772_round_trip() {
        let frames: Vec<Vec<u8>> = (0..6).map(|k| vec![k as u8; 59 + 251 * k]).collect();
        let mut stream = Vec::new();
        for f in &frames {
            let mut b = vec![0u8; 2048];
            let n = ax772_tx(&mut b, f, 512);
            // The receive side pads to even lengths; transmit headers match it.
            stream.extend_from_slice(&b[..4 + f.len()]);
            if f.len() % 2 == 1 {
                stream.push(0);
            }
            assert!(n == 4 + f.len() || n == 8 + f.len());
        }
        for cut in 0..=stream.len() {
            let mut r = Ax772Receiver::default();
            let mut got = Vec::new();
            r.feed(&stream[..cut], &mut |f| got.push(f.to_vec()));
            r.feed(&stream[cut..], &mut |f| got.push(f.to_vec()));
            assert_eq!(got, frames, "split at {cut}");
        }
        // Padding when the packet fills whole USB packets: 508 + 4 = 512.
        let mut b = vec![0u8; 2048];
        assert_eq!(ax772_tx(&mut b, &[1; 508], 512), 516);
        assert_eq!(&b[512..516], &[0, 0, 0xFF, 0xFF]);
    }

    #[test]
    fn ax772_desync_recovers() {
        let mut r = Ax772Receiver::default();
        let mut n = 0;
        r.feed(&[1, 2, 3, 4, 5, 6], &mut |_| n += 1);
        assert_eq!(r.errors, 1);
        let mut b = vec![0u8; 100];
        ax772_tx(&mut b, &[9; 60], 512);
        r.feed(&b[..64], &mut |_| n += 1);
        assert_eq!(n, 1, "a good frame after a bad transfer");
    }

    fn ax179_block(frames: &[Vec<u8>], flags: &[u32]) -> Vec<u8> {
        let mut b = Vec::new();
        let mut headers = Vec::new();
        for (k, f) in frames.iter().enumerate() {
            let len = f.len() + 2;
            headers.push((len as u32) << 16 | flags.get(k).copied().unwrap_or(0));
            b.extend_from_slice(&[0, 0]);
            b.extend_from_slice(f);
            b.resize((b.len() + 7) & !7, 0);
        }
        let table = b.len();
        for h in headers {
            b.extend_from_slice(&h.to_le_bytes());
        }
        b.extend_from_slice(&((table as u32) << 16 | frames.len() as u32).to_le_bytes());
        b
    }

    #[test]
    fn ax179_aggregate() {
        let frames: Vec<Vec<u8>> = (0..5).map(|k| vec![k as u8 + 1; 60 + 333 * k]).collect();
        let block = ax179_block(&frames, &[0, RXHDR_CRC_ERR]);
        let mut got = Vec::new();
        assert_eq!(ax179_rx(&block, &mut |f| got.push(f.to_vec())), Ok(4));
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], frames[0]);
        assert_eq!(got[1..], frames[2..]);
        // A count that runs into the trailer, a table past the end.
        let mut bad = block.clone();
        let n = bad.len();
        bad[n - 4] = 200;
        assert_eq!(ax179_rx(&bad, &mut |_| {}), Err(Error::Truncated));
        let mut bad = block.clone();
        bad[n - 1] = 0xFF;
        assert_eq!(ax179_rx(&bad, &mut |_| {}), Err(Error::Truncated));
    }

    #[test]
    fn ax179_tx_padding_flag() {
        let mut b = vec![0u8; 2048];
        assert_eq!(ax179_tx(&mut b, &[0; 504], 512), 512);
        assert_eq!(le32(&b, 4), Some(0x8000_8000));
        ax179_tx(&mut b, &[0; 60], 512);
        assert_eq!(le32(&b, 4), Some(0));
    }

    #[test]
    fn negotiation() {
        assert_eq!(mii::negotiated(mii::ADVERTISE_ALL, 0x45E1), (100, true));
        assert_eq!(mii::negotiated(mii::ADVERTISE_ALL, 0x0061), (10, true));
        assert_eq!(mii::negotiated(mii::ADVERTISE_ALL, 0x0081), (100, false));
    }
}
