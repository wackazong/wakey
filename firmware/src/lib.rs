//! Tab5 alarm clock firmware: ESP-IDF (std) + Embassy.
//!
//! Layers (see `architecture/developtment concept.md`):
//! - [`board`]: facts about the PCB (pins, I2C bus, IO expanders, panel
//!   variant) and the bring-up order in [`board::Board::init`].
//! - [`display`]: the MIPI-DSI panel, its PSRAM frame buffers and tear-free
//!   buffer swapping. No UI toolkit knowledge.
//! - [`ui`]: glue between the display and the UI toolkits under test
//!   (Slint, Buoyant), plus frame statistics.
//! - [`runtime`]: starting the Embassy executor on top of FreeRTOS.
//!
//! Applications live in `src/bin/` and only use these handles.

pub mod audio;
pub mod board;
pub mod clock;
pub mod config;
pub mod display;
pub mod net;
pub mod profiler;
pub mod runtime;
pub mod ui;
