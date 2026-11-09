//! Epson RX8130CE real-time clock at I2C 0x32, backed by a 70 mF supercap
//! (≈ 7 h without battery). Time is kept in UTC; the time zone is applied
//! by the C library (see `crate::clock`).
//!
//! Its interrupt output is not wired to the P4 but to the PMS150 power
//! controller: the alarm is what switches the board back on from power-off.
//!
//! Register map and the init sequence follow the data sheet and M5Unified's
//! `RX8130_Class`.

use super::i2c::{self, SharedI2c};
use esp_idf_svc::sys::EspError;

pub const ADDR: u8 = 0x32;

// Time and alarm, BCD.
const REG_SEC: u8 = 0x10; // 0x10 sec, 0x11 min, 0x12 hour, 0x13 weekday (bit mask),
// 0x14 day, 0x15 month, 0x16 year (00–99)
const REG_ALARM_MIN: u8 = 0x17; // 0x17 min, 0x18 hour, 0x19 weekday mask / day; bit 7 = AE (ignore)
const REG_EXTENSION: u8 = 0x1C;
const REG_FLAG: u8 = 0x1D;
const REG_CONTROL0: u8 = 0x1E;
const REG_CONTROL1: u8 = 0x1F;
const REG_OFFSET: u8 = 0x30;

// Extension register.
const EXT_WADA: u8 = 1 << 3; // alarm compares the day of the month instead of the weekday
const EXT_TE: u8 = 1 << 4; // fixed-cycle timer enable
// Flag register.
const FLAG_VLF: u8 = 1 << 1; // voltage low: time is invalid
const FLAG_AF: u8 = 1 << 3; // alarm matched
// Control 0.
const CTRL0_AIE: u8 = 1 << 3; // alarm interrupt → nIRQ
const CTRL0_STOP: u8 = 1 << 6;
// Control 1.
const CTRL1_INIEN: u8 = 1 << 4; // nIRQ (alarm, timer, update) output enable
const CTRL1_CHGEN: u8 = 1 << 5; // supercap charging

/// Alarm enable bit in the alarm registers (set = the field is ignored).
const ALARM_AE: u8 = 1 << 7;

/// Cheap to clone (the bus is shared).
#[derive(Clone)]
pub struct Rx8130 {
    bus: SharedI2c,
}

/// UTC date and time (what the chip stores).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday.
    pub weekday: u8,
}

fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn unbcd(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0F)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
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
    /// Field ranges only (the 2-digit year covers 2000–2099).
    pub fn plausible(&self) -> bool {
        (2000..=2099).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

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
            // 1970-01-01 was a Thursday (4).
            weekday: (days + 4).rem_euclid(7) as u8,
        }
    }
}

impl Rx8130 {
    /// Checks that the chip answers and applies the standard configuration:
    /// supercap charging and the nIRQ output on, the fixed-cycle timer off,
    /// no digital offset.
    pub fn new(bus: SharedI2c) -> Result<Self, EspError> {
        let rtc = Self { bus };
        {
            let mut b = i2c::lock(&rtc.bus);
            let ctrl1 = b.read_reg(ADDR, REG_CONTROL1)?;
            b.write_reg(ADDR, REG_CONTROL1, ctrl1 | CTRL1_INIEN | CTRL1_CHGEN)?;
            b.write_reg(ADDR, REG_OFFSET, 0)?;
            // Clock running, all interrupt sources off; the alarm is enabled
            // explicitly by the power-off logic.
            b.write_reg(ADDR, REG_CONTROL0, 0)?;
            let ext = b.read_reg(ADDR, REG_EXTENSION)?;
            b.write_reg(ADDR, REG_EXTENSION, ext & !EXT_TE)?;
        }
        Ok(rtc)
    }

    /// False after a power loss (VLF): the time registers are meaningless
    /// until [`Rx8130::set_time`].
    pub fn time_valid(&self) -> Result<bool, EspError> {
        Ok(i2c::lock(&self.bus).read_reg(ADDR, REG_FLAG)? & FLAG_VLF == 0)
    }

    /// The UTC time, or `None` if the chip lost power since it was set.
    pub fn get_time(&self) -> Result<Option<DateTime>, EspError> {
        let mut b = i2c::lock(&self.bus);
        if b.read_reg(ADDR, REG_FLAG)? & FLAG_VLF != 0 {
            return Ok(None);
        }
        let mut r = [0u8; 7];
        b.write_read(ADDR, &[REG_SEC], &mut r)?;
        let t = DateTime {
            second: unbcd(r[0] & 0x7F),
            minute: unbcd(r[1] & 0x7F),
            hour: unbcd(r[2] & 0x3F),
            weekday: r[3].trailing_zeros().min(6) as u8,
            day: unbcd(r[4] & 0x3F),
            month: unbcd(r[5] & 0x1F),
            year: 2000 + u16::from(unbcd(r[6])),
        };
        // A chip that was never set can hold non-BCD junk with VLF clear.
        let bcd_ok = [0, 1, 2, 4, 5, 6]
            .iter()
            .all(|&i| r[i] & 0x0F <= 9 && (r[i] & 0x7F) >> 4 <= 9);
        if !bcd_ok || !t.plausible() {
            log::warn!("rtc: time registers {r:02x?} are not a valid time");
            return Ok(None);
        }
        Ok(Some(t))
    }

    /// Sets the UTC time and clears the power-loss flag.
    pub fn set_time(&self, t: &DateTime) -> Result<(), EspError> {
        let mut b = i2c::lock(&self.bus);
        let ctrl0 = b.read_reg(ADDR, REG_CONTROL0)?;
        b.write_reg(ADDR, REG_CONTROL0, ctrl0 | CTRL0_STOP)?;
        b.write(
            ADDR,
            &[
                REG_SEC,
                bcd(t.second),
                bcd(t.minute),
                bcd(t.hour),
                1 << t.weekday,
                bcd(t.day),
                bcd(t.month),
                bcd((t.year % 100) as u8),
            ],
        )?;
        b.write_reg(ADDR, REG_CONTROL0, ctrl0 & !CTRL0_STOP)?;
        let flags = b.read_reg(ADDR, REG_FLAG)?;
        b.write_reg(ADDR, REG_FLAG, flags & !FLAG_VLF)?;
        Ok(())
    }

    /// Alarm at `hour:minute` UTC on the weekdays in `weekday_mask` (bit 0 =
    /// Sunday; 0x7F = daily). Enables the interrupt output.
    pub fn set_alarm(&self, hour: u8, minute: u8, weekday_mask: u8) -> Result<(), EspError> {
        let mut b = i2c::lock(&self.bus);
        let ctrl0 = b.read_reg(ADDR, REG_CONTROL0)?;
        b.write_reg(ADDR, REG_CONTROL0, ctrl0 & !CTRL0_AIE)?;
        let ext = b.read_reg(ADDR, REG_EXTENSION)?;
        b.write_reg(ADDR, REG_EXTENSION, ext & !EXT_WADA)?;
        b.write(
            ADDR,
            &[
                REG_ALARM_MIN,
                bcd(minute),
                bcd(hour),
                weekday_mask & !ALARM_AE,
            ],
        )?;
        let flags = b.read_reg(ADDR, REG_FLAG)?;
        b.write_reg(ADDR, REG_FLAG, flags & !FLAG_AF)?;
        b.write_reg(ADDR, REG_CONTROL0, ctrl0 | CTRL0_AIE)
    }

    pub fn alarm_enable(&self, on: bool) -> Result<(), EspError> {
        let mut b = i2c::lock(&self.bus);
        let ctrl0 = b.read_reg(ADDR, REG_CONTROL0)?;
        let v = if on {
            ctrl0 | CTRL0_AIE
        } else {
            ctrl0 & !CTRL0_AIE
        };
        b.write_reg(ADDR, REG_CONTROL0, v)
    }

    /// True if the alarm matched since the flag was last cleared.
    pub fn alarm_flag(&self) -> Result<bool, EspError> {
        Ok(i2c::lock(&self.bus).read_reg(ADDR, REG_FLAG)? & FLAG_AF != 0)
    }

    pub fn alarm_clear(&self) -> Result<(), EspError> {
        let mut b = i2c::lock(&self.bus);
        let flags = b.read_reg(ADDR, REG_FLAG)?;
        b.write_reg(ADDR, REG_FLAG, flags & !FLAG_AF)
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
        let d = DateTime::from_unix(1_794_265_200); // Monday 2026-11-09 23:00 UTC
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.weekday),
            (2026, 11, 9, 23, 1)
        );
    }
}
