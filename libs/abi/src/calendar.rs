//! Calendar conversion shared by clock presentation and UTC network time.
use crate::DateTime;

pub fn from_seconds_since_2000(seconds: u64) -> DateTime {
    let days = (seconds / 86400) as i64 + 10957;
    let rem = seconds % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + i64::from(month <= 2);
    DateTime {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: (rem / 3600) as u8,
        minute: (rem / 60 % 60) as u8,
        second: (rem % 60) as u8,
        weekday: (days + 4).rem_euclid(7) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn epoch_leap_day_and_offset_rollover() {
        let epoch = from_seconds_since_2000(0);
        assert_eq!((epoch.year, epoch.month, epoch.day, epoch.weekday), (2000, 1, 1, 6));
        let leap = from_seconds_since_2000(59 * 86400);
        assert_eq!((leap.month, leap.day), (2, 29));
        let next = from_seconds_since_2000(60 * 86400);
        assert_eq!((next.month, next.day), (3, 1));
        let midnight = from_seconds_since_2000(366 * 86400 - 3600);
        assert_eq!((midnight.year, midnight.month, midnight.day, midnight.hour), (2000, 12, 31, 23));
    }
}
