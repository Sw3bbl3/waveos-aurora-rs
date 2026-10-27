//! Intel Wi-Fi firmware files (`iwlwifi-*.ucode`): the TLV container the
//! Linux firmware repository ships. Reading it needs no hardware, so the
//! parser lives here and is tested on the host.

use alloc::string::String;
use alloc::vec::Vec;

const MAGIC: u32 = 0x0A4C_5749; // "IWL\n"
const HEADER: usize = 4 + 4 + 64 + 4 + 4 + 8;

// TLV types used to find the code sections.
pub const TLV_SEC_RT: u32 = 19;
pub const TLV_SEC_INIT: u32 = 20;
pub const TLV_SEC_WOWLAN: u32 = 21;
pub const TLV_API_CHANGES: u32 = 29;
pub const TLV_ENABLED_CAPABILITIES: u32 = 30;
pub const TLV_FW_VERSION: u32 = 36;

/// One piece of firmware code: where it loads in device memory, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section<'a> {
    pub offset: u32,
    pub data: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware<'a> {
    /// The human-readable version, e.g. "77.ad46c98b.0 cc-a0-77.ucode".
    pub version: String,
    /// Runtime image sections (what the station runs), in file order.
    pub runtime: Vec<Section<'a>>,
    /// Initialization image sections (older devices run it first).
    pub init: Vec<Section<'a>>,
    pub tlv_count: usize,
}

/// Parses a firmware file; `None` if it is not one.
pub fn parse(file: &[u8]) -> Option<Firmware<'_>> {
    let word = |at: usize| file.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if file.len() < HEADER || word(0)? != 0 || word(4)? != MAGIC {
        return None;
    }
    let name = &file[8..72];
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let mut fw = Firmware {
        version: String::from_utf8_lossy(&name[..end]).into_owned(),
        runtime: Vec::new(),
        init: Vec::new(),
        tlv_count: 0,
    };
    let mut at = HEADER;
    while at + 8 <= file.len() {
        let (kind, len) = (word(at)?, word(at + 4)? as usize);
        let data = file.get(at + 8..at + 8 + len)?;
        match kind {
            TLV_SEC_RT | TLV_SEC_INIT if data.len() >= 4 => {
                let s = Section { offset: u32::from_le_bytes([data[0], data[1], data[2], data[3]]), data: &data[4..] };
                if kind == TLV_SEC_RT {
                    fw.runtime.push(s);
                } else {
                    fw.init.push(s);
                }
            }
            _ => {}
        }
        fw.tlv_count += 1;
        at += 8 + len.next_multiple_of(4);
    }
    Some(fw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tlv(out: &mut Vec<u8>, kind: u32, data: &[u8]) {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }

    #[test]
    fn parses_sections() {
        let mut f = Vec::new();
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&MAGIC.to_le_bytes());
        let mut name = [0u8; 64];
        name[..13].copy_from_slice(b"77.ad46c98b.0");
        f.extend_from_slice(&name);
        f.extend_from_slice(&[0; 16]);
        tlv(&mut f, TLV_API_CHANGES, &[1, 2, 3, 4, 5, 6, 7, 8]);
        tlv(&mut f, TLV_SEC_RT, &[0x00, 0x00, 0x40, 0x00, 0xAA, 0xBB, 0xCC]);
        tlv(&mut f, TLV_SEC_INIT, &[0x00, 0x10, 0x00, 0x00, 1]);
        tlv(&mut f, TLV_SEC_RT, &[0x00, 0x00, 0x80, 0x00, 0xDD]);
        let fw = parse(&f).unwrap();
        assert_eq!(fw.version, "77.ad46c98b.0");
        assert_eq!(fw.tlv_count, 4);
        assert_eq!(fw.runtime, alloc::vec![
            Section { offset: 0x40_0000, data: &[0xAA, 0xBB, 0xCC] },
            Section { offset: 0x80_0000, data: &[0xDD] }
        ]);
        assert_eq!(fw.init.len(), 1);
        // Not firmware, and truncations, never panic.
        assert!(parse(b"hello").is_none());
        for n in 0..f.len() {
            let _ = parse(&f[..n]);
        }
    }
}
