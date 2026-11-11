//! The UI layer: Slint on the display, touch input, and the list layer.
//!
//! The panel is portrait (720×1280); the UI is landscape (1280×720).
//! Rendering rotates on the way, with no extra pass: a landscape pixel
//! `(x, y)` lands at panel column `y`, row `1279 - x` (the PPA rotates
//! 90° counter-clockwise). The landscape top edge is the panel's left edge.
//!
//! - [`platform`]: Slint platform and windows, rendering through the PPA.
//! - [`render_loop`]: the frame loop and the [`render_loop::FrameHook`]
//!   extension point.
//! - [`list_layer`]: a scrolling list composed by DMA (a `FrameHook`).
//! - [`input`]: touch polling → landscape pointer events.
//! - [`transition`]: page changes as slides, composed by the 2D-DMA.
//! - [`dim`]: darkening every pixel for the night (with the lowest
//!   backlight).
//! - [`stats`]: frame statistics; `trace`: a per-frame log for performance
//!   work (feature `ui-bench`).

pub mod dim;
pub mod input;
pub mod list_layer;
pub mod platform;
pub mod render_loop;
pub mod stats;
#[cfg(feature = "ui-bench")]
pub mod trace;
pub mod transition;

use crate::display;

/// Landscape UI width, pixels.
pub const UI_WIDTH: u32 = display::HEIGHT as u32;
/// Landscape UI height, pixels.
pub const UI_HEIGHT: u32 = display::WIDTH as u32;

/// A Slint colour as the panel's RGB565.
pub fn rgb565(c: slint::Color) -> u16 {
    u16::from(c.red() >> 3) << 11 | u16::from(c.green() >> 2) << 5 | u16::from(c.blue() >> 3)
}
