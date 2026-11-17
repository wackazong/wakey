//! LCD backlight: GPIO22 drives the enable of the ME2212 boost converter.

use esp_idf_svc::hal::gpio::OutputPin;
use esp_idf_svc::hal::ledc::{
    LedcChannel, LedcDriver, LedcTimer, LedcTimerDriver, LowSpeed, Resolution, config::TimerConfig,
};
use esp_idf_svc::hal::units::Hertz;

/// PWM frequency. M5GFX uses 44.1 kHz, which leaves 10 bits of duty: the
/// visible range (0–2 %, see `ambient`) then has ~20 steps, and fades
/// jump. At 4 kHz the duty has 14 bits (~330 steps there); 5 kHz looks
/// the same as 44.1 kHz (no flicker). Espressif's BSP uses 5 kHz.
const PWM_HZ: u32 = 4_000;

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
                .resolution(Resolution::Bits14),
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
