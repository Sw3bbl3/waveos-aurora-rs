//! The internal panel's PWM backlight (south display, controller 0).
//!
//! The driver only adjusts a PWM the firmware already enabled: it reads
//! the period the firmware chose and changes the duty cycle. Polarity
//! (`BLC_PWM_POLARITY`) inverts the duty. Brightness never goes below
//! [`MIN_PERCENT`], so the panel can't be turned black by accident.
//!
//! Panels whose brightness is set over DPCD (many OLED and HDR panels)
//! are not handled; [`Backlight::probe`] finds no enabled PWM there.

use crate::platform::BacklightLayout;
use crate::regs::*;
use crate::Mmio;

pub const MIN_PERCENT: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backlight {
    pub layout: BacklightLayout,
    pub period: u32,
    pub inverted: bool,
}

impl Backlight {
    /// The firmware's PWM, if it is enabled with a usable period.
    pub fn probe(m: &impl Mmio, layout: BacklightLayout) -> Option<Backlight> {
        let ctl1 = m.read(BLC_PWM_CTL1);
        if ctl1 & BLC_PWM_ENABLE == 0 {
            return None;
        }
        let period = match layout {
            BacklightLayout::Split16 => m.read(BLC_PWM_CTL2) >> 16,
            BacklightLayout::Bxt => m.read(BLC_PWM_CTL2),
        };
        // A period this short can't give useful steps.
        if period < 20 {
            return None;
        }
        Some(Backlight { layout, period, inverted: ctl1 & BLC_PWM_POLARITY != 0 })
    }

    fn duty_raw(&self, m: &impl Mmio) -> u32 {
        match self.layout {
            BacklightLayout::Split16 => m.read(BLC_PWM_CTL2) & 0xFFFF,
            BacklightLayout::Bxt => m.read(BLC_PWM_DUTY),
        }
    }

    /// The level now, 0–100.
    pub fn get(&self, m: &impl Mmio) -> u32 {
        let mut duty = self.duty_raw(m).min(self.period);
        if self.inverted {
            duty = self.period - duty;
        }
        percent_of(duty, self.period)
    }

    /// The duty register value for `percent`.
    pub fn duty_for(&self, percent: u32) -> u32 {
        let p = percent.clamp(MIN_PERCENT, 100);
        let duty = ((self.period as u64 * p as u64 + 50) / 100) as u32;
        if self.inverted {
            self.period - duty
        } else {
            duty
        }
    }

    pub fn set(&self, m: &impl Mmio, percent: u32) {
        let duty = self.duty_for(percent);
        match self.layout {
            BacklightLayout::Split16 => m.write(BLC_PWM_CTL2, self.period << 16 | (duty & 0xFFFF)),
            BacklightLayout::Bxt => m.write(BLC_PWM_DUTY, duty),
        }
    }
}

pub fn percent_of(duty: u32, period: u32) -> u32 {
    if period == 0 {
        return 0;
    }
    ((duty as u64 * 100 + period as u64 / 2) / period as u64) as u32
}
