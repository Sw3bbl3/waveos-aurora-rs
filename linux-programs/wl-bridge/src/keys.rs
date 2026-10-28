//! Keyboards: the keymap clients get (a US layout, built by the client's
//! xkbcommon from the system's XKB data), and WaveOS keys as the Linux
//! (evdev) key codes that keymap expects.

pub const KEYMAP: &str = "xkb_keymap {
\txkb_keycodes { include \"evdev+aliases(qwerty)\" };
\txkb_types { include \"complete\" };
\txkb_compat { include \"complete\" };
\txkb_symbols { include \"pc+us+inet(evdev)\" };
};
";

/// XKB modifier masks for that keymap.
pub const SHIFT: u32 = 1;
pub const CAPS: u32 = 2;
pub const CTRL: u32 = 4;
pub const ALT: u32 = 8;
pub const LOGO: u32 = 64;

/// WaveOS modifier bits (`Event.d`) as XKB (depressed, locked) masks.
pub fn modifiers(bits: u32) -> (u32, u32) {
    let mut d = 0;
    for (w, x) in [(1, SHIFT), (2, CTRL), (4, ALT), (8, LOGO)] {
        if bits & w != 0 {
            d |= x;
        }
    }
    (d, if bits & 16 != 0 { CAPS } else { 0 })
}

/// The evdev code of WaveOS key `code` (`KeyCode` as u32) that typed `ch`.
pub fn evdev(code: u32, ch: u32) -> Option<u32> {
    Some(match code {
        0 => return char_key(char::from_u32(ch)?),
        1 => 1,    // Escape
        2 => 28,   // Enter
        3 => 14,   // Backspace
        4 => 15,   // Tab
        5 => 105,  // Left
        6 => 106,  // Right
        7 => 103,  // Up
        8 => 108,  // Down
        9 => 102,  // Home
        10 => 107, // End
        11 => 104, // Page Up
        12 => 109, // Page Down
        13 => 110, // Insert
        14 => 111, // Delete
        15..=24 => 59 + code - 15,
        25 => 87,  // F11
        26 => 88,  // F12
        27 => 42,  // Shift
        28 => 29,  // Ctrl
        29 => 56,  // Alt
        30 => 125, // Super
        31 => 58,  // Caps Lock
        32 => 127, // Menu
        33 => 99,  // Print Screen
        34 => 113, // Mute
        35 => 114,
        36 => 115,
        37 => 164, // Play/Pause
        38 => 163,
        39 => 165,
        40 => 158, // Back
        41 => 159, // Forward
        _ => return None,
    })
}

/// The key that types `c` on a US keyboard (with or without Shift).
fn char_key(c: char) -> Option<u32> {
    const LETTERS: [u32; 26] =
        [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44];
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
        '1'..='9' => 2 + (c as u32 - '1' as u32),
        '0' => 11,
        '!' => 2,
        '@' => 3,
        '#' => 4,
        '$' => 5,
        '%' => 6,
        '^' => 7,
        '&' => 8,
        '*' => 9,
        '(' => 10,
        ')' => 11,
        '-' | '_' => 12,
        '=' | '+' => 13,
        '[' | '{' => 26,
        ']' | '}' => 27,
        ';' | ':' => 39,
        '\'' | '"' => 40,
        '`' | '~' => 41,
        '\\' | '|' => 43,
        ',' | '<' => 51,
        '.' | '>' => 52,
        '/' | '?' => 53,
        ' ' => 57,
        '\t' => 15,
        '\n' | '\r' => 28,
        '\u{8}' => 14,
        '\u{1b}' => 1,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(evdev(0, 'q' as u32), Some(16));
        assert_eq!(evdev(0, 'Z' as u32), Some(44));
        assert_eq!(evdev(0, '!' as u32), Some(2));
        assert_eq!(evdev(15, 0), Some(59));
        assert_eq!(evdev(24, 0), Some(68));
        assert_eq!(modifiers(1 | 2 | 16), (SHIFT | CTRL, CAPS));
    }
}
