//! Epson RX8130CE real-time clock at I2C 0x32, backed by a 70 mF supercap
//! (≈ 7 h without battery). Time is kept in UTC; the time zone is applied
//! by the C library (see `crate::clock`). [`DateTime`] fields are UTC here.
//!
//! Its interrupt output is not wired to the P4 but to the PMS150 power
//! controller (its alarm could switch the board on from power-off; unused).
//!
//! Register map and the init sequence follow the data sheet and M5Unified's
//! `RX8130_Class`.

use super::i2c::{self, SharedI2c};
use esp_idf_svc::sys::EspError;
pub use wakey_core::datetime::DateTime;

pub const ADDR: u8 = 0x32;

// Time, BCD: 0x10 sec, 0x11 min, 0x12 hour, 0x13 weekday (bit mask),
// 0x14 day, 0x15 month, 0x16 year (00–99).
const REG_SEC: u8 = 0x10;
const REG_EXTENSION: u8 = 0x1C;
const REG_FLAG: u8 = 0x1D;
const REG_CONTROL0: u8 = 0x1E;
const REG_CONTROL1: u8 = 0x1F;
const REG_OFFSET: u8 = 0x30;

// Extension register.
const EXT_TE: u8 = 1 << 4; // fixed-cycle timer enable
// Flag register.
const FLAG_VLF: u8 = 1 << 1; // voltage low: time is invalid
// Control 0.
const CTRL0_STOP: u8 = 1 << 6;
// Control 1.
const CTRL1_INIEN: u8 = 1 << 4; // nIRQ (alarm, timer, update) output enable
const CTRL1_CHGEN: u8 = 1 << 5; // supercap charging

/// Cheap to clone (the bus is shared).
#[derive(Clone)]
pub struct Rx8130 {
    bus: SharedI2c,
}

fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn unbcd(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0F)
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
            // Clock running, all interrupt sources off.
            b.write_reg(ADDR, REG_CONTROL0, 0)?;
            let ext = b.read_reg(ADDR, REG_EXTENSION)?;
            b.write_reg(ADDR, REG_EXTENSION, ext & !EXT_TE)?;
        }
        Ok(rtc)
    }

    /// The UTC time, or `None` if the chip lost power since it was set
    /// (VLF) or holds no valid time.
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
        // Low digits are checked here (the weekday is a bit mask); a high
        // digit above 9 gives a value `plausible` rejects.
        let bcd_ok = [0, 1, 2, 4, 5, 6].iter().all(|&i| r[i] & 0x0F <= 9);
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
}
