//! The HID over I²C protocol against the software device, and on through
//! aurora-hid's parser and gesture engine, as the kernel driver does it.

use aurora_hid::gestures::{Gestures, Settings};
use aurora_hid::{touchpad, Descriptor};
use aurora_i2c::hid::{command, opcode, Device, HidDescriptor, Input, ReportType, DESCRIPTOR_LEN};
use aurora_i2c::mock::{precision_touchpad, touch_report, HidTarget};
use aurora_i2c::Error;

fn pad() -> HidTarget {
    HidTarget::new(0x2C, 0x20, precision_touchpad(), 32)
}

#[test]
fn command_encoding() {
    // SET_POWER(SLEEP) to command register 0x0005: 05 00 01 08.
    assert_eq!(command(5, opcode::SET_POWER, None, 0, 1), vec![0x05, 0x00, 0x01, 0x08]);
    assert_eq!(command(5, opcode::RESET, None, 0, 0), vec![0x05, 0x00, 0x00, 0x01]);
    // GET_REPORT(Feature, id 3): type 3 in bits 5:4.
    assert_eq!(command(5, opcode::GET_REPORT, Some(ReportType::Feature), 3, 0), vec![0x05, 0x00, 0x33, 0x02]);
    // Report ids from 15 up go in an extra byte.
    assert_eq!(command(5, opcode::SET_REPORT, Some(ReportType::Feature), 0x42, 0), vec![0x05, 0x00, 0x3F, 0x03, 0x42]);
}

#[test]
fn descriptor_round_trip_and_rejects() {
    let t = pad();
    let bytes = t.desc.to_bytes();
    assert_eq!(HidDescriptor::parse(&bytes).unwrap(), t.desc);
    let mut bad = bytes;
    bad[0] = 29;
    assert!(HidDescriptor::parse(&bad).is_err(), "wrong length field");
    let mut bad = bytes;
    bad[3] = 2;
    assert!(HidDescriptor::parse(&bad).is_err(), "version 2.x");
    assert!(HidDescriptor::parse(&bytes[..20]).is_err());
    assert!(HidDescriptor::parse(&[0xFF; DESCRIPTOR_LEN]).is_err(), "a bus that idles high");
}

#[test]
fn bring_up_a_precision_touchpad() {
    let mut t = pad();
    // Wrong address: nobody answers.
    assert_eq!(Device::probe(&mut t, 0x15, 0x20).err(), Some(aurora_i2c::hid::Error::Bus(Error::AddressNack)));
    // The device starts asleep and misses the first transfer; probe retries.
    let dev = Device::probe(&mut t, 0x2C, 0x20).unwrap();
    assert_eq!((dev.desc.vendor, dev.desc.product), (0x1234, 0x5678));
    dev.set_power(&mut t, true).unwrap();
    dev.reset(&mut t).unwrap();
    let mut buf = dev.input_buffer();
    assert_eq!(dev.read_input(&mut t, &mut buf).unwrap(), Input::Empty, "reset sentinel");
    let rd = dev.report_descriptor(&mut t).unwrap();
    assert_eq!(rd, precision_touchpad());
    let d = Descriptor::parse(&rd).unwrap();
    assert!(touchpad::is_touchpad(&d));

    // Into touchpad mode, as the kernel does it.
    let mode = touchpad::input_mode_report(&d).unwrap();
    dev.set_report(&mut t, ReportType::Feature, mode[0], &mode).unwrap();
    let sel = touchpad::selective_reporting_report(&d).unwrap();
    dev.set_report(&mut t, ReportType::Feature, sel[0], &sel).unwrap();
    assert_eq!(dev.get_report(&mut t, ReportType::Feature, 3, 8).unwrap(), vec![3, 3], "input mode 3");
    assert_eq!(t.feature(4), Some(&[4u8, 3][..]), "surface and button switches on");

    // Reports flow: one finger moving right, then lifting.
    let info = touchpad::info(&d).unwrap();
    let mut g = Gestures::new(&info);
    let mut frames = touchpad::Frames::new();
    let s = Settings::default();
    let mut dx = 0;
    let mut now = 0;
    for (i, x) in [4000u16, 4100, 4300, 4600].iter().enumerate() {
        t.push_input(&touch_report(&[(1, true, *x, 3000)], 1, false));
        let Input::Report(r) = dev.read_input(&mut t, &mut buf).unwrap() else { panic!("report {i}") };
        let frame = frames.feed(touchpad::read(&d, r).unwrap()).unwrap();
        now += 8;
        dx += g.frame(now, &frame, &s).dx;
    }
    assert!(dx > 20, "moved right: {dx}");
    assert_eq!(dev.read_input(&mut t, &mut buf).unwrap(), Input::Empty, "nothing more");
}

#[test]
fn sleep_and_bad_lengths() {
    let mut t = pad();
    let dev = Device::probe(&mut t, 0x2C, 0x20).unwrap();
    dev.set_power(&mut t, false).unwrap();
    assert!(t.asleep);
    // Waking: the first SET_POWER(ON) is missed, the retry lands.
    dev.set_power(&mut t, true).unwrap();
    assert!(!t.asleep);
    // A report longer than the device's maximum is refused, not trusted.
    let short = Device { addr: 0x2C, desc: HidDescriptor { max_input_len: 4, ..dev.desc } };
    t.push_input(&touch_report(&[], 0, false));
    let mut buf = short.input_buffer();
    assert!(matches!(short.read_input(&mut t, &mut buf), Err(aurora_i2c::hid::Error::BadLength(17))));
}
