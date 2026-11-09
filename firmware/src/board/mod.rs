//! Facts about the Tab5 PCB and the bring-up order.
//!
//! Applications call [`Board::init`] once and get handles back. Required
//! chips panic with their name; the panel variant is detected at run time
//! (three LCD/touch combinations exist across Tab5 production runs).

pub mod i2c;
pub mod ioexp;
pub mod sd;
pub mod touch;

use crate::display::{Display, PanelVariant};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::ledc::{LedcDriver, LedcTimerDriver, Resolution, config::TimerConfig};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use i2c::{SharedI2c, SysI2c};
use ioexp::IoExpanders;

/// Touch controller addresses, used to tell the panel variants apart.
const ST712X_TOUCH_ADDR: u8 = 0x55;
const GT911_ADDR_PRIMARY: u8 = 0x14; // INT pulled high during reset (Tab5 default)
const GT911_ADDR_BACKUP: u8 = 0x5D;

pub struct Board {
    pub i2c: SharedI2c,
    pub ioexp: IoExpanders,
    pub display: Display,
    pub backlight: Backlight,
    /// The microSD card at `/sdcard`, if one is inserted and readable.
    pub sd: Option<sd::SdCard>,
}

impl Board {
    pub fn init() -> anyhow::Result<Self> {
        let p = Peripherals::take()?;

        // 1. I2C with bus recovery.
        let i2c: SharedI2c = std::sync::Arc::new(std::sync::Mutex::new(SysI2c::new(
            p.i2c0,
            p.pins.gpio31.degrade_input_output(),
            p.pins.gpio32.degrade_input_output(),
        )?));

        // 2. Expanders: rails off, LCD and touch in reset. Then release the
        //    resets after the 10 ms minimum pulse width.
        let ioexp = IoExpanders::new(i2c.clone())?;
        FreeRtos::delay_ms(10);
        ioexp.release_display_resets()?;

        // 3. Backlight PWM, dark until the first frame is on the panel.
        let mut backlight = Backlight::new(p.ledc.timer0, p.ledc.channel0, p.pins.gpio22)?;
        backlight.set_percent(0)?;

        // 4. Panel variant from the touch controller. The ST712x touch
        //    controller needs a moment after reset before it answers.
        FreeRtos::delay_ms(300);
        let variant = detect_panel(&mut i2c::lock(&i2c));
        log::info!("board: panel variant {variant:?}");

        // 5. Display: DSI PHY power, panel init, video mode streaming.
        let display = Display::new(p.dsi, variant)?;

        // 6. microSD (optional).
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
            display,
            backlight,
            sd,
        })
    }
}

fn detect_panel(i2c: &mut SysI2c) -> PanelVariant {
    if i2c.probe(ST712X_TOUCH_ADDR) {
        // Firmware version register 0x0000 (16-bit register address).
        let mut fw = [0u8];
        match i2c.write_read(ST712X_TOUCH_ADDR, &[0x00, 0x00], &mut fw) {
            Ok(()) if fw[0] == 1 => return PanelVariant::St7121,
            Ok(()) if fw[0] == 3 => return PanelVariant::St7123,
            Ok(()) => log::warn!(
                "board: unknown ST712x touch firmware {}, assuming ST7123",
                fw[0]
            ),
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

/// LCD backlight: GPIO22 drives the enable of the ME2212 boost converter.
/// 44.1 kHz PWM as in M5GFX (Espressif's BSP uses 5 kHz).
pub struct Backlight {
    channel: LedcDriver<'static>,
}

impl Backlight {
    fn new(
        timer: impl esp_idf_svc::hal::ledc::LedcTimer<SpeedMode = esp_idf_svc::hal::ledc::LowSpeed>
        + 'static,
        channel: impl esp_idf_svc::hal::ledc::LedcChannel<SpeedMode = esp_idf_svc::hal::ledc::LowSpeed>
        + 'static,
        pin: impl esp_idf_svc::hal::gpio::OutputPin + 'static,
    ) -> anyhow::Result<Self> {
        let timer = LedcTimerDriver::new(
            timer,
            &TimerConfig::new()
                .frequency(Hertz(44_100))
                .resolution(Resolution::Bits10),
        )?;
        let channel = LedcDriver::new(channel, timer, pin)?;
        Ok(Self { channel })
    }

    pub fn set_percent(&mut self, percent: u8) -> anyhow::Result<()> {
        let max = self.channel.get_max_duty();
        self.channel
            .set_duty(max * u32::from(percent.min(100)) / 100)?;
        Ok(())
    }
}
