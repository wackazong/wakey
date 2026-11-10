//! The ES8388 audio codec (I2C 0x10) driving the built-in speaker through
//! the NS4150B amplifier (`SPK_EN`, expander A P1). Audio comes over I2S0:
//! MCLK GPIO30, BCLK GPIO27, LRCK GPIO29, data to the codec GPIO26.
//!
//! Register values from M5Unified's Tab5 speaker setup (DAC only, I2S
//! slave, 16-bit Philips, MCLK = 128 × sample rate, ADC powered down).

use super::i2c::{self, SharedI2c};
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2s::I2S0;
use esp_idf_svc::sys::EspError;

pub const ADDR: u8 = 0x10;

/// The I2S peripheral and pins wired to the codec.
pub struct CodecPins {
    pub i2s: I2S0<'static>,
    pub mclk: AnyIOPin<'static>,
    pub bclk: AnyIOPin<'static>,
    pub ws: AnyIOPin<'static>,
    pub dout: AnyIOPin<'static>,
}

/// Register, value; written in this order.
const INIT: [(u8, u8); 30] = [
    (0, 0x80), // reset, CSM power on
    (0, 0x00),
    (0, 0x00),
    (0, 0x0E),
    (1, 0x00),
    (2, 0x0A), // chip power: all up
    (3, 0xFF), // ADC power: all down
    (4, 0x3C), // DAC power: up, LOUT1/ROUT1/LOUT2/ROUT2 on
    (5, 0x00),
    (6, 0x00),
    (7, 0x7C),
    (8, 0x00),  // I2S slave
    (23, 0x18), // I2S, 16 bit
    (24, 0x00), // MCLK / LRCK = 128
    (25, 0x20), // DAC unmuted
    (26, 0x00), // left DAC volume 0 dB
    (27, 0x00), // right DAC volume 0 dB
    (28, 0x08), // click-free power up and down
    (29, 0x00),
    (38, 0x00),
    (39, 0xB8), // left mixer: DAC
    (42, 0xB8), // right mixer: DAC
    (43, 0x08),
    (45, 0x00),
    (46, 0x1E), // LOUT1 0 dB
    (47, 0x1E), // ROUT1 0 dB
    (48, 0x1E), // LOUT2 0 dB
    (49, 0x1E), // ROUT2 0 dB
    (25, 0x20),
    (2, 0x0A),
];

/// Powers the DAC path up. MCLK must be running.
pub fn init(bus: &SharedI2c) -> Result<(), EspError> {
    let mut b = i2c::lock(bus);
    for (reg, value) in INIT {
        b.write(ADDR, &[reg, value])?;
    }
    Ok(())
}
