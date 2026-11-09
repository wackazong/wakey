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
//! - [`stats`]: frame statistics.

pub mod input;
pub mod list_layer;
pub mod platform;
pub mod render_loop;
pub mod stats;

use crate::display;

/// Landscape UI width, pixels.
pub const UI_WIDTH: u32 = display::HEIGHT as u32;
/// Landscape UI height, pixels.
pub const UI_HEIGHT: u32 = display::WIDTH as u32;
