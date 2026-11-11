//! Dimming by pixels: in a dark room the backlight is at its lowest usable
//! level ([`crate::ambient`]), which is still too bright for the night, so
//! every pixel Slint draws is halved on its way to the PPA
//! ([`super::platform`]). Everything on screen comes through there: the
//! pages, the list layer's rows and swipe pictures, and the slides (copies
//! of drawn frames).
//!
//! A change takes effect for what is drawn afterwards: the app repaints the
//! whole window ([`changed_since`]), the list layer re-renders its rows (the
//! [`epoch`] is part of their stamps).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static ON: AtomicBool = AtomicBool::new(false);
static EPOCH: AtomicU32 = AtomicU32::new(0);

/// Switches dimming (any thread).
pub fn set(on: bool) {
    if ON.swap(on, Ordering::Relaxed) != on {
        EPOCH.fetch_add(1, Ordering::Relaxed);
        log::info!("dim: {}", if on { "on" } else { "off" });
    }
}

/// Counts the changes: drawn content from another epoch is stale.
pub fn epoch() -> u32 {
    EPOCH.load(Ordering::Relaxed)
}

/// True once if the dimming changed since `seen` (updated).
pub fn changed_since(seen: &mut u32) -> bool {
    let now = epoch();
    core::mem::replace(seen, now) != now
}

/// Halves a freshly drawn line if dimming is on.
pub fn apply(px: &mut [u16]) {
    if ON.load(Ordering::Relaxed) {
        for p in px {
            // Each channel shifted right by one, without bleeding into the
            // next: RRRRRGGGGGGBBBBB → 0RRRR0GGGGG0BBBB.
            *p = (*p >> 1) & 0x7bef;
        }
    }
}
