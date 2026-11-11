//! When the display sleeps (`power` in `config.json`): after
//! `sleep_after_s` without a touch, on the clock page, not while an alarm
//! rings, and only when `sleep_when` allows it: `"dark"` in a dark room
//! (the light meter's night mode) or on battery, `"always"`, `"never"`.
//! It wakes on a touch (`ui::power`), when an alarm rings, and when the
//! condition ends (the room gets light on USB power).

use super::alarm::{AlarmClock, View};
use super::screens::AppWindow;
use super::{PAGE_CLOCK, show_page};
use crate::board::i2c::SharedI2c;
use crate::board::ina226;
use crate::ui::{input, power};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wakey_core::config::{PowerConfig, SleepWhen};

/// Checks twice a second.
pub fn start(
    window: &AppWindow,
    cfg: PowerConfig,
    alarm: Rc<RefCell<AlarmClock>>,
    bus: SharedI2c,
) -> Timer {
    let weak = window.as_weak();
    // The battery reading is an I2C transfer: every 5 s is plenty.
    let mut ticks = 0u32;
    let mut on_battery = false;
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
        if power::asleep() {
            if ringing || !allowed {
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
