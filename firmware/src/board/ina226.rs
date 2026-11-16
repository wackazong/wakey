//! The INA226 on the battery (0x41, 5 mΩ shunt): whether the clock runs
//! from its battery. On USB power the system's current does not pass the
//! shunt (it reads ~0); on battery all of it does (hundreds of mA).

use super::i2c::{SharedI2c, lock};

const ADDR: u8 = 0x41;
const SHUNT_VOLTAGE: u8 = 0x01;
/// Battery current above this (either direction) means on battery.
const ON_BATTERY_MA: f32 = 30.0;

/// Battery current in mA (2.5 µV per LSB over 5 mΩ), if the chip answers.
pub fn current_ma(bus: &SharedI2c) -> Option<f32> {
    let mut b = [0u8; 2];
    lock(bus).write_read(ADDR, &[SHUNT_VOLTAGE], &mut b).ok()?;
    Some(f32::from(i16::from_be_bytes(b)) * 0.5)
}

/// True if the clock runs from its battery.
pub fn on_battery(bus: &SharedI2c) -> bool {
    current_ma(bus).is_some_and(|ma| ma.abs() > ON_BATTERY_MA)
}
