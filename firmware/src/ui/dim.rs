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

/// The channels of an RGB565 pixel spread over a 32-bit word with gaps
/// (green in the upper half), so one multiply scales all three.
const SPREAD: u32 = 0x07e0_f81f;

/// The factor in 1/32 (what the gaps of [`SPREAD`] leave room for), or
/// `None` while undimmed.
fn factor_32() -> Option<u32> {
    let f = u32::from(factor());
    (f < u32::from(FULL)).then_some((f + 4) >> 3)
}

/// Sets the brightness factor in 1/256 (any thread; [`FULL`] = undimmed).
pub fn set(factor: u16) {
    let factor = factor.min(FULL);
    if FACTOR.swap(factor, Ordering::Relaxed) != factor {
        // Release, paired with the acquire in `epoch`: a reader on the
        // other core that sees the new epoch also sees the new factor, and
        // does not stamp rows of the old brightness with it.
        EPOCH.fetch_add(1, Ordering::Release);
        log::debug!("dim: {factor}/256");
    }
}

/// The brightness factor in 1/256.
pub fn factor() -> u16 {
    FACTOR.load(Ordering::Relaxed)
}

/// Counts the changes: drawn content from another epoch is stale.
pub fn epoch() -> u32 {
    EPOCH.load(Ordering::Acquire)
}

/// True once if the dimming changed since `seen` (updated).
pub fn changed_since(seen: &mut u32) -> bool {
    let now = epoch();
    core::mem::replace(seen, now) != now
}

/// One colour scaled by the factor, rounded to the nearest level (for
/// areas filled without Slint, e.g. the list layer's background).
pub fn scale(px: u16) -> u16 {
    let Some(f) = factor_32() else {
        return px;
    };
    const HALF: u32 = 16 | (16 << 11) | (16 << 21);
    let x = (u32::from(px) | (u32::from(px) << 16)) & SPREAD;
    let x = ((x * f + HALF) >> 5) & SPREAD;
    (x | (x >> 16)) as u16
}

/// Scales a freshly drawn line (RGB565) by the factor, to 1/32, with one
/// multiply per pixel ([`SPREAD`]; ~20 ms less per full screen than
/// three).
///
/// The scaled channels have 5 bits of fraction. Cut off, a dimmed
/// gradient shows stripes (at 35 % a channel has ~11 levels left, and
/// the renderer's dithering shrinks below one level); instead an ordered
/// (4×4 Bayer) threshold per pixel decides between the two neighbouring
/// levels, so the average stays right. `x0`, `y`: the line's first pixel
/// on screen (the pattern is offset from the renderer's, which uses the
/// same matrix).
pub fn apply(px: &mut [u16], x0: usize, y: usize) {
    let Some(f) = factor_32() else {
        return;
    };
    const BAYER: [[u32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    // The threshold (0..31) at each channel's lowest bit: blue 0, red 11,
    // green 21. Fits: 31 × 32 + 31 < 2^10, 63 × 32 + 31 < 2^11.
    let row = &BAYER[(y + 2) & 3];
    let pattern: [u32; 4] = core::array::from_fn(|i| {
        let t = row[(x0 + i + 1) & 3] * 2 + 1;
        t | (t << 11) | (t << 21)
    });
    for chunk in px.chunks_mut(4) {
        for (p, d) in chunk.iter_mut().zip(pattern) {
            let x = (u32::from(*p) | (u32::from(*p) << 16)) & SPREAD;
            let x = ((x * f + d) >> 5) & SPREAD;
            *p = (x | (x >> 16)) as u16;
        }
    }
}
