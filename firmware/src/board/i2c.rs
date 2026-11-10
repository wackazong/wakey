//! The system I2C bus (GPIO31 SDA / GPIO32 SCL, 400 kHz).
//!
//! Expanders, RTC, INA226, touch, codecs and IMU all share it. The firmware
//! owns the only driver on these pins; chip drivers and tasks hold a
//! [`SharedI2c`] and lock it per transaction.

use esp_idf_svc::hal::delay::{Ets, TickType};
use esp_idf_svc::hal::gpio::{AnyIOPin, PinDriver, Pull};
use esp_idf_svc::hal::i2c::{I2c, I2cConfig, I2cDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(50);

/// The bus, shared between chip drivers and tasks.
pub type SharedI2c = std::sync::Arc<std::sync::Mutex<SysI2c>>;

pub struct SysI2c {
    driver: I2cDriver<'static>,
}

/// Locks a [`SharedI2c`] (a poisoned lock is still a usable bus).
pub fn lock(bus: &SharedI2c) -> std::sync::MutexGuard<'_, SysI2c> {
    bus.lock().unwrap_or_else(|e| e.into_inner())
}

impl SysI2c {
    /// Frees the bus, then installs the driver.
    pub fn new(
        i2c: impl I2c + 'static,
        mut sda: AnyIOPin<'static>,
        mut scl: AnyIOPin<'static>,
    ) -> Result<Self, EspError> {
        // SAFETY: the reborrowed pin drivers are dropped before the I2C
        // driver takes the pins.
        unsafe { recover(sda.reborrow(), scl.reborrow())? };

        // The hardware gives up when a device holds SCL low (clock
        // stretching) longer than this. The driver's default is 2^10 clock
        // cycles, ~26 µs: the touch controller stretches longer while it
        // processes a touch, and every such read timed out (touch lagged,
        // then stopped). 10 ms rounds up to 2^19 cycles, ~13 ms.
        let config = I2cConfig::new()
            .timeout(Duration::from_millis(10).into())
            .baudrate(Hertz(400_000))
            .sda_enable_pullup(true)
            .scl_enable_pullup(true);
        let driver = I2cDriver::new(i2c, sda, scl, &config)?;
        Ok(Self { driver })
    }

    fn ticks() -> u32 {
        TickType::from(TIMEOUT).ticks()
    }

    /// True if a device acknowledges `addr`.
    pub fn probe(&mut self, addr: u8) -> bool {
        self.driver.write(addr, &[], Self::ticks()).is_ok()
    }

    pub fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), EspError> {
        self.driver.write(addr, bytes, Self::ticks())
    }

    pub fn write_read(&mut self, addr: u8, out: &[u8], buf: &mut [u8]) -> Result<(), EspError> {
        self.driver.write_read(addr, out, buf, Self::ticks())
    }

    /// Writes one 8-bit register.
    pub fn write_reg(&mut self, addr: u8, reg: u8, value: u8) -> Result<(), EspError> {
        self.write(addr, &[reg, value])
    }

    /// Reads one 8-bit register.
    pub fn read_reg(&mut self, addr: u8, reg: u8) -> Result<u8, EspError> {
        let mut b = [0u8];
        self.write_read(addr, &[reg], &mut b)?;
        Ok(b[0])
    }
}

/// Manual bus recovery: a reset in the middle of a read can leave a slave
/// holding SDA low. Nine SCL pulses let it finish its byte, then a STOP
/// condition releases the bus. The RTC and the expanders stay powered across
/// resets, so this runs on every boot.
fn recover(sda: AnyIOPin<'_>, scl: AnyIOPin<'_>) -> Result<(), EspError> {
    let mut sda = PinDriver::input_output_od(sda, Pull::Up)?;
    let mut scl = PinDriver::input_output_od(scl, Pull::Up)?;
    sda.set_high()?;
    scl.set_high()?;
    Ets::delay_us(5);

    if sda.is_high() {
        return Ok(());
    }
    log::warn!("i2c: SDA held low, clocking the bus free");
    for _ in 0..9 {
        scl.set_low()?;
        Ets::delay_us(5);
        scl.set_high()?;
        Ets::delay_us(5);
        if sda.is_high() {
            break;
        }
    }
    // STOP: SDA low → high while SCL is high.
    sda.set_low()?;
    Ets::delay_us(5);
    scl.set_high()?;
    Ets::delay_us(5);
    sda.set_high()?;
    Ets::delay_us(5);
    Ok(())
}
