//! Hardware-independent logic of the Tab5 alarm clock.
//!
//! Everything here builds and is tested on the host (`cargo test` in this
//! directory); the firmware links it for the ESP32-P4. Nothing here knows
//! about ESP-IDF, chips or threads.
//!
//! - [`alarm`]: alarm settings and the ringing state machine.
//! - [`datetime`]: calendar dates ↔ Unix time, German date strings.
//! - [`audio_file`]: WAV and AIFF/AIFC readers (PCM → stereo i16).
//! - [`resample`]: sample-rate conversion to the speaker's 48 kHz.
//! - [`uac`]: USB Audio Class 1.0 configuration descriptor parsing.
//! - [`config`]: the `config.json` model.
//! - [`tone`]: the built-in alarm tone, volume curve and fade-in.
//! - [`scroll`]: touch scrolling physics (drag, fling, rubber band, spring).

pub mod alarm;
pub mod audio_file;
pub mod config;
pub mod datetime;
pub mod resample;
pub mod scroll;
pub mod tone;
pub mod uac;
