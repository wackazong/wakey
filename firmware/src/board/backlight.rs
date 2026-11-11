//! LCD backlight: GPIO22 drives the enable of the ME2212 boost converter.

use esp_idf_svc::hal::gpio::OutputPin;
use esp_idf_svc::hal::ledc::{
    LedcChannel, LedcDriver, LedcTimer, LedcTimerDriver, LowSpeed, Resolution, config::TimerConfig,
};
use esp_idf_svc::hal::units::Hertz;

/// PWM frequency from M5GFX, adopted together with its panel timings when
/// those fixed the flicker (Espressif's BSP uses 5 kHz).
const PWM_HZ: u32 = 44_100;

pub struct Backlight {
    channel: LedcDriver<'static>,
}

impl Backlight {
    pub(super) fn new(
        timer: impl LedcTimer<SpeedMode = LowSpeed> + 'static,
        channel: impl LedcChannel<SpeedMode = LowSpeed> + 'static,
        pin: impl OutputPin + 'static,
    ) -> anyhow::Result<Self> {
        let timer = LedcTimerDriver::new(
            timer,
            &TimerConfig::new()
                .frequency(Hertz(PWM_HZ))
                .resolution(Resolution::Bits10),
        )?;
        let channel = LedcDriver::new(channel, timer, pin)?;
        Ok(Self { channel })
    }

    /// 0 = dark, 100 = full brightness.
    pub fn set_percent(&mut self, percent: u8) -> anyhow::Result<()> {
        let max = self.channel.get_max_duty();
        self.channel
            .set_duty(max * u32::from(percent.min(100)) / 100)?;
        Ok(())
    }

    /// 0.0 = dark, 1.0 = full brightness, in the PWM's full resolution
    /// (fine steps at the dark end).
    pub fn set_level(&mut self, level: f32) -> anyhow::Result<()> {
        let max = self.channel.get_max_duty();
        self.channel
            .set_duty((max as f32 * level.clamp(0.0, 1.0)).round() as u32)?;
        Ok(())
    }
}
