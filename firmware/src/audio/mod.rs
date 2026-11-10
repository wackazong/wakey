//! Audio output to the USB speaker.
//!
//! - [`uac`]: USB Audio Class 1.0 output (the Creative Pebble V3 on the
//!   USB-A port) on ESP-IDF's USB host library: power, enumeration,
//!   streaming, volume, re-plugging.
//! - [`player`]: file playback (WAV/AIFF from the SD card, converted to
//!   48 kHz) as a sample source for the stream.
//!
//! File parsing and sample-rate conversion live in `wakey_core`.

pub mod click;
pub mod player;
pub mod uac;
