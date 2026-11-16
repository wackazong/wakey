//! Firmware of the Tab5 alarm clock: Rust (std) on ESP-IDF with the Embassy
//! executor. See `docs/architecture.md` for the overall design.
//!
//! Layers, top to bottom; each uses only the ones below it:
//!
//! - [`app`]: the alarm clock application (screens, wiring, UI texts).
//! - Services: [`ui`] (Slint, touch, list layer), [`audio`] (USB speaker,
//!   built-in speaker, file playback), [`net`] (Wi-Fi sessions, NTP),
//!   [`tidal`] (login, playlist, downloads) over [`http`], [`airdrop`]
//!   (sound files from iPhones), [`sounds`] (the card's sound files),
//!   [`clock`] (local time, RTC), [`config`] (`config.json`), [`ambient`]
//!   (backlight from the room's light, camera as light meter).
//! - Hardware: [`board`] (pins, chips, power switches, bring-up),
//!   [`display`] (panel, frame buffers, PPA, 2D-DMA) and [`c6`] (the Wi-Fi
//!   co-processor: power, esp_hosted's transport, its images).
//! - Support: [`runtime`] (threads, the Embassy executor), [`psram`]
//!   (DMA buffers), [`taskstats`] (CPU time per task).
//!
//! Pure logic (dates, file formats, resampling, descriptors, config model,
//! TIDAL's protocol, scroll physics) is in the `wakey_core` crate and
//! tested on the host. Binaries in `src/bin/` are thin: the product
//! (`wakey`) and the UI bench (`ui_bench`).

pub mod airdrop;
pub mod ambient;
pub mod app;
pub mod audio;
pub mod board;
pub mod c6;
pub mod clock;
pub mod config;
pub mod display;
pub mod http;
pub mod net;
pub mod psram;
pub mod runtime;
pub mod sounds;
pub mod taskstats;
pub mod tidal;
pub mod ui;
