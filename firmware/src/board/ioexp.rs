//! Two PI4IOE5V6408 8-bit IO expanders on the system I2C bus.
//!
//! Pin meanings from the Tab5 schematic; power-on values from M5Unified
//! (`Power_Class.inl`), see the hardware tables in the architecture doc.

use super::i2c::{self, SharedI2c, SysI2c};
use esp_idf_svc::sys::EspError;
use std::sync::{Arc, Mutex};

pub const IOE_A: u8 = 0x43;
pub const IOE_B: u8 = 0x44;

mod reg {
    pub const DIRECTION: u8 = 0x03; // 1 = output
    pub const OUTPUT: u8 = 0x05;
    pub const OUTPUT_HIGH_Z: u8 = 0x07; // 1 = high impedance
    pub const PULL_ENABLE: u8 = 0x0B;
    pub const PULL_SELECT: u8 = 0x0D; // 1 = pull-up
    pub const INPUT: u8 = 0x0F;
}

/// Expander pins the firmware drives or reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// A.P1: NS4150B speaker amplifier enable.
    SpkEn,
    /// A.P2: 5 V on M5-Bus/Grove. Keep off.
    Ext5vEn,
    /// A.P4: LCD reset, active low. Released = input with pull-up.
    LcdRst,
    /// A.P5: touch controller reset, active low.
    TpRst,
    /// A.P6: camera reset, active low.
    CamRst,
    /// A.P7 (input): headphone plugged in.
    HpDet,
    /// B.P0: 3.3 V for the ESP32-C6 (Wi-Fi).
    WlanPwrEn,
    /// B.P3: 5 V on the USB-A port (powers the speaker).
    Usb5vEn,
    /// B.P4: power-off pulse to the PMS150.
    PwroffPulse,
    /// B.P5: fast charge (QC) disable, active high: 0 = fast charge allowed.
    NChgQcEn,
    /// B.P6 (input): charging, active low.
    ChgStat,
    /// B.P7: allow battery charging.
    ChgEn,
}

impl Pin {
    const fn location(self) -> (u8, u8) {
        match self {
            Pin::SpkEn => (IOE_A, 1),
            Pin::Ext5vEn => (IOE_A, 2),
            Pin::LcdRst => (IOE_A, 4),
            Pin::TpRst => (IOE_A, 5),
            Pin::CamRst => (IOE_A, 6),
            Pin::HpDet => (IOE_A, 7),
            Pin::WlanPwrEn => (IOE_B, 0),
            Pin::Usb5vEn => (IOE_B, 3),
            Pin::PwroffPulse => (IOE_B, 4),
            Pin::NChgQcEn => (IOE_B, 5),
            Pin::ChgStat => (IOE_B, 6),
            Pin::ChgEn => (IOE_B, 7),
        }
    }
}

/// Register image of one expander. The firmware is the only writer, so the
/// cached copies stay in sync and every change is a single register write.
#[derive(Debug, Clone, Copy)]
struct Image {
    dir: u8,
    out: u8,
    high_z: u8,
    pull_en: u8,
    pull_sel: u8,
}

/// Both expanders. Cheap to clone; clones share the register images and
/// the bus.
#[derive(Clone)]
pub struct IoExpanders {
    bus: SharedI2c,
    images: Arc<Mutex<(Image, Image)>>,
}

impl IoExpanders {
    /// Programs both expanders. Panics name the chip if one is missing.
    ///
    /// Starting state differs from M5Unified in two places: the LCD and the
    /// touch controller are held in reset (released by
    /// [`IoExpanders::release_display_resets`]) and Wi-Fi power is off.
    pub fn new(bus: SharedI2c) -> Result<Self, EspError> {
        for addr in [IOE_A, IOE_B] {
            assert!(
                i2c::lock(&bus).probe(addr),
                "IO expander 0x{addr:02x} does not answer"
            );
        }

        // A: P0 antenna select (0 = internal), P1 speaker amp off,
        //    P2 EXT5V off, P4 LCD_RST low, P5 TP_RST low, P6 CAM_RST high.
        let a = Image {
            dir: 0b0111_0111,
            out: 0b0100_0000,
            high_z: 0b0000_1000,
            pull_en: 0b0001_0100,
            pull_sel: 0b0001_0100,
        };
        // B: P0 WLAN power off, P3 USB-A 5 V off, P4 power-off pulse idle low,
        //    P5 fast-charge default, P7 charging allowed.
        let b = Image {
            dir: 0b1011_1001,
            out: 0b1000_0000,
            high_z: 0b0000_0110,
            pull_en: 0b0000_1000,
            pull_sel: 0b0000_1000,
        };
        let this = Self {
            bus,
            images: Arc::new(Mutex::new((a, b))),
        };
        this.with(IOE_A, Self::write_image)?;
        this.with(IOE_B, Self::write_image)?;
        Ok(this)
    }

    /// Runs `f` with the bus and the register image of expander `addr`.
    fn with<R>(&self, addr: u8, f: impl FnOnce(&mut SysI2c, u8, &mut Image) -> R) -> R {
        let mut images = self.images.lock().unwrap_or_else(|e| e.into_inner());
        let img = if addr == IOE_A {
            &mut images.0
        } else {
            &mut images.1
        };
        f(&mut i2c::lock(&self.bus), addr, img)
    }

    fn write_image(bus: &mut SysI2c, addr: u8, img: &mut Image) -> Result<(), EspError> {
        // Levels before directions, so outputs start at a defined level.
        bus.write_reg(addr, reg::OUTPUT, img.out)?;
        bus.write_reg(addr, reg::OUTPUT_HIGH_Z, img.high_z)?;
        bus.write_reg(addr, reg::PULL_SELECT, img.pull_sel)?;
        bus.write_reg(addr, reg::PULL_ENABLE, img.pull_en)?;
        bus.write_reg(addr, reg::DIRECTION, img.dir)
    }

    /// Drives an output pin.
    pub fn set(&self, pin: Pin, level: bool) -> Result<(), EspError> {
        let (addr, bit) = pin.location();
        self.with(addr, |bus, addr, img| {
            if level {
                img.out |= 1 << bit;
            } else {
                img.out &= !(1 << bit);
            }
            bus.write_reg(addr, reg::OUTPUT, img.out)
        })
    }

    /// Makes a pin an output (`true`) or an input (`false`, pull as configured).
    pub fn set_output(&self, pin: Pin, output: bool) -> Result<(), EspError> {
        let (addr, bit) = pin.location();
        self.with(addr, |bus, addr, img| {
            if output {
                img.dir |= 1 << bit;
            } else {
                img.dir &= !(1 << bit);
            }
            bus.write_reg(addr, reg::DIRECTION, img.dir)
        })
    }

    /// Reads the input level of a pin.
    pub fn get(&self, pin: Pin) -> Result<bool, EspError> {
        let (addr, bit) = pin.location();
        self.with(addr, |bus, addr, _| {
            Ok(bus.read_reg(addr, reg::INPUT)? & (1 << bit) != 0)
        })
    }

    /// Releases the LCD reset (input, pulled up, as the BSP does) and then
    /// the touch reset. Wait about 120 ms before talking to the panel.
    pub fn release_display_resets(&self) -> Result<(), EspError> {
        self.set_output(Pin::LcdRst, false)?;
        self.set(Pin::TpRst, true)
    }
}
