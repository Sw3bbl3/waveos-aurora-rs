//! Decoding resource descriptors (ACPI 6.5, section 6.4).
//!
//! Small items start with one byte, `0 tttt lll`: type and length. Large
//! items start with `1 ttttttt` and a 16-bit length. The list ends with the
//! End Tag (`0x79` and a checksum). Buffers come from firmware, so nothing
//! is trusted: a descriptor that runs past the buffer is an error, unknown
//! ones are kept as [`Resource::Other`], and nothing panics.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A descriptor runs past the end of the buffer, or is shorter than its type needs.
    Truncated { offset: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Level,
    Edge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    High,
    Low,
    /// Both edges (GPIO interrupts only).
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    Default,
    Up,
    Down,
    None,
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressSpace {
    Memory,
    Io,
    Bus,
    Other(u8),
}

/// A GPIO connection (`GpioInt` or `GpioIo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpio {
    /// `GpioInt` (an interrupt) rather than `GpioIo`.
    pub interrupt: bool,
    pub trigger: Trigger,
    pub polarity: Polarity,
    pub shared: bool,
    pub wake: bool,
    pub pull: Pull,
    /// Debounce timeout, in hundredths of a millisecond.
    pub debounce: u16,
    /// Output drive strength, in hundredths of a milliampere (`GpioIo`).
    pub drive_strength: u16,
    /// `GpioIo` only: 0 none, 1 input only, 2 output only, 3 preserve.
    pub io_restriction: u8,
    /// Pin numbers on the controller named by `source`.
    pub pins: Vec<u16>,
    /// The GPIO controller's path, e.g. "\\_SB.GPI0".
    pub source: String,
}

/// A serial bus connection (`I2cSerialBusV2`, `SpiSerialBusV2`, `UartSerialBusV2`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialBus {
    /// The controller's path, e.g. "\\_SB.PCI0.I2C1".
    pub source: String,
    /// The device starts transfers itself (`DeviceInitiated`).
    pub device_initiated: bool,
    /// The descriptor revision (2 for the V2 macros).
    pub revision: u8,
    pub kind: BusKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusKind {
    I2c {
        address: u16,
        ten_bit: bool,
        speed_hz: u32,
    },
    Spi {
        speed_hz: u32,
        data_bits: u8,
        /// Clock phase (0: first edge) and polarity (0: idle low).
        phase: u8,
        polarity: u8,
        chip_select: u16,
        chip_select_active_high: bool,
        three_wire: bool,
    },
    Uart {
        baud: u32,
        data_bits: u8,
        /// 0 none, 1 one, 2 one and a half, 3 two.
        stop_bits: u8,
        /// 0 none, 1 hardware (RTS/CTS), 2 XON/XOFF.
        flow_control: u8,
        /// 0 none, 1 even, 2 odd, 3 mark, 4 space.
        parity: u8,
        rx_fifo: u16,
        tx_fifo: u16,
        /// Lines in use: bit 7 RTS, 6 CTS, 5 DTR, 4 DSR, 3 RI, 2 DTD.
        lines: u8,
        big_endian: bool,
    },
    Other(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Legacy ISA IRQs (`IRQ`, `IRQNoFlags`).
    Irq {
        irqs: Vec<u8>,
        trigger: Trigger,
        polarity: Polarity,
        shared: bool,
        wake: bool,
    },
    /// ISA DMA channels as a bit mask.
    Dma {
        channels: u8,
    },
    Io {
        min: u16,
        max: u16,
        align: u8,
        len: u8,
    },
    FixedIo {
        base: u16,
        len: u16,
    },
    /// `Memory24`, `Memory32` (the minimum base) and `Memory32Fixed`.
    Memory {
        base: u64,
        len: u64,
        writable: bool,
    },
    /// Word, DWord, QWord and Extended address spaces.
    Address {
        space: AddressSpace,
        min: u64,
        max: u64,
        len: u64,
        translation: u64,
        consumer: bool,
    },
    /// `Interrupt` (the extended interrupt descriptor): global system
    /// interrupts, unless `source` names another interrupt controller.
    Interrupt {
        interrupts: Vec<u32>,
        trigger: Trigger,
        polarity: Polarity,
        shared: bool,
        wake: bool,
        consumer: bool,
        source: Option<String>,
    },
    Gpio(Gpio),
    SerialBus(SerialBus),
    /// Anything else, by its type byte (small items as `0x00..0x0F`, large as `0x80..`).
    Other {
        tag: u8,
        len: usize,
    },
}

impl Resource {
    /// The I²C connection, if this is one: (controller path, address, speed).
    pub fn i2c(&self) -> Option<(&str, u16, u32)> {
        match self {
            Resource::SerialBus(SerialBus { source, kind: BusKind::I2c { address, speed_hz, .. }, .. }) => {
                Some((source.as_str(), *address, *speed_hz))
            }
            _ => None,
        }
    }
}

/// The first memory range in `list` (a fixed or minimum base and length).
pub fn first_memory(list: &[Resource]) -> Option<(u64, u64)> {
    list.iter().find_map(|r| match r {
        Resource::Memory { base, len, .. } if *len > 0 => Some((*base, *len)),
        Resource::Address { space: AddressSpace::Memory, min, len, .. } if *len > 0 => Some((*min, *len)),
        _ => None,
    })
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u32_at(b, i) as u64 | (u32_at(b, i + 4) as u64) << 32
}

/// A NUL-terminated string starting at `at` within `d` (empty if out of range).
fn string_at(d: &[u8], at: usize) -> String {
    let Some(rest) = d.get(at..) else { return String::new() };
    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
    rest[..end].iter().map(|&c| if c.is_ascii() { c as char } else { '?' }).collect()
}

/// Decodes a whole resource template.
pub fn decode(buf: &[u8]) -> Result<Vec<Resource>, Error> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let tag = buf[pos];
        if tag & 0x80 == 0 {
            let kind = (tag >> 3) & 0x0F;
            let len = (tag & 0x07) as usize;
            let body = buf.get(pos + 1..pos + 1 + len).ok_or(Error::Truncated { offset: pos })?;
            if kind == 0x0F {
                // End Tag. Some firmware (and older interpreters' ConcatenateResTemplate)
                // leave one in the middle: keep going if the rest decodes cleanly.
                let rest = &buf[pos + 1 + len..];
                if rest.iter().any(|&b| b != 0) {
                    if let Ok(more) = decode(rest) {
                        out.extend(more);
                    }
                }
                return Ok(out);
            }
            out.push(small(kind, body).ok_or(Error::Truncated { offset: pos })?);
            pos += 1 + len;
        } else {
            let len =
                buf.get(pos + 1..pos + 3).map(|l| u16_at(l, 0) as usize).ok_or(Error::Truncated { offset: pos })?;
            let whole = buf.get(pos..pos + 3 + len).ok_or(Error::Truncated { offset: pos })?;
            out.push(large(tag, whole).ok_or(Error::Truncated { offset: pos })?);
            pos += 3 + len;
        }
    }
    Ok(out)
}

fn small(kind: u8, b: &[u8]) -> Option<Resource> {
    Some(match kind {
        0x04 => {
            let mask = u16_at(b.get(..2)?, 0);
            let info = b.get(2).copied();
            let irqs = (0..16u8).filter(|i| mask & (1 << i) != 0).collect();
            // Without the information byte: edge-triggered, active high.
            let info = info.unwrap_or(1);
            Resource::Irq {
                irqs,
                trigger: if info & 1 != 0 { Trigger::Edge } else { Trigger::Level },
                polarity: if info & 0x08 != 0 { Polarity::Low } else { Polarity::High },
                shared: info & 0x10 != 0,
                wake: info & 0x20 != 0,
            }
        }
        0x05 => Resource::Dma { channels: *b.first()? },
        0x08 => {
            let b = b.get(..7)?;
            Resource::Io { min: u16_at(b, 1), max: u16_at(b, 3), align: b[5], len: b[6] }
        }
        0x09 => {
            let b = b.get(..3)?;
            Resource::FixedIo { base: u16_at(b, 0) & 0x3FF, len: b[2] as u16 }
        }
        _ => Resource::Other { tag: kind, len: b.len() },
    })
}

/// `d` is the whole descriptor, tag and length included (GPIO and serial bus
/// descriptors give offsets from its start).
fn large(tag: u8, d: &[u8]) -> Option<Resource> {
    let body = &d[3..];
    Some(match tag {
        // Memory24: values in 256-byte units.
        0x81 => {
            let b = body.get(..9)?;
            Resource::Memory {
                base: (u16_at(b, 1) as u64) << 8,
                len: (u16_at(b, 7) as u64) << 8,
                writable: b[0] & 1 != 0,
            }
        }
        0x85 => {
            let b = body.get(..17)?;
            Resource::Memory { base: u32_at(b, 1) as u64, len: u32_at(b, 13) as u64, writable: b[0] & 1 != 0 }
        }
        0x86 => {
            let b = body.get(..9)?;
            Resource::Memory { base: u32_at(b, 1) as u64, len: u32_at(b, 5) as u64, writable: b[0] & 1 != 0 }
        }
        0x87 | 0x88 | 0x8A | 0x8B => {
            let (width, start) = match tag {
                0x88 => (2, 3),
                0x87 => (4, 3),
                0x8A => (8, 3),
                _ => (8, 5), // extended: revision and a reserved byte first
            };
            let need = start + 5 * width;
            let b = body.get(..need)?;
            let num = |i: usize| -> u64 {
                let at = start + i * width;
                match width {
                    2 => u16_at(b, at) as u64,
                    4 => u32_at(b, at) as u64,
                    _ => u64_at(b, at),
                }
            };
            let space = match b[0] {
                0 => AddressSpace::Memory,
                1 => AddressSpace::Io,
                2 => AddressSpace::Bus,
                n => AddressSpace::Other(n),
            };
            Resource::Address {
                space,
                min: num(1),
                max: num(2),
                translation: num(3),
                len: num(4),
                consumer: b[1] & 1 != 0,
            }
        }
        0x89 => {
            let flags = *body.first()?;
            let count = *body.get(1)? as usize;
            let list = body.get(2..2 + 4 * count)?;
            let interrupts = (0..count).map(|i| u32_at(list, i * 4)).collect();
            // An optional resource source: an index byte, then a string.
            let source = (body.len() > 2 + 4 * count + 1).then(|| string_at(body, 2 + 4 * count + 1));
            Resource::Interrupt {
                interrupts,
                trigger: if flags & 0x02 != 0 { Trigger::Edge } else { Trigger::Level },
                polarity: if flags & 0x04 != 0 { Polarity::Low } else { Polarity::High },
                shared: flags & 0x08 != 0,
                wake: flags & 0x10 != 0,
                consumer: flags & 0x01 != 0,
                source: source.filter(|s| !s.is_empty()),
            }
        }
        0x8C => gpio(d)?,
        0x8E => serial_bus(d)?,
        _ => Resource::Other { tag, len: body.len() },
    })
}

/// The GPIO connection descriptor: a 23-byte header, then the pin table,
/// the resource source and vendor data at the offsets it gives.
fn gpio(d: &[u8]) -> Option<Resource> {
    let h = d.get(..23)?;
    let interrupt = h[4] == 0;
    let flags = u16_at(h, 7);
    let pin_at = u16_at(h, 14) as usize;
    let source_at = u16_at(h, 17) as usize;
    // The pin table runs up to the resource source name.
    let pins_end = if source_at > pin_at { source_at } else { d.len() };
    let pins =
        d.get(pin_at..pins_end.min(d.len()))?.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect();
    let source = string_at(d, source_at);
    Some(Resource::Gpio(Gpio {
        interrupt,
        trigger: if interrupt && flags & 1 != 0 { Trigger::Edge } else { Trigger::Level },
        polarity: if !interrupt {
            Polarity::High
        } else {
            match (flags >> 1) & 3 {
                0 => Polarity::High,
                1 => Polarity::Low,
                _ => Polarity::Both,
            }
        },
        shared: flags & 0x08 != 0,
        wake: interrupt && flags & 0x10 != 0,
        io_restriction: if interrupt { 0 } else { (flags & 3) as u8 },
        pull: match h[9] {
            0 => Pull::Default,
            1 => Pull::Up,
            2 => Pull::Down,
            3 => Pull::None,
            n => Pull::Other(n),
        },
        drive_strength: u16_at(h, 10),
        debounce: u16_at(h, 12),
        pins,
        source,
    }))
}

/// The generic serial bus connection descriptor: a 12-byte header, the
/// bus type's data, then the resource source.
fn serial_bus(d: &[u8]) -> Option<Resource> {
    let h = d.get(..12)?;
    let revision = h[3];
    let bus_type = h[5];
    let general = h[6];
    let type_flags = u16_at(h, 7);
    let data_len = u16_at(h, 10) as usize;
    let t = d.get(12..12 + data_len)?;
    let kind = match bus_type {
        1 => {
            let t = t.get(..6)?;
            BusKind::I2c { speed_hz: u32_at(t, 0), address: u16_at(t, 4), ten_bit: type_flags & 1 != 0 }
        }
        2 => {
            let t = t.get(..9)?;
            BusKind::Spi {
                speed_hz: u32_at(t, 0),
                data_bits: t[4],
                phase: t[5],
                polarity: t[6],
                chip_select: u16_at(t, 7),
                three_wire: type_flags & 1 != 0,
                chip_select_active_high: type_flags & 2 != 0,
            }
        }
        3 => {
            let t = t.get(..10)?;
            BusKind::Uart {
                baud: u32_at(t, 0),
                rx_fifo: u16_at(t, 4),
                tx_fifo: u16_at(t, 6),
                parity: t[8],
                lines: t[9],
                flow_control: (type_flags & 3) as u8,
                stop_bits: ((type_flags >> 2) & 3) as u8,
                data_bits: 5 + ((type_flags >> 4) & 7) as u8,
                big_endian: type_flags & 0x80 != 0,
            }
        }
        n => BusKind::Other(n),
    };
    Some(Resource::SerialBus(SerialBus {
        source: string_at(d, 12 + data_len),
        device_initiated: general & 1 != 0,
        revision,
        kind,
    }))
}
