//! Glue between [`crate::display`] and the UI toolkits under test.
//!
//! The panel is portrait (720×1280); the UI is landscape (1280×720). Both
//! toolkits rotate while rendering, so no extra pass or copy is needed:
//! a landscape pixel `(x, y)` lands at panel column `y`, row `1279 - x`
//! (Slint's `RenderingRotation::Rotate270`; the PPA rotates 90°
//! counter-clockwise). The landscape top edge is the panel's left edge.

pub mod list_layer;
pub mod slint_platform;
pub mod stats;
pub mod touch;

use crate::display;

/// Landscape UI width, pixels.
pub const UI_WIDTH: u32 = display::HEIGHT as u32;
/// Landscape UI height, pixels.
pub const UI_HEIGHT: u32 = display::WIDTH as u32;
