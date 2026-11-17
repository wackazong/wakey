//! Display power: on, or asleep (backlight off, nothing drawn). The app
//! decides when (`app::power`); the render loop carries it out
//! ([`super::render_loop`]): it stops drawing and wakes on a touch, which is
//! swallowed (it only wakes). The panel is not put into sleep mode: the
//! ST7123's touch half stops scanning then (`docs/pitfalls.md`).
//! The light meter ([`crate::ambient`]) switches the backlight off while
//! asleep and on again once the first fresh frame is on the panel.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static ASLEEP: AtomicBool = AtomicBool::new(false);
/// The pending [`Request`], 0 for none.
static REQUEST: AtomicU8 = AtomicU8::new(0);

/// What the app asks of the render loop.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
pub(super) enum Request {
    Sleep = 1,
    Wake,
}

impl Request {
    fn from_u8(v: u8) -> Option<Self> {
        [Self::Sleep, Self::Wake]
            .into_iter()
            .find(|r| *r as u8 == v)
    }
}

/// True from the moment the panel goes to sleep until the first frame
/// after waking is on it.
pub fn asleep() -> bool {
    ASLEEP.load(Ordering::Relaxed)
}

/// Asks the render loop to put the display to sleep. Called from the UI
/// thread (the app's power timer, the render loop itself); the request is
/// an atomic, so another thread could call it too.
pub fn sleep() {
    REQUEST.store(Request::Sleep as u8, Ordering::Relaxed);
}

/// Asks the render loop to wake the display (see [`sleep`] for the
/// callers).
pub fn wake() {
    REQUEST.store(Request::Wake as u8, Ordering::Relaxed);
}

/// The request since the last call, if any.
pub(super) fn take_request() -> Option<Request> {
    Request::from_u8(REQUEST.swap(0, Ordering::Relaxed))
}

/// The render loop's report: the display went to sleep, or is awake again.
pub(super) fn set_asleep(on: bool) {
    if ASLEEP.swap(on, Ordering::Relaxed) != on {
        log::info!("power: display {}", if on { "asleep" } else { "awake" });
    }
}
