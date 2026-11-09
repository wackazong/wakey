//! Audio output.
//!
//! [`uac`]: a USB Audio Class 1.0 speaker (the Creative Pebble V3 on the
//! USB-A port), driven through ESP-IDF's USB host library from Rust. No C
//! class driver: the descriptors, class requests and isochronous streaming
//! are implemented here.

pub mod uac;
