//! Display power: on, or asleep (backlight off, nothing drawn). The app
//! decides when (`app::power`); the render loop carries it out
//! ([`super::render_loop`]): it stops drawing and wakes on a touch, which is
//! swallowed (it only wakes). The panel is not put into sleep mode: the
//! ST7123's touch half stops scanning then (`docs/pitfalls.md`).
//! The light meter ([`crate::ambient`]) switches the backlight off while
//! asleep and on again once the first fresh frame is on the panel.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static ASLEEP: AtomicBool = AtomicBool::new(false);
/// 0 none, 1 sleep, 2 wake.
static REQUEST: AtomicU8 = AtomicU8::new(0);

/// True from the moment the panel goes to sleep until the first frame
/// after waking is on it.
pub fn asleep() -> bool {
    ASLEEP.load(Ordering::Relaxed)
}

/// Asks the render loop to put the display to sleep (any thread).
pub fn sleep() {
    REQUEST.store(1, Ordering::Relaxed);
}

/// Asks the render loop to wake the display (any thread).
pub fn wake() {
    REQUEST.store(2, Ordering::Relaxed);
}

/// The pending request: `Some(true)` sleep, `Some(false)` wake.
pub(super) fn take_request() -> Option<bool> {
    match REQUEST.swap(0, Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

pub(super) fn set_asleep(on: bool) {
    if ASLEEP.swap(on, Ordering::Relaxed) != on {
        log::info!("power: display {}", if on { "asleep" } else { "awake" });
    }
}
