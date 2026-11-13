//! When the display sleeps (`power` in `config.json`): after
//! `sleep_after_s` without a touch, on the clock page, not while an alarm
//! rings, and only when `sleep_when` allows it: `"dark"` in a dark room
//! (the light meter's night mode) or on battery, `"always"`, `"never"`.
//! It wakes on a touch (`ui::power`), when an alarm rings, and when the
//! condition ends (the room gets light on USB power).
//!
//! The USB-A port is on only when the speaker may be needed (the Pebble
//! draws ~80 mA even when silent): from [`USB_LEAD_S`] before something
//! rings (it needs a few seconds to boot and enumerate) and on the alarm
//! pages (sound previews). Not on the clock page, awake or not.

use super::alarm::{AlarmClock, View};
use super::screens::AppWindow;
use super::{PAGE_CLOCK, show_page};
use crate::audio::uac::UsbAudio;
use crate::board::i2c::SharedI2c;
use crate::board::ina226;
use crate::ui::{input, power};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wakey_core::config::{PowerConfig, SleepWhen};

/// USB-A on this long before an alarm rings (or a snooze ends).
const USB_LEAD_S: i64 = 60;

/// Checks twice a second.
pub fn start(
    window: &AppWindow,
    cfg: PowerConfig,
    alarm: Rc<RefCell<AlarmClock>>,
    bus: SharedI2c,
    usb: UsbAudio,
) -> Timer {
    let weak = window.as_weak();
    // Feature `usb-cycle-test`: USB-A on and off at random 50–3000 ms, so
    // power changes hit device enumeration and port resets (the hub
    // driver's abort, docs/pitfalls.md); the normal USB-A rule is off.
    #[cfg(feature = "usb-cycle-test")]
    {
        let usb = usb.clone();
        std::thread::Builder::new()
            .name("usb-cycle".into())
            .stack_size(4096)
            .spawn(move || {
                let mut x: u32 = 0x1234_5678;
                let mut on = true;
                let mut n = 0u32;
                loop {
                    n += 1;
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    let ms = 50 + x % 2950;
                    std::thread::sleep(Duration::from_millis(u64::from(ms)));
                    on = !on;
                    log::info!("usb-cycle-test: {n}: USB-A {} after {ms} ms", if on { "on" } else { "off" });
                    usb.set_power(on);
                }
            })
            .expect("usb-cycle thread");
    }
    // The battery reading is an I2C transfer: every 5 s is plenty.
    let mut ticks = 0u32;
    let mut on_battery = false;
    let mut usb_on = true;
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        let Some(window) = weak.upgrade() else { return };
        if ticks.is_multiple_of(10) {
            let now = ina226::on_battery(&bus);
            if now != on_battery {
                log::info!("power: on {}", if now { "battery" } else { "USB power" });
                on_battery = now;
            }
        }
        ticks = ticks.wrapping_add(1);
        let allowed = match cfg.sleep_when {
            SleepWhen::Never => false,
            SleepWhen::Always => true,
            SleepWhen::Dark | SleepWhen::Unknown => crate::ambient::night() || on_battery,
        };
        let ringing = matches!(alarm.borrow().view(), View::Ringing { .. });
        if cfg!(feature = "sleep-cycle-test") {
            if ticks.is_multiple_of(16) {
                if power::asleep() {
                    power::wake()
                } else {
                    power::sleep()
                }
            }
            return;
        }
        #[cfg(feature = "debug-overlay")]
        window.set_debug_text(debug_text(&cfg, allowed, ringing, window.get_page()).into());
        let soon = alarm
            .borrow()
            .secs_to_ring()
            .is_some_and(|s| s <= USB_LEAD_S);
        let usb_wanted = soon || window.get_page() != PAGE_CLOCK;
        if usb_wanted != usb_on && !cfg!(feature = "usb-cycle-test") {
            usb.set_power(usb_wanted);
            usb_on = usb_wanted;
        }
        if power::asleep() {
            if ringing || !allowed {
                log::info!(
                    "power: woken ({}), light {:.1}",
                    if ringing { "ringing" } else { "no longer dark" },
                    crate::ambient::last_light()
                );
                power::wake();
            }
        } else if allowed
            && !ringing
            && window.get_page() == PAGE_CLOCK
            && input::idle().as_millis() >= u64::from(cfg.sleep_after_s) * 1000
        {
            power::sleep();
        } else if ringing && window.get_page() != PAGE_CLOCK {
            show_page(&window, PAGE_CLOCK);
        }
    });
    timer
}

/// The debug overlay's text: brightness, light, and when the display
/// sleeps (or why not).
#[cfg(feature = "debug-overlay")]
fn debug_text(cfg: &PowerConfig, allowed: bool, ringing: bool, page: i32) -> String {
    let dim = u32::from(crate::ui::dim::factor()) * 100 / u32::from(crate::ui::dim::FULL);
    let backlight = crate::ambient::backlight_level() * 100.0;
    let light = crate::ambient::last_light();
    let night = if crate::ambient::night() {
        "night"
    } else {
        "day"
    };
    let sleep = if power::asleep() {
        "asleep".to_string()
    } else if !allowed {
        match cfg.sleep_when {
            SleepWhen::Never => "no sleep: config never".to_string(),
            _ => "no sleep: room not dark".to_string(),
        }
    } else if ringing {
        "no sleep: ringing".to_string()
    } else if page != PAGE_CLOCK {
        "no sleep: not the clock page".to_string()
    } else {
        let idle = input::idle().as_secs();
        let left = u64::from(cfg.sleep_after_s).saturating_sub(idle);
        format!("sleep in {left} s")
    };
    format!("pixels {dim} %  backlight {backlight:.1} %  light {light:.1} ({night})\n{sleep}")
}
