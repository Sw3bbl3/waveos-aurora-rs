//! The test run's I²C touchpad, described in ACPI the way laptop firmware
//! describes one, as an SSDT this module assembles (no `iasl` needed).
//!
//! QEMU has no I²C controller, so the kernel's self-test build binds the
//! controller's test id (`WVOS0001`) to a software bus with a Precision
//! Touchpad on it. Everything above the bus is the real code: the AML
//! interpreter walks this table, `_STA` hides the absent alternative,
//! `_CRS` is built with `ConcatenateResTemplate` from an `I2cSerialBusV2`
//! and a `GpioInt` template, `_DSM` checks the HID-over-I²C UUID and
//! returns the HID descriptor register, `_PS0` records that it ran.
//!
//! ```text
//! Scope (\_SB) {
//!   Device (I2CT) {                       // the controller
//!     Name (_HID, "WVOS0001")  Name (_UID, 0)
//!     Name (SSCN, Package () { 0x210, 0x258, 0x1C })
//!     Name (FMCN, Package () { 0x6C, 0xC0, 0x1C })
//!     Device (TPD0) {                     // the touchpad
//!       Name (_HID, "WVOS0002")  Name (_CID, "PNP0C50")  Name (_UID, 1)
//!       Name (PSTA, 0)
//!       Method (_STA) { Return (0x0F) }
//!       Method (_CRS, 0, Serialized) {
//!         Name (SBFB, ResourceTemplate () { I2cSerialBusV2 (0x2C, …, 400000, …, "\\_SB.I2CT") })
//!         Name (SBFI, ResourceTemplate () { GpioInt (Level, ActiveLow, ExclusiveAndWake, PullUp, 0, "\\_SB.GPI0") { 0x17 } })
//!         Return (ConcatenateResTemplate (SBFB, SBFI))
//!       }
//!       Method (_DSM, 4, Serialized) {
//!         If (Arg0 == ToUUID ("3cdff6f7-4267-4555-ad05-b30a3d8938de")) {
//!           If (Arg2 == Zero) { If (Arg1 == One) { Return (Buffer () { 0x03 }) } }
//!           If (Arg2 == One) { Return (0x20) }
//!         }
//!         Return (Buffer () { 0 })
//!       }
//!       Method (_PS0) { PSTA = One }
//!       Method (_PS3) { PSTA = 3 }
//!     }
//!     Device (TPD1) {                     // a model this machine doesn't have
//!       Name (_HID, "WVOS0003")  Name (_CID, "PNP0C50")
//!       Method (_STA) { Return (Zero) }
//!     }
//!   }
//! }
//! ```

use aurora_acpi_res::{encode, Polarity, Trigger};

pub const TOUCHPAD_ADDRESS: u16 = 0x2C;
pub const HID_DESCRIPTOR_REGISTER: u16 = 0x20;

/// Minimal AML encoding (ACPI 6.5, section 20).
mod aml {
    pub const ZERO: u8 = 0x00;
    pub const ONE: u8 = 0x01;
    const NAME_OP: u8 = 0x08;
    const BYTE_PREFIX: u8 = 0x0A;
    const WORD_PREFIX: u8 = 0x0B;
    const DWORD_PREFIX: u8 = 0x0C;
    const STRING_PREFIX: u8 = 0x0D;
    const SCOPE_OP: u8 = 0x10;
    const BUFFER_OP: u8 = 0x11;
    const PACKAGE_OP: u8 = 0x12;
    const METHOD_OP: u8 = 0x14;
    const EXT_OP: u8 = 0x5B;
    const DEVICE_OP: u8 = 0x82;
    pub const ARG0: u8 = 0x68;
    pub const ARG1: u8 = 0x69;
    pub const ARG2: u8 = 0x6A;
    const STORE_OP: u8 = 0x70;
    const CONCAT_RES_OP: u8 = 0x84;
    const LEQUAL_OP: u8 = 0x93;
    const IF_OP: u8 = 0xA0;
    const RETURN_OP: u8 = 0xA4;

    /// PkgLength: the length of what follows plus the encoding itself.
    pub fn pkg(body: &[u8]) -> Vec<u8> {
        let len = body.len();
        let mut out = if len + 1 < 0x40 {
            vec![(len + 1) as u8]
        } else if len + 2 < 0x1000 {
            let n = len + 2;
            vec![0x40 | (n & 0xF) as u8, (n >> 4) as u8]
        } else {
            let n = len + 3;
            vec![0x80 | (n & 0xF) as u8, (n >> 4) as u8, (n >> 12) as u8]
        };
        out.extend_from_slice(body);
        out
    }

    /// A NameString: a root path ("\\_SB.I2CT") or one segment ("TPD0").
    pub fn name(path: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let rest = match path.strip_prefix('\\') {
            Some(r) => {
                out.push(b'\\');
                r
            }
            None => path,
        };
        let segs: Vec<&str> = rest.split('.').collect();
        match segs.len() {
            1 => {}
            2 => out.push(0x2E),
            n => {
                out.push(0x2F);
                out.push(n as u8);
            }
        }
        for s in segs {
            let mut seg = [b'_'; 4];
            seg[..s.len()].copy_from_slice(s.as_bytes());
            out.extend_from_slice(&seg);
        }
        out
    }

    pub fn int(v: u64) -> Vec<u8> {
        match v {
            0 => vec![ZERO],
            1 => vec![ONE],
            2..=0xFF => vec![BYTE_PREFIX, v as u8],
            0x100..=0xFFFF => {
                let mut o = vec![WORD_PREFIX];
                o.extend_from_slice(&(v as u16).to_le_bytes());
                o
            }
            _ => {
                let mut o = vec![DWORD_PREFIX];
                o.extend_from_slice(&(v as u32).to_le_bytes());
                o
            }
        }
    }

    pub fn string(s: &str) -> Vec<u8> {
        let mut o = vec![STRING_PREFIX];
        o.extend_from_slice(s.as_bytes());
        o.push(0);
        o
    }

    pub fn buffer(bytes: &[u8]) -> Vec<u8> {
        let mut body = int(bytes.len() as u64);
        body.extend_from_slice(bytes);
        let mut o = vec![BUFFER_OP];
        o.extend(pkg(&body));
        o
    }

    pub fn package(items: &[Vec<u8>]) -> Vec<u8> {
        let mut body = vec![items.len() as u8];
        for i in items {
            body.extend_from_slice(i);
        }
        let mut o = vec![PACKAGE_OP];
        o.extend(pkg(&body));
        o
    }

    pub fn name_obj(n: &str, value: Vec<u8>) -> Vec<u8> {
        let mut o = vec![NAME_OP];
        o.extend(name(n));
        o.extend(value);
        o
    }

    pub fn scope(path: &str, body: &[Vec<u8>]) -> Vec<u8> {
        let mut inner = name(path);
        inner.extend(body.concat());
        let mut o = vec![SCOPE_OP];
        o.extend(pkg(&inner));
        o
    }

    pub fn device(n: &str, body: &[Vec<u8>]) -> Vec<u8> {
        let mut inner = name(n);
        inner.extend(body.concat());
        let mut o = vec![EXT_OP, DEVICE_OP];
        o.extend(pkg(&inner));
        o
    }

    pub fn method(n: &str, args: u8, serialized: bool, body: &[Vec<u8>]) -> Vec<u8> {
        let mut inner = name(n);
        inner.push(args | (serialized as u8) << 3);
        inner.extend(body.concat());
        let mut o = vec![METHOD_OP];
        o.extend(pkg(&inner));
        o
    }

    pub fn ret(value: Vec<u8>) -> Vec<u8> {
        let mut o = vec![RETURN_OP];
        o.extend(value);
        o
    }

    pub fn if_(predicate: Vec<u8>, body: &[Vec<u8>]) -> Vec<u8> {
        let mut inner = predicate;
        inner.extend(body.concat());
        let mut o = vec![IF_OP];
        o.extend(pkg(&inner));
        o
    }

    pub fn lequal(a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
        let mut o = vec![LEQUAL_OP];
        o.extend(a);
        o.extend(b);
        o
    }

    pub fn store(value: Vec<u8>, target: &str) -> Vec<u8> {
        let mut o = vec![STORE_OP];
        o.extend(value);
        o.extend(name(target));
        o
    }

    /// `ConcatenateResTemplate (a, b)` with no target.
    pub fn concat_res(a: &str, b: &str) -> Vec<u8> {
        let mut o = vec![CONCAT_RES_OP];
        o.extend(name(a));
        o.extend(name(b));
        o.push(ZERO); // NullName: no target
        o
    }

    /// An SSDT around `body`, with its checksum.
    pub fn ssdt(oem_table: &[u8; 8], body: &[u8]) -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(b"SSDT");
        t.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
        t.push(2); // revision: 64-bit integers
        t.push(0); // checksum, below
        t.extend_from_slice(b"WAVEOS");
        t.extend_from_slice(oem_table);
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(b"WVOS");
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(body);
        let sum = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
        t
    }
}

/// The touchpad's `_CRS` templates: the I²C connection and the interrupt.
pub fn touchpad_resources() -> (Vec<u8>, Vec<u8>) {
    let i2c = encode::template(&[encode::i2c_serial_bus(TOUCHPAD_ADDRESS, 400_000, "\\_SB.I2CT")]);
    let gpio = encode::template(&[encode::gpio_int(Trigger::Level, Polarity::Low, true, &[0x17], "\\_SB.GPI0")]);
    (i2c, gpio)
}

pub fn build() -> Vec<u8> {
    use aml::*;
    let (i2c, gpio) = touchpad_resources();
    let uuid = aurora_acpi_res::uuid(aurora_acpi_res::HID_I2C_DSM).expect("UUID");
    let dsm = method(
        "_DSM",
        4,
        true,
        &[
            if_(
                lequal(vec![ARG0], buffer(&uuid)),
                &[
                    if_(lequal(vec![ARG2], int(0)), &[if_(lequal(vec![ARG1], int(1)), &[ret(buffer(&[0x03]))])]),
                    if_(lequal(vec![ARG2], int(1)), &[ret(int(HID_DESCRIPTOR_REGISTER as u64))]),
                ],
            ),
            ret(buffer(&[0])),
        ],
    );
    let touchpad = device(
        "TPD0",
        &[
            name_obj("_HID", string("WVOS0002")),
            name_obj("_CID", string("PNP0C50")),
            name_obj("_UID", int(1)),
            name_obj("PSTA", int(0)),
            method("_STA", 0, false, &[ret(int(0x0F))]),
            method(
                "_CRS",
                0,
                true,
                &[name_obj("SBFB", buffer(&i2c)), name_obj("SBFI", buffer(&gpio)), ret(concat_res("SBFB", "SBFI"))],
            ),
            dsm,
            method("_PS0", 0, false, &[store(int(1), "PSTA")]),
            method("_PS3", 0, false, &[store(int(3), "PSTA")]),
        ],
    );
    let absent = device(
        "TPD1",
        &[
            name_obj("_HID", string("WVOS0003")),
            name_obj("_CID", string("PNP0C50")),
            method("_STA", 0, false, &[ret(int(0))]),
        ],
    );
    let controller = device(
        "I2CT",
        &[
            name_obj("_HID", string("WVOS0001")),
            name_obj("_UID", int(0)),
            name_obj("SSCN", package(&[int(0x210), int(0x258), int(0x1C)])),
            name_obj("FMCN", package(&[int(0x6C), int(0xC0), int(0x1C)])),
            touchpad,
            absent,
        ],
    );
    ssdt(b"AURORI2C", &scope("\\_SB", &[controller]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_well_formed() {
        let t = build();
        assert_eq!(&t[..4], b"SSDT");
        assert_eq!(u32::from_le_bytes(t[4..8].try_into().unwrap()) as usize, t.len());
        assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0, "checksum");
        // The resource templates are in it, and decode to what the kernel expects.
        let (i2c, gpio) = touchpad_resources();
        assert!(t.windows(i2c.len()).any(|w| w == i2c.as_slice()));
        assert!(t.windows(gpio.len()).any(|w| w == gpio.as_slice()));
        let r = aurora_acpi_res::decode(&i2c).unwrap();
        assert_eq!(r[0].i2c(), Some(("\\_SB.I2CT", TOUCHPAD_ADDRESS, 400_000)));
    }

    #[test]
    fn package_lengths() {
        assert_eq!(aml::pkg(&[0; 10])[0], 11);
        let p = aml::pkg(&[0; 100]);
        // Two bytes: 0x40 | low nibble, then the rest.
        assert_eq!(((p[0] & 0xF) as usize) | (p[1] as usize) << 4, 102);
        let p = aml::pkg(&[0; 5000]);
        assert_eq!(((p[0] & 0xF) as usize) | (p[1] as usize) << 4 | (p[2] as usize) << 12, 5003);
        assert_eq!(aml::name("\\_SB.I2CT"), b"\\\x2E_SB_I2CT".to_vec());
    }
}
