//! The real-time clock: the CMOS RTC on x86, the PL031 on Arm (QEMU `virt`).

#[cfg(target_arch = "x86_64")]
use crate::arch::interrupts::without_interrupts;
#[cfg(target_arch = "x86_64")]
use crate::arch::io::Port;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// The CMOS index and data ports are one register pair for the whole
/// machine: a read on one CPU must not select a register under another's
/// (two programs reading the date at once got the day as the month, and
/// `weekday` indexed past its table — found by `fuzz`).
#[cfg(target_arch = "x86_64")]
static CMOS: crate::sync::IrqMutex<()> = crate::sync::IrqMutex::new(());

#[cfg(target_arch = "x86_64")]
fn cmos(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg | 0x80); // keep NMI disabled bit set
        Port::<u8>::new(0x71).read()
    }
}

#[cfg(target_arch = "x86_64")]
fn raw() -> [u8; 7] {
    while cmos(0x0A) & 0x80 != 0 {}
    [cmos(0x00), cmos(0x02), cmos(0x04), cmos(0x07), cmos(0x08), cmos(0x09), cmos(0x32)]
}

#[cfg(target_arch = "x86_64")]
pub fn now() -> DateTime {
    without_interrupts(|| {
        let _cmos = CMOS.lock();
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
        let month = self.month.clamp(1, 12);
        let mut y = self.year;
        if month < 3 {
            y = y.saturating_sub(1);
        }
        ((y + y / 4 - y / 100 + y / 400 + T[(month - 1) as usize] + self.day as u16) % 7) as u8
    }

    pub fn weekday_name(&self) -> &'static str {
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][self.weekday() as usize]
    }

    pub fn month_name(&self) -> &'static str {
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
            [(self.month.clamp(1, 12) - 1) as usize]
    }
}

#[cfg(target_arch = "x86_64")]
fn cmos_write(reg: u8, value: u8) {
    unsafe {
        Port::<u8>::new(0x70).write(reg | 0x80);
        Port::<u8>::new(0x71).write(value);
    }
}

/// Sets the hardware clock (local time).
#[cfg(target_arch = "x86_64")]
pub fn set(d: &DateTime) {
    without_interrupts(|| {
        let _cmos = CMOS.lock();
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

/// The PL031 on QEMU's `virt` (its fixed address there; the firmware
/// describes it only in the device tree). It counts seconds; the kernel
/// keeps local time in it, as in the CMOS RTC.
#[cfg(target_arch = "aarch64")]
const PL031: u64 = 0x0901_0000;

#[cfg(target_arch = "aarch64")]
fn pl031() -> *mut u32 {
    crate::mm::phys_to_virt(PL031) as *mut u32
}

/// Seconds from 1970 (the PL031's count) to 2000.
#[cfg(target_arch = "aarch64")]
const UNIX_2000: u64 = 946_684_800;

#[cfg(target_arch = "aarch64")]
pub fn now() -> DateTime {
    // SAFETY: the data register (offset 0) of the PL031, device-mapped in
    // the physical window.
    let unix = unsafe { pl031().read_volatile() } as u64;
    let d = aurora_abi::calendar::from_seconds_since_2000(unix.saturating_sub(UNIX_2000));
    DateTime { year: d.year, month: d.month, day: d.day, hour: d.hour, minute: d.minute, second: d.second }
}

/// Sets the hardware clock (local time).
#[cfg(target_arch = "aarch64")]
pub fn set(d: &DateTime) {
    let unix = crate::time::seconds_since_2000(d) + UNIX_2000;
    // SAFETY: the load register (offset 8) of the PL031.
    unsafe { pl031().add(2).write_volatile(unix as u32) };
}
