//! Hardware-independent logic of Wakey, an alarm clock for the M5Stack
//! Tab5.
//!
//! Everything here builds and is tested on the host (`cargo test` in this
//! directory); the firmware links it for the ESP32-P4. Nothing here knows
//! about ESP-IDF, chips or threads.
//!
//! - [`airdrop`]: files received by AirDrop (the C6's frames, names, part
//!   files).
//! - [`alarm`]: alarm settings and the ringing state machine.
//! - [`audio_file`]: audio file readers (WAV, AIFF, FLAC, MP3, AAC or ALAC
//!   in M4A, ADTS AAC) → stereo i16.
//! - [`config`]: the `config.json` model.
//! - [`datetime`]: calendar dates ↔ Unix time.
//! - [`lang`]: the screen's texts per language (`lang/*.json`).
//! - [`mix`]: sample operations for the built-in speaker (mono, mixing,
//!   keep-alive).
//! - [`resample`]: sample-rate conversion to the speaker's 48 kHz.
//! - [`scroll`]: touch scrolling physics (drag, fling, rubber band, spring).
//! - [`sky`]: the sun's elevation and the clock face's sky gradient.
//! - [`tidal`]: TIDAL login, playlist sync and track downloads (requests
//!   and responses only, no I/O).
//! - [`tone`]: the built-in alarm tone, volume curve, fade-in and wheel
//!   click.
//! - [`uac`]: USB Audio Class 1.0 descriptor parsing for USB speakers.

pub mod airdrop;
pub mod alarm;
pub mod audio_file;
pub mod config;
pub mod datetime;
pub mod lang;
pub mod mix;
pub mod resample;
pub mod scroll;
pub mod sky;
pub mod tidal;
pub mod tone;
pub mod uac;
