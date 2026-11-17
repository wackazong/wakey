//! The ES8388 audio codec (I2C 0x10) driving the built-in speaker through
//! the NS4150B amplifier (`SPK_EN`, expander A P1). Audio comes over I2S0:
//! MCLK GPIO30, BCLK GPIO27, LRCK GPIO29, data to the codec GPIO26.
//!
//! Register values from M5Unified's Tab5 speaker setup (DAC only, I2S
//! slave, 16-bit Philips, ADC powered down), with two changes after
//! Espressif's ES8388 driver (esp-adf `es8388.c`): register 24 says MCLK =
//! 256 × sample rate, which is what the I2S driver sends (M5Unified's table
//! says 128), and the init ends by restarting the codec's state machine.

use super::i2c::{self, SharedI2c};
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::i2s::I2S0;
use esp_idf_svc::sys::EspError;

const ADDR: u8 = 0x10;

/// The I2S peripheral and pins wired to the codec.
pub struct CodecPins {
    pub i2s: I2S0<'static>,
    pub mclk: AnyIOPin<'static>,
    pub bclk: AnyIOPin<'static>,
    pub ws: AnyIOPin<'static>,
    pub dout: AnyIOPin<'static>,
}

/// Register, value; written in this order.
const INIT: [(u8, u8); 31] = [
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
    (24, 0x02), // MCLK / LRCK = 256 (single speed)
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
    (46, 0x21), // LOUT1 +4.5 dB (the amplifier takes LOUT1 only)
    (47, 0x21), // ROUT1
    (48, 0x21), // LOUT2
    (49, 0x21), // ROUT2
    (25, 0x20),
    // Restart the state machine, then power up (ADC parts stay down). The
    // codec stays powered while the P4 restarts (a flash) and loses its
    // clock mid-stream; without the restart it then stays silent until the
    // board is power-cycled.
    (2, 0xF0),
    (2, 0x0A),
];

/// Powers the codec down (DAC, then the chip): ~9 mA less at 5 V. [`init`]
/// powers it up again (MCLK running first).
pub fn power_down(bus: &SharedI2c) -> Result<(), EspError> {
    let mut b = i2c::lock(bus);
    b.write_reg(ADDR, 4, 0xC0)?;
    b.write_reg(ADDR, 2, 0xFF)
}

/// Powers the DAC path up. MCLK must be running.
pub fn init(bus: &SharedI2c) -> Result<(), EspError> {
    let mut b = i2c::lock(bus);
    for (reg, value) in INIT {
        b.write_reg(ADDR, reg, value)?;
    }
    Ok(())
}
