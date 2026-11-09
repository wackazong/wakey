//! Audio output.
//!
//! - [`uac`]: a USB Audio Class 1.0 speaker (the Creative Pebble V3 on the
//!   USB-A port), driven through ESP-IDF's USB host library from Rust. No C
//!   class driver: descriptors, class requests and isochronous streaming
//!   are implemented here.
//! - [`file`]: WAV and AIFF/AIFC readers (uncompressed PCM).
//! - [`player`]: decoder thread → ring buffer → USB stream, with sample-rate
//!   conversion to 48 kHz.

pub mod file;
pub mod player;
pub mod uac;
