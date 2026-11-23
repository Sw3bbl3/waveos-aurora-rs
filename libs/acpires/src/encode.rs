//! Building resource descriptors, byte for byte as ASL's resource macros
//! compile them: for tests, and for the test SSDT xtask generates.

use crate::resource::{Polarity, Trigger};
use alloc::vec::Vec;

fn large(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(3 + body.len());
    d.push(tag);
    d.extend_from_slice(&(body.len() as u16).to_le_bytes());
    d.extend_from_slice(body);
    d
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.bytes().collect();
    v.push(0);
    v
}

/// `I2cSerialBusV2 (address, ControllerInitiated, speed, AddressingMode7Bit,
/// source, 0, ResourceConsumer, , Exclusive)`.
pub fn i2c_serial_bus(address: u16, speed_hz: u32, source: &str) -> Vec<u8> {
    // Revision 2 (V2), resource source index 0, I2C, controller initiated + consumer + exclusive.
    let mut b = vec_of(&[2, 0, 1, 0x02]);
    b.extend_from_slice(&0u16.to_le_bytes()); // 7-bit addressing
    b.push(1); // type-specific revision
    b.extend_from_slice(&6u16.to_le_bytes()); // type data length
    b.extend_from_slice(&speed_hz.to_le_bytes());
    b.extend_from_slice(&address.to_le_bytes());
    b.extend_from_slice(&cstr(source));
    large(0x8E, &b)
}

/// `GpioInt (trigger, polarity, Exclusive[AndWake], PullUp, 0, source) { pins }`.
pub fn gpio_int(trigger: Trigger, polarity: Polarity, wake: bool, pins: &[u16], source: &str) -> Vec<u8> {
    let pin_at = 23u16;
    let source_at = pin_at + 2 * pins.len() as u16;
    let vendor_at = source_at + source.len() as u16 + 1;
    let mut flags = match trigger {
        Trigger::Level => 0u16,
        Trigger::Edge => 1,
    };
    flags |= match polarity {
        Polarity::High => 0,
        Polarity::Low => 1,
        Polarity::Both => 2,
    } << 1;
    if wake {
        flags |= 0x10;
    }
    // Revision 1, an interrupt connection.
    let mut b = vec_of(&[1, 0]);
    b.extend_from_slice(&1u16.to_le_bytes()); // consumer
    b.extend_from_slice(&flags.to_le_bytes());
    b.push(1); // pull-up
    b.extend_from_slice(&0u16.to_le_bytes()); // drive strength
    b.extend_from_slice(&0u16.to_le_bytes()); // debounce
    b.extend_from_slice(&pin_at.to_le_bytes());
    b.push(0); // resource source index
    b.extend_from_slice(&source_at.to_le_bytes());
    b.extend_from_slice(&vendor_at.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // no vendor data
    for p in pins {
        b.extend_from_slice(&p.to_le_bytes());
    }
    b.extend_from_slice(&cstr(source));
    large(0x8C, &b)
}

/// `Interrupt (ResourceConsumer, trigger, polarity, Exclusive) { gsis }`.
pub fn interrupt(trigger: Trigger, polarity: Polarity, gsis: &[u32]) -> Vec<u8> {
    let mut flags = 0x01u8; // consumer
    if trigger == Trigger::Edge {
        flags |= 0x02;
    }
    if polarity == Polarity::Low {
        flags |= 0x04;
    }
    let mut b = vec_of(&[flags, gsis.len() as u8]);
    for g in gsis {
        b.extend_from_slice(&g.to_le_bytes());
    }
    large(0x89, &b)
}

/// `Memory32Fixed (ReadWrite, base, len)`.
pub fn memory32_fixed(base: u32, len: u32) -> Vec<u8> {
    let mut b = vec_of(&[1]);
    b.extend_from_slice(&base.to_le_bytes());
    b.extend_from_slice(&len.to_le_bytes());
    large(0x86, &b)
}

/// The End Tag, with a zero ("not computed") checksum.
pub fn end_tag() -> Vec<u8> {
    vec_of(&[0x79, 0x00])
}

/// Descriptors joined into a template with an End Tag.
pub fn template(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out: Vec<u8> = parts.concat();
    out.extend_from_slice(&end_tag());
    out
}

fn vec_of(b: &[u8]) -> Vec<u8> {
    b.to_vec()
}
