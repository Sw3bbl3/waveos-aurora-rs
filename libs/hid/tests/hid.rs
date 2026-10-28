//! Host tests: real devices' report descriptors and reports.

use aurora_hid::gamepad::{self, button, Layout};
use aurora_hid::gestures::{Gestures, Output, Settings};
use aurora_hid::touchpad::{self, Contact, Frame};
use aurora_hid::{keyboard, pointer, usage, Descriptor, Kind};

/// The boot keyboard descriptor (HID 1.11 Appendix B.1), as most keyboards send.
const BOOT_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
    0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05,
    0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19,
    0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

/// A gaming keyboard: N-key rollover bitmap (report 1), media keys
/// (Consumer Control, report 2) and sleep/power (System Control, report 3).
const NKRO_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, // keyboard, report 1
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, // modifiers
    0x19, 0x00, 0x29, 0x77, 0x95, 0x78, 0x81, 0x02, // 120 keys, one bit each
    0x05, 0x08, 0x19, 0x01, 0x29, 0x03, 0x95, 0x03, 0x91, 0x02, 0x95, 0x05, 0x91, 0x01, // LEDs
    0xC0, //
    0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, // consumer control, report 2
    0x19, 0x00, 0x2A, 0x3C, 0x02, 0x15, 0x00, 0x26, 0x3C, 0x02, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xC0, //
    0x05, 0x01, 0x09, 0x80, 0xA1, 0x01, 0x85, 0x03, // system control, report 3
    0x19, 0x81, 0x29, 0x83, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x03, 0x81, 0x02, 0x95, 0x05, 0x81, 0x01, 0xC0,
];

/// A five-button wheel mouse with horizontal scrolling (AC Pan).
const WHEEL_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25,
    0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
    0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06, 0x05, 0x0C, 0x0A, 0x38, 0x02, 0x95, 0x01,
    0x81, 0x06, 0xC0, 0xC0,
];

/// QEMU's USB tablet: an absolute pointer, 0..=0x7FFF on both axes.
const QEMU_TABLET: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25,
    0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
    0x15, 0x00, 0x26, 0xFF, 0x7F, 0x35, 0x00, 0x46, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09,
    0x38, 0x15, 0x81, 0x25, 0x7F, 0x35, 0x00, 0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0, 0xC0,
];

/// A Precision Touchpad (after Microsoft's sample): two fingers per report,
/// 100 × 60 mm, scan time, contact count, button; contact-count maximum and
/// pad type; Input Mode; selective reporting; and a mouse for legacy mode.
fn precision_touchpad() -> Vec<u8> {
    let finger: &[u8] = &[
        0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, // Finger, logical collection
        0x15, 0x00, 0x25, 0x01, 0x09, 0x47, 0x09, 0x42, 0x95, 0x02, 0x75, 0x01, 0x81, 0x02, // confidence, tip
        0x95, 0x01, 0x75, 0x03, 0x25, 0x05, 0x09, 0x51, 0x81, 0x02, // contact id
        0x75, 0x01, 0x95, 0x03, 0x81, 0x03, // padding
        0x05, 0x01, 0x15, 0x00, 0x26, 0x10, 0x27, 0x75, 0x10, 0x55, 0x0E, 0x65, 0x11, // 0..10000, cm × 10^-2
        0x09, 0x30, 0x35, 0x00, 0x46, 0xE8, 0x03, 0x95, 0x01, 0x81, 0x02, // X: 10.00 cm
        0x26, 0x70, 0x17, 0x46, 0x58, 0x02, 0x09, 0x31, 0x81, 0x02, // Y: 0..6000, 6.00 cm
        0xC0,
    ];
    let mut d = vec![0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01];
    d.extend_from_slice(finger);
    d.extend_from_slice(finger);
    d.extend_from_slice(&[
        0x05, 0x0D, 0x55, 0x0C, 0x66, 0x01, 0x10, 0x47, 0xFF, 0xFF, 0x00, 0x00, 0x27, 0xFF, 0xFF, 0x00, 0x00, 0x75,
        0x10, 0x95, 0x01, 0x09, 0x56, 0x81, 0x02, // scan time
        0x09, 0x54, 0x25, 0x7F, 0x95, 0x01, 0x75, 0x08, 0x81, 0x02, // contact count
        0x05, 0x09, 0x09, 0x01, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, // button
        0x05, 0x0D, 0x85, 0x02, 0x09, 0x55, 0x09, 0x59, 0x75, 0x04, 0x95, 0x02, 0x25, 0x0F, 0xB1,
        0x02, // max, pad type
        0xC0, //
        0x05, 0x0D, 0x09, 0x0E, 0xA1, 0x01, 0x85, 0x03, 0x09, 0x22, 0xA1, 0x02, // configuration
        0x09, 0x52, 0x15, 0x00, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x02, 0xC0, // input mode
        0x09, 0x22, 0xA1, 0x00, 0x85, 0x04, 0x09, 0x57, 0x09, 0x58, 0x75, 0x01, 0x95, 0x02, 0x25, 0x01, 0xB1, 0x02,
        0x95, 0x06, 0xB1, 0x03, 0xC0, 0xC0, // selective reporting
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x05, 0x09, 0x01, 0xA1, 0x00, // legacy mouse, report 5
        0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03,
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xC0, 0xC0,
    ]);
    d
}

/// Sony DualShock 4 (054C:05C4), the start of its USB descriptor: report 1.
const DUALSHOCK4: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, 0x15, 0x00, 0x26,
    0xFF, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x35, 0x00, 0x46, 0x3B, 0x01,
    0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0E, 0x15, 0x00, 0x25,
    0x01, 0x75, 0x01, 0x95, 0x0E, 0x81, 0x02, 0x06, 0x00, 0xFF, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25,
    0x7F, 0x81, 0x02, 0x05, 0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81,
    0x02, 0x06, 0x00, 0xFF, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, 0x85, 0x05, 0x09, 0x22, 0x95, 0x1F, 0x91, 0x02, 0xC0,
];

/// A generic USB pad (DragonRise 0079:0006 style): X, Y, Z, Z, Rz; a hat;
/// twelve buttons.
const GENERIC_PAD: &[u8] = &[
    0x05, 0x01, 0x09, 0x04, 0xA1, 0x01, 0xA1, 0x02, 0x75, 0x08, 0x95, 0x05, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x35, 0x00,
    0x46, 0xFF, 0x00, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x32, 0x09, 0x35, 0x81, 0x02, 0x75, 0x04, 0x95, 0x01,
    0x25, 0x07, 0x46, 0x3B, 0x01, 0x65, 0x14, 0x09, 0x39, 0x81, 0x42, 0x65, 0x00, 0x75, 0x01, 0x95, 0x0C, 0x25, 0x01,
    0x45, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0C, 0x81, 0x02, 0x06, 0x00, 0xFF, 0x75, 0x01, 0x95, 0x08, 0x25, 0x01,
    0x45, 0x01, 0x09, 0x01, 0x81, 0x02, 0xC0, 0xC0,
];

fn parse(d: &[u8]) -> Descriptor {
    Descriptor::parse(d).expect("descriptor parses")
}

// ------------------------------------------------------------ descriptors

#[test]
fn boot_keyboard_layout() {
    let d = parse(BOOT_KEYBOARD);
    assert!(!d.numbered);
    assert_eq!(d.applications(), vec![usage::KEYBOARD]);
    assert_eq!(d.report_len(Kind::Input, 0), 8);
    assert_eq!(d.report_len(Kind::Output, 0), 1);
    let (f, e) = d.find(Kind::Output, usage::LED_CAPS_LOCK).unwrap();
    assert_eq!((f.offset + e * f.size, f.size), (1, 1));
}

#[test]
fn malformed_descriptors_are_errors() {
    assert!(Descriptor::parse(&[0x05]).is_err(), "truncated item");
    assert!(Descriptor::parse(&[0xC0]).is_err(), "end collection without collection");
    assert!(Descriptor::parse(&[0x85, 0x00]).is_err(), "report id 0");
    assert!(Descriptor::parse(&[0x97, 0xFF, 0xFF, 0x00, 0x00, 0x75, 0x08, 0x81, 0x02]).is_err(), "huge count");
}

// ------------------------------------------------------------ keyboards

#[test]
fn boot_keyboard_keys_and_rollover() {
    let d = parse(BOOT_KEYBOARD);
    let mut k = keyboard::Keys::new();
    // Left Shift + a.
    let c = k.feed(&d, &[0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    assert!(c.contains(&keyboard::Change { usage: usage::u(7, 0xE1), pressed: true }));
    assert!(c.contains(&keyboard::Change { usage: usage::u(7, 0x04), pressed: true }));
    // Phantom state ("too many keys"): nothing changes.
    assert!(k.feed(&d, &[0x02, 0, 1, 1, 1, 1, 1, 1]).is_empty());
    // a up, b down, shift still held.
    let c = k.feed(&d, &[0x02, 0, 0x05, 0, 0, 0, 0, 0]);
    assert_eq!(
        c,
        vec![
            keyboard::Change { usage: usage::u(7, 0x04), pressed: false },
            keyboard::Change { usage: usage::u(7, 0x05), pressed: true }
        ]
    );
    assert_eq!(keyboard::led_report(&d, false, true, false), Some(vec![0x02]));
    assert_eq!(keyboard::led_report(&d, true, true, true), Some(vec![0x07]));
}

#[test]
fn nkro_media_and_power_keys() {
    let d = parse(NKRO_KEYBOARD);
    assert!(d.numbered);
    let mut k = keyboard::Keys::new();
    // Ten keys at once (beyond the boot protocol's six): usages 4..=13.
    let mut r = vec![0u8; 1 + d.report_len(Kind::Input, 1)];
    r[0] = 1;
    for u in 4..=13u32 {
        let bit = 8 + u;
        r[1 + (bit / 8) as usize] |= 1 << (bit % 8);
    }
    let c = k.feed(&d, &r);
    assert_eq!(c.iter().filter(|c| c.pressed).count(), 10);
    // Volume up (consumer 0xE9) on report 2, then released.
    let c = k.feed(&d, &[2, 0xE9, 0x00]);
    assert_eq!(c, vec![keyboard::Change { usage: usage::VOLUME_UP, pressed: true }]);
    let c = k.feed(&d, &[2, 0, 0]);
    assert_eq!(c, vec![keyboard::Change { usage: usage::VOLUME_UP, pressed: false }]);
    // The keyboard's own keys were not released by the consumer report.
    assert_eq!(k.all_held().len(), 10);
    // Sleep (system control).
    let c = k.feed(&d, &[3, 0x02]);
    assert_eq!(c, vec![keyboard::Change { usage: usage::SYSTEM_SLEEP, pressed: true }]);
    // LEDs on report 1.
    assert_eq!(keyboard::led_report(&d, false, true, false), Some(vec![1, 0x02]));
}

// ------------------------------------------------------------- pointers

#[test]
fn wheel_mouse() {
    let d = parse(WHEEL_MOUSE);
    assert!(pointer::is_pointer(&d));
    // Back button (4), right 3, up 2, wheel -1, pan +1.
    let r = pointer::read(&d, &[0x08, 3, 0xFE, 0xFF, 0x01]).unwrap();
    assert!(!r.absolute);
    assert_eq!((r.x, r.y, r.buttons, r.wheel, r.pan), (Some(3), Some(-2), 0x08, -1, 1));
    let r = pointer::read(&d, &[0x13, 0, 0, 0, 0]).unwrap();
    assert_eq!(r.buttons, 0x13, "left, right and forward");
}

#[test]
fn absolute_tablet() {
    let d = parse(QEMU_TABLET);
    let r = pointer::read(&d, &[0x01, 0xFF, 0x3F, 0x00, 0x40, 0x00]).unwrap();
    assert!(r.absolute);
    assert_eq!(r.buttons, 1);
    assert_eq!(pointer::scale(r.x.unwrap(), r.x_range, 1280), 639);
    assert_eq!(pointer::scale(r.y.unwrap(), r.y_range, 800), 399);
}

// ------------------------------------------------------------ touchpads

fn touch_report(d: &Descriptor, contacts: &[(u8, bool, u16, u16)], count: u8, button: bool) -> Vec<u8> {
    let mut r = d.new_report(Kind::Input, 1);
    for (slot, &(id, tip, x, y)) in contacts.iter().enumerate() {
        let base = 1 + slot * 5;
        r[base] = 0x01 | (tip as u8) << 1 | id << 2;
        r[base + 1..base + 3].copy_from_slice(&x.to_le_bytes());
        r[base + 3..base + 5].copy_from_slice(&y.to_le_bytes());
    }
    r[13] = count;
    r[14] = button as u8;
    r
}

#[test]
fn touchpad_contacts_and_mode() {
    let d = parse(&precision_touchpad());
    assert!(touchpad::is_touchpad(&d));
    assert!(!pointer::is_pointer(&d) || d.has_application(usage::MOUSE));
    let info = touchpad::info(&d).unwrap();
    assert_eq!((info.x_max, info.y_max), (10000, 6000));
    assert_eq!((info.width_hmm, info.height_hmm), (Some(10000), Some(6000)), "100 × 60 mm");
    assert_eq!(info.max_contacts, 2, "finger slots per report");
    assert_eq!(touchpad::input_mode_report(&d), Some(vec![3, 3]));
    assert_eq!(touchpad::selective_reporting_report(&d), Some(vec![4, 0x03]));
    let r = touchpad::read(&d, &touch_report(&d, &[(1, true, 5000, 3000), (2, true, 6000, 3100)], 2, true)).unwrap();
    assert_eq!(r.count, Some(2));
    assert!(r.button);
    assert_eq!(r.contacts[1], Contact { id: 2, x: 6000, y: 3100, tip: true, confident: true }, "second finger");
}

#[test]
fn hybrid_frames() {
    let d = parse(&precision_touchpad());
    let mut f = touchpad::Frames::new();
    // Three fingers, two per report: 3 in the first report, 0 in the second.
    let a = touchpad::read(&d, &touch_report(&d, &[(1, true, 10, 10), (2, true, 20, 20)], 3, false)).unwrap();
    assert!(f.feed(a).is_none());
    let b = touchpad::read(&d, &touch_report(&d, &[(3, true, 30, 30), (0, false, 0, 0)], 0, false)).unwrap();
    let frame = f.feed(b).unwrap();
    assert_eq!(frame.contacts.iter().map(|c| c.id).collect::<Vec<_>>(), vec![1, 2, 3]);
}

// ------------------------------------------------------------- gestures

fn pad() -> Gestures {
    Gestures::new(&touchpad::info(&parse(&precision_touchpad())).unwrap())
}

fn frame(fingers: &[(u32, i32, i32)], button: bool) -> Frame {
    Frame {
        contacts: fingers.iter().map(|&(id, x, y)| Contact { id, x, y, tip: true, confident: true }).collect(),
        button,
    }
}

fn lift() -> Frame {
    Frame::default()
}

#[test]
fn one_finger_moves_the_pointer_faster_when_quick() {
    let s = Settings::default();
    let mut g = pad();
    g.frame(0, &frame(&[(1, 5000, 3000)], false), &s);
    // 1 mm (100 units) slowly, then 10 mm at once.
    let slow = g.frame(100, &frame(&[(1, 5100, 3000)], false), &s);
    assert!((2..=4).contains(&slow.dx), "about 3 px per mm, got {}", slow.dx);
    let fast = g.frame(108, &frame(&[(1, 6100, 3000)], false), &s);
    assert!(fast.dx > 10 * slow.dx, "acceleration: {} vs {}", fast.dx, slow.dx);
    assert_eq!(fast.buttons, 0);
}

#[test]
fn taps_click() {
    let s = Settings::default();
    let mut g = pad();
    g.frame(0, &frame(&[(1, 5000, 3000)], false), &s);
    let o = g.frame(80, &lift(), &s);
    assert_eq!(o.buttons, 1, "a one-finger tap clicks");
    let (o, next) = g.tick(80 + 60);
    assert_eq!(o.map(|o| o.buttons), Some(0), "and lets go");
    assert!(next.is_none());
    // Two fingers: secondary click.
    g.frame(1000, &frame(&[(1, 5000, 3000), (2, 6000, 3000)], false), &s);
    assert_eq!(g.frame(1100, &lift(), &s).buttons, 2);
    // A long touch is not a tap; nor is one with tap to click off.
    g.tick(2000);
    g.frame(3000, &frame(&[(1, 5000, 3000)], false), &s);
    assert_eq!(g.frame(3500, &lift(), &s).buttons, 0);
    let off = Settings { tap_to_click: false, ..s };
    g.frame(4000, &frame(&[(1, 5000, 3000)], false), &off);
    assert_eq!(g.frame(4050, &lift(), &off).buttons, 0);
}

#[test]
fn tap_then_touch_drags() {
    let s = Settings::default();
    let mut g = pad();
    g.frame(0, &frame(&[(1, 5000, 3000)], false), &s);
    g.frame(60, &lift(), &s);
    g.tick(120); // the click ends
                 // Touch again quickly and move: the button is held while dragging.
    let o = g.frame(200, &frame(&[(2, 5000, 3000)], false), &s);
    assert_eq!(o.buttons, 1);
    let o = g.frame(300, &frame(&[(2, 5500, 3000)], false), &s);
    assert_eq!(o.buttons, 1);
    assert!(o.dx > 0);
    assert_eq!(g.frame(400, &lift(), &s).buttons, 0, "lifting ends the drag");
}

#[test]
fn clickpad_two_finger_click() {
    let s = Settings::default();
    let mut g = pad();
    assert_eq!(g.frame(0, &frame(&[(1, 5000, 3000)], true), &s).buttons, 1);
    assert_eq!(g.frame(50, &frame(&[(1, 5000, 3000)], false), &s).buttons, 0);
    assert_eq!(g.frame(100, &frame(&[(1, 5000, 3000), (2, 6000, 3000)], true), &s).buttons, 2);
}

#[test]
fn two_fingers_scroll_and_glide() {
    let s = Settings::default();
    let mut g = pad();
    g.frame(0, &frame(&[(1, 4000, 3000), (2, 5000, 3000)], false), &s);
    // Fingers move up 5 mm: with natural scrolling the view moves down.
    let mut total = 0;
    let mut t = 0;
    for step in 1..=5 {
        t = step * 10;
        let y = 3000 - step as i32 * 100;
        let o: Output = g.frame(t, &frame(&[(1, 4000, y), (2, 5000, y)], false), &s);
        assert_eq!(o.dx, 0, "scrolling doesn't move the pointer");
        total += o.scroll_y;
    }
    assert!((45..=55).contains(&total), "about 10 px per mm, got {total}");
    assert_eq!(g.frame(t + 10, &lift(), &s).buttons, 0, "a scroll is not a tap");
    // A quick flick keeps gliding, slower and slower, then stops.
    let (o, next) = g.tick(t + 26);
    assert!(o.unwrap().scroll_y > 0);
    assert_eq!(next, Some(16));
    let mut glides = 1;
    let mut now = t + 26;
    while g.tick(now).1.is_some() {
        now += 16;
        glides += 1;
        assert!(glides < 200, "gliding stops");
    }
    // Traditional direction: fingers up scroll up.
    let trad = Settings { natural_scrolling: false, ..s };
    let mut g = pad();
    g.frame(0, &frame(&[(1, 4000, 3000), (2, 5000, 3000)], false), &trad);
    assert!(g.frame(20, &frame(&[(1, 4000, 2800), (2, 5000, 2800)], false), &trad).scroll_y < 0);
}

#[test]
fn palms_are_ignored() {
    let s = Settings::default();
    let mut g = pad();
    let palm = |x| Frame { contacts: vec![Contact { id: 9, x, y: 3000, tip: true, confident: false }], button: false };
    g.frame(0, &palm(5000), &s);
    let o = g.frame(50, &palm(7000), &s);
    assert_eq!((o.dx, o.buttons), (0, 0));
}

// ------------------------------------------------------------ gamepads

#[test]
fn dualshock4() {
    let d = parse(DUALSHOCK4);
    let pad = gamepad::HidGamepad::new(&d, 0x054C, 0x05C4).unwrap();
    assert_eq!(pad.layout(), Layout::PlayStation);
    let mut r = vec![0u8; 64];
    r[0] = 0x01;
    r[1..5].copy_from_slice(&[0x00, 0xFF, 0x80, 0x80]); // left stick left and down; right centred
    r[5] = 0x02 | 0x20; // hat: right; cross
    r[6] = 0x01 | 0x20; // L1, options
    r[7] = 0x01; // PS
    r[8] = 0xFF; // L2 fully
    let s = pad.read(&d, &r).unwrap();
    assert_eq!(s.axes[0], -32768);
    assert_eq!(s.axes[1], 32767);
    assert!(s.axes[2].abs() < 300);
    for b in [button::RIGHT, button::SOUTH, button::LEFT_SHOULDER, button::START, button::GUIDE, button::LEFT_TRIGGER] {
        assert!(s.pressed(b), "button {b}");
    }
    assert!(!s.pressed(button::UP) && !s.pressed(button::EAST));
    assert_eq!(s.triggers, [65535, 0]);
    // Hat "centred" (8) presses nothing.
    r[5] = 0x08;
    assert_eq!(pad.read(&d, &r).unwrap().buttons & 0xF000, 0);
    assert_eq!(gamepad::playstation_rumble(0x05C4, 200, 100)[4..6], [100, 200]);
}

#[test]
fn generic_pad() {
    let d = parse(GENERIC_PAD);
    let pad = gamepad::HidGamepad::new(&d, 0x0079, 0x0006).unwrap();
    assert_eq!(pad.layout(), Layout::Generic);
    // Bytes: X, Y, Z, Z, Rz; hat (low nibble) and buttons 1–4; buttons 5–12; vendor.
    // Left stick centred, right stick left and down; hat up; buttons 1 and 7.
    let s = pad.read(&d, &[0x80, 0x80, 0x00, 0x80, 0xFF, 0x10, 0x04, 0x00]).unwrap();
    assert!(s.pressed(button::UP) && !s.pressed(button::DOWN));
    assert!(s.pressed(button::SOUTH));
    assert!(s.pressed(button::LEFT_TRIGGER));
    assert_eq!(s.axes[2], -32768);
    assert_eq!(s.axes[3], 32767, "right stick on Z and Rz");
    assert_eq!(s.triggers, [65535, 0], "a digital trigger reads as fully pressed");
    // Hat out of range (centred): no d-pad.
    let s = pad.read(&d, &[0x80, 0x80, 0x80, 0x80, 0x80, 0x0F, 0x00, 0x00]).unwrap();
    assert_eq!(s.buttons, 0);
}

#[test]
fn xbox_controllers() {
    // Xbox 360: A and up held, left stick fully up, right trigger half.
    let mut r = [0u8; 20];
    r[1] = 0x14;
    r[2] = 0x01;
    r[3] = 0x10;
    r[5] = 0x80;
    r[8..10].copy_from_slice(&i16::MAX.to_le_bytes());
    let s = gamepad::xbox360::read(&r).unwrap();
    assert!(s.pressed(button::SOUTH) && s.pressed(button::UP));
    assert_eq!(s.axes[1], i16::MIN, "up is negative in our layout");
    assert!(s.triggers[1] > 32000 && s.pressed(button::RIGHT_TRIGGER));
    assert!(gamepad::xbox360::read(&[0x01, 0x03, 0x02]).is_none(), "LED status is not input");

    // Xbox One: B and menu, then the guide button in its own message.
    let mut r = [0u8; 18];
    r[0] = 0x20;
    r[4] = 0x20 | 0x04;
    r[6..8].copy_from_slice(&1023u16.to_le_bytes());
    let s = gamepad::xbox_one::read(&r, &Default::default()).unwrap();
    assert!(s.pressed(button::EAST) && s.pressed(button::START));
    assert_eq!(s.triggers[0], 65535);
    let s = gamepad::xbox_one::read(&[0x07, 0x20, 0x00, 0x02, 0x01, 0x5B], &s).unwrap();
    assert!(s.pressed(button::GUIDE) && s.pressed(button::EAST));
    let s = gamepad::xbox_one::read(&r, &s).unwrap();
    assert!(s.pressed(button::GUIDE), "the guide button stays held across input messages");
    assert_eq!(gamepad::xbox_one::start(), [0x05, 0x20, 0x00, 0x01, 0x00]);
}

// ------------------------------------------------------------ robustness

/// Random descriptors and reports never panic (a small xorshift for repeatability).
#[test]
fn random_input_never_panics() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let known: Vec<Vec<u8>> = vec![
        BOOT_KEYBOARD.to_vec(),
        NKRO_KEYBOARD.to_vec(),
        WHEEL_MOUSE.to_vec(),
        QEMU_TABLET.to_vec(),
        precision_touchpad(),
        DUALSHOCK4.to_vec(),
        GENERIC_PAD.to_vec(),
    ];
    for round in 0..20_000 {
        // Mutate a real descriptor, or make one up.
        let mut desc = if round % 2 == 0 { known[round % known.len()].clone() } else { Vec::new() };
        let len = (next() % 96) as usize;
        if desc.is_empty() {
            desc = (0..len).map(|_| next() as u8).collect();
        } else {
            for _ in 0..(next() % 4) {
                let i = (next() as usize) % desc.len();
                desc[i] = next() as u8;
            }
        }
        let Ok(d) = Descriptor::parse(&desc) else { continue };
        let report: Vec<u8> = (0..(next() % 70)).map(|_| next() as u8).collect();
        let _ = keyboard::Keys::new().feed(&d, &report);
        let _ = keyboard::led_report(&d, true, true, true);
        let _ = pointer::read(&d, &report);
        let _ = touchpad::read(&d, &report);
        let _ = touchpad::info(&d);
        let _ = touchpad::input_mode_report(&d);
        if let Some(p) = gamepad::HidGamepad::new(&d, 0x054C, 0x05C4) {
            let _ = p.read(&d, &report);
        }
        let _ = gamepad::xbox360::read(&report);
        let _ = gamepad::xbox_one::read(&report, &Default::default());
    }
}
