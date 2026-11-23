use aurora_acpi_res::resource::{first_memory, AddressSpace};
use aurora_acpi_res::{decode, eisa_id, encode, normalize_path, uuid, BusKind, Polarity, Pull, Resource, Trigger};

/// `I2cSerialBusV2 (0x15, ControllerInitiated, 400000, AddressingMode7Bit,
/// "\\_SB.PCI0.I2C1", 0x00, ResourceConsumer, , Exclusive)`, laid out by hand
/// from ACPI 6.5 table 6.270 / 6.271.
const I2C_TOUCHPAD: &[u8] = &[
    0x8E, 0x1E, 0x00, // large item 0x0E, length 30
    0x02, 0x00, 0x01, 0x02, // revision 2, source index 0, I2C, controller initiated + consumer
    0x00, 0x00, 0x01, 0x06, 0x00, // 7-bit; type revision 1; type data length 6
    0x80, 0x1A, 0x06, 0x00, // 400 000 Hz
    0x15, 0x00, // address 0x15
    b'\\', b'_', b'S', b'B', b'.', b'P', b'C', b'I', b'0', b'.', b'I', b'2', b'C', b'1', 0x00,
];

/// `GpioInt (Level, ActiveLow, ExclusiveAndWake, PullUp, 0x0000, "\\_SB.GPI0")
/// { 0x0017 }` (ACPI 6.5 table 6.267).
const GPIO_INT: &[u8] = &[
    0x8C, 0x20, 0x00, // large item 0x0C, length 32
    0x01, 0x00, 0x01, 0x00, // revision 1, interrupt connection, consumer
    0x12, 0x00, // level, active low, exclusive, wake capable
    0x01, 0x00, 0x00, 0x00, 0x00, // pull-up, drive strength 0, debounce 0
    0x17, 0x00, 0x00, 0x19, 0x00, // pin table at 23; source index 0; source name at 25
    0x23, 0x00, 0x00, 0x00, // vendor data at 35, length 0
    0x17, 0x00, // pin 23
    b'\\', b'_', b'S', b'B', b'.', b'G', b'P', b'I', b'0', 0x00,
];

const END: &[u8] = &[0x79, 0x00];

#[test]
fn i2c_hid_device_crs() {
    let crs = [I2C_TOUCHPAD, GPIO_INT, END].concat();
    let r = decode(&crs).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].i2c(), Some(("\\_SB.PCI0.I2C1", 0x15, 400_000)));
    let Resource::Gpio(g) = &r[1] else { panic!("{:?}", r[1]) };
    assert!(g.interrupt && g.wake && !g.shared);
    assert_eq!((g.trigger, g.polarity, g.pull), (Trigger::Level, Polarity::Low, Pull::Up));
    assert_eq!(g.pins, vec![0x17]);
    assert_eq!(g.source, "\\_SB.GPI0");
}

#[test]
fn encoder_matches_the_hand_layout() {
    assert_eq!(encode::i2c_serial_bus(0x15, 400_000, "\\_SB.PCI0.I2C1"), I2C_TOUCHPAD);
    assert_eq!(encode::gpio_int(Trigger::Level, Polarity::Low, true, &[0x17], "\\_SB.GPI0"), GPIO_INT);
}

#[test]
fn controller_and_interrupts() {
    // A Haswell-style ACPI-enumerated controller: fixed memory and an IOAPIC interrupt.
    let crs = encode::template(&[
        encode::memory32_fixed(0xFE10_3000, 0x1000),
        encode::interrupt(Trigger::Level, Polarity::Low, &[0x20]),
    ]);
    let r = decode(&crs).unwrap();
    assert_eq!(first_memory(&r), Some((0xFE10_3000, 0x1000)));
    assert_eq!(
        r[1],
        Resource::Interrupt {
            interrupts: vec![0x20],
            trigger: Trigger::Level,
            polarity: Polarity::Low,
            shared: false,
            wake: false,
            consumer: true,
            source: None
        }
    );
}

#[test]
fn small_items_and_address_spaces() {
    // IRQ (edge, active high, mask with IRQ 1), fixed I/O 0x60, I/O 0x64..0x64.
    let mut crs = vec![0x23, 0x02, 0x00, 0x01, 0x4B, 0x60, 0x00, 0x01, 0x47, 0x01, 0x64, 0x00, 0x64, 0x00, 0x01, 0x01];
    // DWord memory: consumer, 0xFED40000..0xFED44FFF.
    crs.extend_from_slice(&[0x87, 0x17, 0x00, 0x00, 0x01, 0x00]);
    for v in [0u32, 0xFED4_0000, 0xFED4_4FFF, 0, 0x5000] {
        crs.extend_from_slice(&v.to_le_bytes());
    }
    crs.extend_from_slice(END);
    let r = decode(&crs).unwrap();
    assert_eq!(
        r[0],
        Resource::Irq { irqs: vec![1], trigger: Trigger::Edge, polarity: Polarity::High, shared: false, wake: false }
    );
    assert_eq!(r[1], Resource::FixedIo { base: 0x60, len: 1 });
    assert_eq!(r[2], Resource::Io { min: 0x64, max: 0x64, align: 1, len: 1 });
    assert_eq!(
        r[3],
        Resource::Address {
            space: AddressSpace::Memory,
            min: 0xFED4_0000,
            max: 0xFED4_4FFF,
            len: 0x5000,
            translation: 0,
            consumer: true
        }
    );
    assert_eq!(first_memory(&r), Some((0xFED4_0000, 0x5000)));
}

#[test]
fn uart_and_spi() {
    // UartSerialBusV2 (115200, DataBitsEight, StopBitsOne, 0xC0, LittleEndian,
    // ParityTypeNone, FlowControlHardware, 32, 32, "\\_SB.PCI0.UA00").
    let mut body = vec![0x02, 0x00, 0x03, 0x02, 0x35, 0x00, 0x01, 0x0A, 0x00];
    body.extend_from_slice(&115_200u32.to_le_bytes());
    body.extend_from_slice(&32u16.to_le_bytes());
    body.extend_from_slice(&32u16.to_le_bytes());
    body.extend_from_slice(&[0x00, 0xC0]);
    body.extend_from_slice(b"\\_SB.PCI0.UA00\0");
    let mut crs = vec![0x8E];
    crs.extend_from_slice(&(body.len() as u16).to_le_bytes());
    crs.extend_from_slice(&body);
    crs.extend_from_slice(END);
    let r = decode(&crs).unwrap();
    let Resource::SerialBus(s) = &r[0] else { panic!() };
    assert_eq!(s.source, "\\_SB.PCI0.UA00");
    assert_eq!(
        s.kind,
        BusKind::Uart {
            baud: 115_200,
            data_bits: 8,
            stop_bits: 1,
            flow_control: 1,
            parity: 0,
            rx_fifo: 32,
            tx_fifo: 32,
            lines: 0xC0,
            big_endian: false
        }
    );
}

#[test]
fn concatenated_templates_with_a_stray_end_tag() {
    // What `ConcatenateResTemplate` made in interpreters that kept both end tags.
    let crs = [I2C_TOUCHPAD, END, GPIO_INT, END].concat();
    let r = decode(&crs).unwrap();
    assert_eq!(r.len(), 2, "both descriptors: {r:?}");
}

#[test]
fn truncated_and_garbage_never_panic() {
    assert!(decode(&I2C_TOUCHPAD[..20]).is_err());
    assert!(decode(&[0x8C, 0x40]).is_err());
    // A GPIO descriptor whose offsets point outside it.
    let mut bad = GPIO_INT.to_vec();
    bad[17] = 0xFF;
    let _ = decode(&bad);
    let mut seed = 0x1234_5678u32;
    for len in 0..300 {
        let buf: Vec<u8> = (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let _ = decode(&buf);
        let mut gpio = GPIO_INT.to_vec();
        gpio.extend_from_slice(&buf);
        let _ = decode(&gpio);
    }
}

#[test]
fn ids_and_uuids() {
    assert_eq!(eisa_id(0x500CD041), "PNP0C50");
    assert_eq!(eisa_id(0x0A0CD041), "PNP0C0A");
    assert_eq!(
        uuid(aurora_acpi_res::HID_I2C_DSM).unwrap(),
        [0xF7, 0xF6, 0xDF, 0x3C, 0x67, 0x42, 0x55, 0x45, 0xAD, 0x05, 0xB3, 0x0A, 0x3D, 0x89, 0x38, 0xDE]
    );
    assert!(uuid("not-a-uuid").is_none());
    assert_eq!(normalize_path("\\_SB.PCI0.I2C1"), "\\_SB_.PCI0.I2C1");
    assert_eq!(normalize_path("\\_SB_.PCI0.I2C1"), "\\_SB_.PCI0.I2C1");
}
