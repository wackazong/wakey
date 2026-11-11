//! Dimming by pixels: in a dark room the backlight is at its lowest usable
//! level ([`crate::ambient`]), which is still too bright for the night, so
//! every pixel Slint draws is scaled down on its way to the PPA
//! ([`super::platform`]). Everything on screen comes through there: the
//! pages, the list layer's rows and swipe pictures, and the slides (copies
//! of drawn frames).
//!
//! A change takes effect for what is drawn afterwards: the app repaints the
//! whole window ([`changed_since`]), the list layer re-renders its rows (the
//! [`epoch`] is part of their stamps).

use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

/// Brightness factor in 1/256: 256 = undimmed.
pub const FULL: u16 = 256;
static FACTOR: AtomicU16 = AtomicU16::new(FULL);
static EPOCH: AtomicU32 = AtomicU32::new(0);

/// Sets the brightness factor in 1/256 (any thread; [`FULL`] = undimmed).
pub fn set(factor: u16) {
    let factor = factor.min(FULL);
    if FACTOR.swap(factor, Ordering::Relaxed) != factor {
        EPOCH.fetch_add(1, Ordering::Relaxed);
        log::debug!("dim: {factor}/256");
    }
}

/// The brightness factor in 1/256.
pub fn factor() -> u16 {
    FACTOR.load(Ordering::Relaxed)
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

/// Scales a freshly drawn line (RGB565) by the factor, to 1/32: the
/// channels are spread over a 32-bit word with gaps (green in the upper
/// half), so one multiply scales all three (~20 ms less per full screen
/// than three).
pub fn apply(px: &mut [u16]) {
    let f = u32::from(FACTOR.load(Ordering::Relaxed));
    if f >= u32::from(FULL) {
        return;
    }
    let f32_ = (f + 4) >> 3;
    const MASK: u32 = 0x07e0_f81f;
    for p in px {
        let x = (u32::from(*p) | (u32::from(*p) << 16)) & MASK;
        let x = ((x * f32_) >> 5) & MASK;
        *p = (x | (x >> 16)) as u16;
    }
}
