//! Keyboard layouts: USB HID usage + modifiers → character.
//!
//! Both keyboard drivers speak HID usages (PS/2 translates its scancodes,
//! USB reports them directly), so one set of tables serves both. Each layout
//! lists what the 48 keys of the main block produce, in four layers: plain,
//! Shift, AltGr and AltGr+Shift. A combining accent in a table is a dead key:
//! it waits for the next letter and combines with it (^ then e → ê).

use core::sync::atomic::{AtomicU8, Ordering};

/// The keys the tables describe, in table order (HID usages).
const KEYS: [u8; 48] = [
    0x35, // ` (left of 1)
    0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, // 1–0
    0x2D, 0x2E, // - =
    0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13, // q–p
    0x2F, 0x30, // [ ]
    0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F, // a–l
    0x33, 0x34, 0x31, // ; ' \ (the key next to Enter)
    0x64, // the ISO key left of Z
    0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10, // z–m
    0x36, 0x37, 0x38, // , . /
];

pub struct Layout {
    pub id: &'static str,
    pub name: &'static str,
    /// What the 48 keys produce plain and with Shift, in `KEYS` order.
    plain: &'static str,
    shift: &'static str,
    /// Extra characters with AltGr (right Alt, or Ctrl+Alt): (usage, char).
    altgr: &'static [(u8, char)],
}

const DEAD_GRAVE: char = '\u{300}';
const DEAD_ACUTE: char = '\u{301}';
const DEAD_CIRCUMFLEX: char = '\u{302}';
const DEAD_TILDE: char = '\u{303}';
const DEAD_DIAERESIS: char = '\u{308}';

pub const LAYOUTS: [Layout; 6] = [
    Layout {
        id: "us",
        name: "U.S.",
        plain: "`1234567890-=qwertyuiop[]asdfghjkl;'\\\\zxcvbnm,./",
        shift: "~!@#$%^&*()_+QWERTYUIOP{}ASDFGHJKL:\"||ZXCVBNM<>?",
        altgr: &[],
    },
    Layout {
        id: "uk",
        name: "British",
        plain: "`1234567890-=qwertyuiop[]asdfghjkl;'#\\zxcvbnm,./",
        shift: "¬!\"£$%^&*()_+QWERTYUIOP{}ASDFGHJKL:@~|ZXCVBNM<>?",
        altgr: &[(0x35, '¦'), (0x21, '€'), (0x08, 'é'), (0x18, 'ú'), (0x0C, 'í'), (0x12, 'ó'), (0x04, 'á')],
    },
    Layout {
        id: "de",
        name: "German",
        plain: "\u{302}1234567890ß\u{301}qwertzuiopü+asdfghjklöä#<yxcvbnm,.-",
        shift: "°!\"§$%&/()=?\u{300}QWERTZUIOPÜ*ASDFGHJKLÖÄ'>YXCVBNM;:_",
        altgr: &[
            (0x1F, '²'),
            (0x20, '³'),
            (0x24, '{'),
            (0x25, '['),
            (0x26, ']'),
            (0x27, '}'),
            (0x2D, '\\'),
            (0x14, '@'),
            (0x08, '€'),
            (0x30, '~'),
            (0x64, '|'),
            (0x10, 'µ'),
        ],
    },
    Layout {
        id: "fr",
        name: "French",
        plain: "²&é\"'(-è_çà)=azertyuiop\u{302}$qsdfghjklmù*<wxcvbn,;:!",
        shift: "²1234567890°+AZERTYUIOP\u{308}£QSDFGHJKLM%µ>WXCVBN?./§",
        altgr: &[
            (0x1F, DEAD_TILDE),
            (0x20, '#'),
            (0x21, '{'),
            (0x22, '['),
            (0x23, '|'),
            (0x24, '`'),
            (0x25, '\\'),
            (0x26, '^'),
            (0x27, '@'),
            (0x2D, ']'),
            (0x2E, '}'),
            (0x08, '€'),
            (0x30, '¤'),
        ],
    },
    Layout {
        id: "es",
        name: "Spanish",
        plain: "º1234567890'¡qwertyuiop\u{300}+asdfghjklñ\u{301}ç<zxcvbnm,.-",
        shift: "ª!\"·$%&/()=?¿QWERTYUIOP\u{302}*ASDFGHJKLÑ\u{308}Ç>ZXCVBNM;:_",
        altgr: &[
            (0x35, '\\'),
            (0x1E, '|'),
            (0x1F, '@'),
            (0x20, '#'),
            (0x21, '~'),
            (0x22, '€'),
            (0x23, '¬'),
            (0x08, '€'),
            (0x2F, '['),
            (0x30, ']'),
            (0x34, '{'),
            (0x31, '}'),
        ],
    },
    Layout {
        id: "se",
        name: "Swedish",
        plain: "§1234567890+\u{301}qwertyuiopå\u{308}asdfghjklöä'<zxcvbnm,.-",
        shift: "½!\"#¤%&/()=?\u{300}QWERTYUIOPÅ\u{302}ASDFGHJKLÖÄ*>ZXCVBNM;:_",
        altgr: &[
            (0x1F, '@'),
            (0x20, '£'),
            (0x21, '$'),
            (0x22, '€'),
            (0x24, '{'),
            (0x25, '['),
            (0x26, ']'),
            (0x27, '}'),
            (0x2D, '\\'),
            (0x08, '€'),
            (0x30, '~'),
            (0x64, '|'),
            (0x10, 'µ'),
        ],
    },
];

static CURRENT: AtomicU8 = AtomicU8::new(0);
/// A dead key waiting for its letter.
static PENDING: AtomicU8 = AtomicU8::new(0);

pub fn current() -> &'static Layout {
    &LAYOUTS[CURRENT.load(Ordering::Relaxed) as usize % LAYOUTS.len()]
}

pub fn set(id: &str) -> bool {
    match LAYOUTS.iter().position(|l| l.id == id) {
        Some(i) => {
            if CURRENT.swap(i as u8, Ordering::Relaxed) != i as u8 {
                log!("kbd", "keyboard layout: {}", LAYOUTS[i].name);
            }
            PENDING.store(0, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

fn lookup(layout: &Layout, usage: u8, shift: bool, altgr: bool) -> Option<char> {
    if altgr {
        return layout.altgr.iter().find(|(u, _)| *u == usage).map(|(_, c)| *c);
    }
    let k = KEYS.iter().position(|&u| u == usage)?;
    if shift {
        layout.shift.chars().nth(k)
    } else {
        layout.plain.chars().nth(k)
    }
}

fn is_dead(c: char) -> bool {
    matches!(c, DEAD_GRAVE | DEAD_ACUTE | DEAD_CIRCUMFLEX | DEAD_TILDE | DEAD_DIAERESIS)
}

/// The spacing form of a dead accent (typed on its own or before a space).
fn spacing(dead: char) -> char {
    match dead {
        DEAD_GRAVE => '`',
        DEAD_ACUTE => '´',
        DEAD_CIRCUMFLEX => '^',
        DEAD_TILDE => '~',
        _ => '¨',
    }
}

fn compose(dead: char, base: char) -> Option<char> {
    const TABLE: [(char, &str, &str); 5] = [
        (DEAD_GRAVE, "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        (DEAD_ACUTE, "aeiouyAEIOUY", "áéíóúýÁÉÍÓÚÝ"),
        (DEAD_CIRCUMFLEX, "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        (DEAD_TILDE, "anoANO", "ãñõÃÑÕ"),
        (DEAD_DIAERESIS, "aeiouyAEIOUY", "äëïöüÿÄËÏÖÜŸ"),
    ];
    let (_, from, to) = TABLE.iter().find(|(d, _, _)| *d == dead)?;
    let i = from.chars().position(|c| c == base)?;
    to.chars().nth(i)
}

/// What a key press produces. Dead keys return `None` and remember the accent.
pub fn translate(usage: u8, shift: bool, caps: bool, altgr: bool) -> Option<char> {
    let layout = current();
    let base = lookup(layout, usage, false, false)?;
    // Caps Lock shifts letters only.
    let shifted = shift ^ (caps && base.is_alphabetic());
    let c = lookup(layout, usage, shifted, altgr)?;
    let pending = PENDING.swap(0, Ordering::Relaxed);
    if is_dead(c) {
        if pending != 0 {
            // Two dead keys: emit the first accent, keep the second.
            PENDING.store(dead_index(c), Ordering::Relaxed);
            return Some(spacing(dead_from_index(pending)));
        }
        PENDING.store(dead_index(c), Ordering::Relaxed);
        return None;
    }
    if pending != 0 {
        let dead = dead_from_index(pending);
        return Some(compose(dead, c).unwrap_or(c));
    }
    Some(c)
}

/// Space after a dead key types the accent itself.
pub fn space() -> char {
    let pending = PENDING.swap(0, Ordering::Relaxed);
    if pending != 0 {
        spacing(dead_from_index(pending))
    } else {
        ' '
    }
}

fn dead_index(c: char) -> u8 {
    match c {
        DEAD_GRAVE => 1,
        DEAD_ACUTE => 2,
        DEAD_CIRCUMFLEX => 3,
        DEAD_TILDE => 4,
        _ => 5,
    }
}

fn dead_from_index(i: u8) -> char {
    [DEAD_GRAVE, DEAD_GRAVE, DEAD_ACUTE, DEAD_CIRCUMFLEX, DEAD_TILDE, DEAD_DIAERESIS][i as usize % 6]
}

/// Checks the tables at boot (a wrong count would shift every key).
pub fn validate() {
    for l in &LAYOUTS {
        for (which, layer) in [("plain", l.plain), ("shift", l.shift)] {
            let n = layer.chars().count();
            if n != KEYS.len() {
                log!("kbd", "layout {} ({}) has {} keys, expected {}", l.id, which, n, KEYS.len());
            }
        }
    }
}
