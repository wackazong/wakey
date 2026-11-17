//! The Tab5 PCB: pins, chips on the system I2C bus, power switches, and
//! the bring-up order.
//!
//! Applications call [`Board::init`] once and get owned handles back.
//! The IO expanders, the RTC and the touch controller panic with their
//! name when they do not answer; other failures come back as the error of
//! [`Board::init`]. The microSD card is optional. The
//! panel variant is detected at run time (three LCD/touch combinations
//! exist across Tab5 production runs).

mod backlight;
pub mod camera;
pub mod codec;
pub mod i2c;
pub mod ina226;
pub mod ioexp;
pub mod rtc;
pub mod sd;
pub mod touch;

pub use backlight::Backlight;

use crate::display::{Display, PanelVariant};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::Gpio14;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::UART1;
use i2c::{SharedI2c, SysI2c};
use ioexp::IoExpanders;
use std::sync::{Arc, Mutex};

/// GT911 touch controller addresses (the ILI9881C variant); the ST712x
/// variants answer at [`touch::ADDR`].
const GT911_ADDR_PRIMARY: u8 = 0x14; // INT pulled high during reset (Tab5 default)
const GT911_ADDR_BACKUP: u8 = 0x5D;

/// The UART and the pin of the C6's one-way link to the P4 (`RF_C6_IO2`).
pub struct C6Link {
    pub uart: UART1<'static>,
    pub rx: Gpio14<'static>,
}

pub struct Board {
    /// The system I2C bus (touch polling uses it directly).
    pub i2c: SharedI2c,
    /// Power switches and resets on the two IO expanders.
    pub ioexp: IoExpanders,
    /// The RX8130 real-time clock (UTC).
    pub rtc: rtc::Rx8130,
    pub display: Display,
    pub backlight: Backlight,
    /// The microSD card at [`sd::MOUNT_POINT`], if one is inserted and
    /// readable. Unmounted when dropped.
    pub sd: Option<sd::SdCard>,
    /// Wi-Fi through the ESP32-C6, for [`crate::net::start`].
    pub modem: Modem<'static>,
    /// The C6's IO2 → GPIO14, read by UART1: the AirDrop receiver's frames.
    pub c6_link: C6Link,
    /// I2S to the ES8388 codec (built-in speaker).
    pub codec: codec::CodecPins,
}

impl Board {
    pub fn init() -> anyhow::Result<Self> {
        let p = Peripherals::take()?;

        // 1. I2C with bus recovery.
        let i2c: SharedI2c = Arc::new(Mutex::new(SysI2c::new(
            p.i2c0,
            p.pins.gpio31.degrade_input_output(),
            p.pins.gpio32.degrade_input_output(),
        )?));

        // 2. Expanders: rails off, LCD and touch in reset. Then release the
        //    resets after the 10 ms minimum pulse width.
        let ioexp = IoExpanders::new(i2c.clone())?;
        FreeRtos::delay_ms(10);
        ioexp.release_display_resets()?;

        // 3. RTC: supercap charging on, interrupt sources off.
        let rtc = rtc::Rx8130::new(i2c.clone())
            .unwrap_or_else(|e| panic!("RX8130 RTC at {:#04x} does not answer: {e}", rtc::ADDR));

        // 4. Backlight PWM, dark until the first frame is on the panel.
        let mut backlight = Backlight::new(p.ledc.timer0, p.ledc.channel0, p.pins.gpio22)?;
        backlight.set_percent(0)?;

        // 5. Panel variant from the touch controller. The ST712x touch
        //    controller needs a moment after reset before it answers.
        FreeRtos::delay_ms(300);
        let variant = detect_panel(&mut i2c::lock(&i2c));
        log::info!("board: panel variant {variant:?}");

        // 6. Display: DSI PHY power, panel init, video mode streaming.
        let display = Display::new(p.dsi, variant)?;

        // 7. microSD (optional).
        let sd = match sd::mount(
            p.sdmmc0,
            sd::SdPins {
                clk: p.pins.gpio43,
                cmd: p.pins.gpio44,
                d0: p.pins.gpio39,
                d1: p.pins.gpio40,
                d2: p.pins.gpio41,
                d3: p.pins.gpio42,
            },
        ) {
            Ok(card) => {
                log::info!("board: microSD mounted at {}", sd::MOUNT_POINT);
                Some(card)
            }
            Err(e) => {
                log::warn!("board: no microSD ({e})");
                None
            }
        };

        Ok(Self {
            i2c,
            ioexp,
            rtc,
            display,
            backlight,
            sd,
            modem: p.modem,
            c6_link: C6Link {
                uart: p.uart1,
                rx: p.pins.gpio14,
            },
            codec: codec::CodecPins {
                i2s: p.i2s0,
                mclk: p.pins.gpio30.degrade_input_output(),
                bclk: p.pins.gpio27.degrade_input_output(),
                ws: p.pins.gpio29.degrade_input_output(),
                dout: p.pins.gpio26.degrade_input_output(),
            },
        })
    }
}

fn detect_panel(i2c: &mut SysI2c) -> PanelVariant {
    if i2c.probe(touch::ADDR) {
        match touch::firmware(i2c) {
            Ok(1) => return PanelVariant::St7121,
            Ok(3) => return PanelVariant::St7123,
            Ok(fw) => log::warn!("board: unknown ST712x touch firmware {fw}, assuming ST7123"),
            Err(e) => log::warn!("board: ST712x touch firmware read failed ({e}), assuming ST7123"),
        }
        return PanelVariant::St7123;
    }
    if i2c.probe(GT911_ADDR_PRIMARY) || i2c.probe(GT911_ADDR_BACKUP) {
        return PanelVariant::Ili9881c;
    }
    panic!(
        "touch controller (ST712x 0x55 / GT911 0x14/0x5D) does not answer: unknown panel variant"
    );
}
