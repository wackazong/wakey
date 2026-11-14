//! Firmware of the Tab5 alarm clock: Rust (std) on ESP-IDF with the Embassy
//! executor. See `docs/architecture.md` for the overall design.
//!
//! Layers, top to bottom; each uses only the ones below it:
//!
//! - [`app`]: the alarm clock application (screens, wiring).
//! - Services: [`ui`] (Slint, touch, list layer), [`audio`] (USB speaker,
//!   file playback), [`net`] (Wi-Fi, NTP), [`clock`] (local time, RTC),
//!   [`config`] (`config.json`), [`ambient`] (backlight from the room's
//!   light, camera as light meter).
//! - Hardware: [`board`] (pins, chips, power switches, bring-up) and
//!   [`display`] (panel, frame buffers, PPA, 2D-DMA).
//! - [`runtime`]: starting the Embassy executor; [`profiler`]: a sampling
//!   profiler for performance work.
//!
//! Pure logic (dates, file formats, resampling, descriptors, config model,
//! scroll physics) is in the `wakey_core` crate and tested on the host.
//! Binaries in `src/bin/` are thin: the product (`wakey`) and two benches.

pub mod airdrop;
pub mod ambient;
pub mod app;
pub mod audio;
pub mod board;
pub mod c6_ota;
#[cfg(feature = "c6-update")]
pub mod c6_update;
pub mod clock;
pub mod config;
pub mod display;
pub mod http;
pub mod net;
#[cfg(feature = "download-bench")]
pub mod netbench;
pub mod profiler;
pub mod runtime;
pub mod sounds;
pub mod taskstats;
pub mod tidal;
pub mod ui;
