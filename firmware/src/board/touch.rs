//! ST7123 touch controller (the touch half of the ST7123 TDDI panel), I2C
//! 0x55, 16-bit register addresses. Ported from Espressif's
//! `esp_lcd_touch_st7123` component.
//!
//! Only the ST7123/ST7121 panel variants are supported; the older
//! ILI9881C variant has a GT911 (not implemented yet).

use super::i2c::SysI2c;
use esp_idf_svc::sys::EspError;

pub const ADDR: u8 = 0x55;

const REG_ADV_INFO: u16 = 0x0010;
const REG_MAX_TOUCHES: u16 = 0x0009;
const REG_REPORT: u16 = 0x0014;
/// Bytes per report: x_h (bits 0-5, valid = bit 7), x_l, y_h, y_l, area,
/// intensity, reserved.
const REPORT_LEN: usize = 7;
const MAX_POINTS: usize = 10;

fn read(i2c: &mut SysI2c, reg: u16, buf: &mut [u8]) -> Result<(), EspError> {
    i2c.write_read(ADDR, &reg.to_be_bytes(), buf)
}

/// First touch point in panel coordinates (portrait, x 0..720, y 0..1280),
/// or `None` when nothing touches the screen.
pub fn read_point(i2c: &mut SysI2c) -> Result<Option<(u16, u16)>, EspError> {
    let mut adv = [0u8];
    read(i2c, REG_ADV_INFO, &mut adv)?;
    if adv[0] & 0x08 == 0 {
        // No coordinates reported.
        return Ok(None);
    }
    let mut max = [0u8];
    read(i2c, REG_MAX_TOUCHES, &mut max)?;
    let n = (max[0] as usize).min(MAX_POINTS);
    if n == 0 {
        return Ok(None);
    }
    let mut reports = [0u8; REPORT_LEN * MAX_POINTS];
    read(i2c, REG_REPORT, &mut reports[..n * REPORT_LEN])?;
    Ok(reports[..n * REPORT_LEN]
        .as_chunks::<REPORT_LEN>()
        .0
        .iter()
        .find(|r| r[0] & 0x80 != 0)
        .map(|r| {
            let x = u16::from(r[0] & 0x3F) << 8 | u16::from(r[1]);
            let y = u16::from(r[2]) << 8 | u16::from(r[3]);
            (x, y)
        }))
}
