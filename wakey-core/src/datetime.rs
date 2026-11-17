//! Calendar date and time without a time zone (the RTC stores UTC; the
//! firmware gets local time from the C library) and Unix time conversion.

use serde::{Deserialize, Serialize};

/// A calendar date (no time of day).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Date {
    pub year: u16,
    /// 1–12
    pub month: u8,
    /// 1–31
    pub day: u8,
}

/// A calendar date and time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    /// 1–12
    pub month: u8,
    /// 1–31
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday.
    pub weekday: u8,
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl DateTime {
    /// Field ranges only (a two-digit RTC year covers 2000–2099).
    pub fn plausible(&self) -> bool {
        (2000..=2099).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
            && self.weekday < 7
    }

    /// Seconds since 1970-01-01 00:00, reading the fields as UTC.
    pub fn to_unix(&self) -> i64 {
        days_from_civil(
            i64::from(self.year),
            u32::from(self.month),
            u32::from(self.day),
        ) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// The UTC date and time of a Unix time.
    pub fn from_unix(t: i64) -> Self {
        let days = t.div_euclid(86_400);
        let secs = t.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        Self {
            year: y as u16,
            month: m as u8,
            day: d as u8,
            hour: (secs / 3600) as u8,
            minute: (secs / 60 % 60) as u8,
            second: (secs % 60) as u8,
            // 1970-01-01 was a Thursday.
            weekday: (days + 4).rem_euclid(7) as u8,
        }
    }

    pub fn date(&self) -> Date {
        Date {
            year: self.year,
            month: self.month,
            day: self.day,
        }
    }

    /// Seconds since midnight.
    pub fn second_of_day(&self) -> u32 {
        u32::from(self.hour) * 3600 + u32::from(self.minute) * 60 + u32::from(self.second)
    }

    /// The same time of day `days` calendar days later (weekday adjusted).
    pub fn add_days(&self, days: i64) -> Self {
        Self::from_unix(self.to_unix() + days * 86_400)
    }

    /// "07:05:09"
    pub fn hms(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_round_trip() {
        for t in [0, 86_399, 951_782_400, 1_794_265_200, 4_102_444_800] {
            assert_eq!(DateTime::from_unix(t).to_unix(), t);
        }
    }

    #[test]
    fn known_date() {
        let d = DateTime::from_unix(1_794_265_200); // Monday 2026-11-09 23:00 UTC
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.weekday),
            (2026, 11, 9, 23, 0, 1)
        );
        assert!(d.plausible());
        assert_eq!(d.hms(), "23:00:00");
    }

    #[test]
    fn leap_day() {
        let d = DateTime::from_unix(1_709_164_800); // 2024-02-29
        assert_eq!((d.year, d.month, d.day), (2024, 2, 29));
    }

    #[test]
    fn add_days_crosses_months_and_keeps_the_time() {
        let d = DateTime::from_unix(1_709_164_800 + 6 * 3600 + 30 * 60); // Thursday 2024-02-29 06:30
        let next = d.add_days(1);
        assert_eq!((next.month, next.day, next.weekday), (3, 1, 5));
        assert_eq!(next.second_of_day(), d.second_of_day());
        assert_eq!(d.add_days(0), d);
        assert_eq!(d.add_days(7).weekday, d.weekday);
    }

    #[test]
    fn implausible() {
        let mut d = DateTime::from_unix(0);
        assert!(!d.plausible()); // 1970
        d.year = 2026;
        assert!(d.plausible());
        d.month = 13;
        assert!(!d.plausible());
    }
}
