//! CMOS real-time clock.

use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

fn cmos(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg | 0x80); // keep NMI disabled bit set
        Port::<u8>::new(0x71).read()
    }
}

fn raw() -> [u8; 7] {
    while cmos(0x0A) & 0x80 != 0 {}
    [cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09), cmos(0x32)]
}

pub fn now() -> DateTime {
    without_interrupts(|| {
        let mut r = raw();
        loop {
            let again = raw();
            if again == r {
                break;
            }
            r = again;
        }
        let status_b = cmos(0x0B);
        let bcd = |v: u8| if status_b & 0x04 == 0 { (v & 0x0F) + (v >> 4) * 10 } else { v };
        let [s, m, h, d, mo, y, c] = r;
        let pm = h & 0x80 != 0;
        let mut hour = bcd(h & 0x7F);
        if status_b & 0x02 == 0 {
            // 12-hour mode
            hour %= 12;
            if pm {
                hour += 12;
            }
        }
        let century = if c != 0 && c != 0xFF { bcd(c) as u16 } else { 20 };
        DateTime {
            year: century * 100 + bcd(y) as u16,
            month: bcd(mo),
            day: bcd(d),
            hour,
            minute: bcd(m),
            second: bcd(s),
        }
    })
}

impl DateTime {
    /// 0 = Sunday.
    pub fn weekday(&self) -> u8 {
        const T: [u16; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let mut y = self.year;
        if self.month < 3 {
            y -= 1;
        }
        ((y + y / 4 - y / 100 + y / 400 + T[(self.month - 1) as usize] + self.day as u16) % 7) as u8
    }

    pub fn weekday_name(&self) -> &'static str {
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][self.weekday() as usize]
    }

    pub fn month_name(&self) -> &'static str {
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
            [(self.month.clamp(1, 12) - 1) as usize]
    }
}

fn cmos_write(reg: u8, value: u8) {
    unsafe {
        Port::<u8>::new(0x70).write(reg | 0x80);
        Port::<u8>::new(0x71).write(value);
    }
}

/// Sets the hardware clock (local time).
pub fn set(d: &DateTime) {
    without_interrupts(|| {
        let status_b = cmos(0x0B);
        let binary = status_b & 0x04 != 0;
        let enc = |v: u8| if binary { v } else { (v / 10) << 4 | (v % 10) };
        let hour = if status_b & 0x02 == 0 {
            // 12-hour mode: 12 AM = 12, PM flag in bit 7.
            let h12 = match d.hour % 12 {
                0 => 12,
                h => h,
            };
            enc(h12) | if d.hour >= 12 { 0x80 } else { 0 }
        } else {
            enc(d.hour)
        };
        // Halt updates while writing (SET bit), then resume.
        cmos_write(0x0B, status_b | 0x80);
        cmos_write(0x00, enc(d.second));
        cmos_write(0x02, enc(d.minute));
        cmos_write(0x04, hour);
        cmos_write(0x07, enc(d.day));
        cmos_write(0x08, enc(d.month));
        cmos_write(0x09, enc((d.year % 100) as u8));
        cmos_write(0x32, enc((d.year / 100) as u8));
        cmos_write(0x0B, status_b & !0x80);
    });
}
