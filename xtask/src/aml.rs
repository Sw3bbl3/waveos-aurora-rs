//! A tiny AML assembler for a test SSDT: a laptop battery (73 %, discharging),
//! a power adapter (unplugged) and a lid (open), so the ACPI runtime can be
//! exercised in QEMU (`-acpitable file=…`), which has none of these.

/// PkgLength: the length of what follows, including the PkgLength itself.
fn pkg(body: Vec<u8>) -> Vec<u8> {
    for extra in 0..4usize {
        let total = body.len() + 1 + extra;
        if (extra == 0 && total < 64) || (extra > 0 && total < 1 << (4 + 8 * extra)) {
            let mut out = Vec::with_capacity(total);
            if extra == 0 {
                out.push(total as u8);
            } else {
                out.push((extra as u8) << 6 | (total & 0xF) as u8);
                for i in 0..extra {
                    out.push((total >> (4 + 8 * i)) as u8);
                }
            }
            out.extend(body);
            return out;
        }
    }
    panic!("AML package too large");
}

fn int(v: u64) -> Vec<u8> {
    match v {
        0 => vec![0x00],
        1 => vec![0x01],
        2..=0xFF => vec![0x0A, v as u8],
        0x100..=0xFFFF => [&[0x0B][..], &(v as u16).to_le_bytes()].concat(),
        0x1_0000..=0xFFFF_FFFF => [&[0x0C][..], &(v as u32).to_le_bytes()].concat(),
        _ => [&[0x0E][..], &v.to_le_bytes()].concat(),
    }
}

fn string(s: &str) -> Vec<u8> {
    [&[0x0D][..], s.as_bytes(), &[0]].concat()
}

/// `\_SB_.XXXX` as an absolute two-segment name.
fn path(seg: &str) -> Vec<u8> {
    [&b"\\\x2E_SB_"[..], seg.as_bytes()].concat()
}

/// The compressed EISA id of e.g. "PNP0C0A", as `EisaId()` encodes it.
fn eisa(id: &str) -> u64 {
    let b = id.as_bytes();
    let c = |i: usize| (b[i] - 0x40) as u32;
    let v = c(0) << 26 | c(1) << 21 | c(2) << 16 | u32::from_str_radix(&id[3..], 16).unwrap();
    v.swap_bytes() as u64
}

fn name(seg: &str, value: Vec<u8>) -> Vec<u8> {
    [&[0x08][..], seg.as_bytes(), &value].concat()
}

fn method(seg: &str, returns: Vec<u8>) -> Vec<u8> {
    let body = [seg.as_bytes(), &[0x00], &[0xA4], &returns].concat(); // no arguments; Return(…)
    [vec![0x14], pkg(body)].concat()
}

fn package(items: Vec<Vec<u8>>) -> Vec<u8> {
    let body = [vec![items.len() as u8], items.concat()].concat();
    [vec![0x12], pkg(body)].concat()
}

fn device(seg: &str, contents: Vec<Vec<u8>>) -> Vec<u8> {
    let body = [path(seg), contents.concat()].concat();
    [vec![0x5B, 0x82], pkg(body)].concat()
}

pub fn battery_ssdt() -> Vec<u8> {
    let battery = device(
        "BAT0",
        vec![
            name("_HID", int(eisa("PNP0C0A"))),
            name("_UID", int(1)),
            method("_STA", int(0x1F)),
            // _BIF: mWh, design 50 000, last full 48 000, rechargeable, 11.4 V, …
            method(
                "_BIF",
                package(vec![
                    int(0),
                    int(50_000),
                    int(48_000),
                    int(1),
                    int(11_400),
                    int(2_500),
                    int(1_000),
                    int(100),
                    int(100),
                    string("Aurora Test Cell"),
                    string("0001"),
                    string("LION"),
                    string("WaveOS"),
                ]),
            ),
            // _BST: discharging at 9.5 W, 35 040 mWh left (73 % of 48 000), 11.1 V.
            method("_BST", package(vec![int(1), int(9_500), int(35_040), int(11_100)])),
        ],
    );
    let adapter =
        device("ADP0", vec![name("_HID", string("ACPI0003")), method("_STA", int(0x0F)), method("_PSR", int(0))]);
    let lid = device("LID0", vec![name("_HID", int(eisa("PNP0C0D"))), method("_LID", int(1))]);
    let aml = [battery, adapter, lid].concat();

    let mut t = Vec::with_capacity(36 + aml.len());
    t.extend_from_slice(b"SSDT");
    t.extend_from_slice(&((36 + aml.len()) as u32).to_le_bytes());
    t.push(2); // revision
    t.push(0); // checksum, below
    t.extend_from_slice(b"WAVEOS");
    t.extend_from_slice(b"AURORBAT");
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(b"AURA");
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(&aml);
    let sum = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eisa_ids_match_iasl() {
        assert_eq!(eisa("PNP0C0A"), 0x0A0C_D041);
        assert_eq!(eisa("PNP0C0D"), 0x0D0C_D041);
    }

    #[test]
    fn pkg_lengths() {
        assert_eq!(pkg(vec![0; 10])[0], 11);
        let long = pkg(vec![0; 100]);
        assert_eq!(long[0] >> 6, 1);
        assert_eq!((long[0] & 0xF) as usize | (long[1] as usize) << 4, 102);
    }

    #[test]
    fn table_checksums_to_zero() {
        let t = battery_ssdt();
        assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        assert_eq!(u32::from_le_bytes(t[4..8].try_into().unwrap()) as usize, t.len());
    }
}
