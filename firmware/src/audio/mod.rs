//! Audio output: the USB speaker and the built-in one.
//!
//! - [`uac`]: USB Audio Class 1.0 output (the Creative Pebble V3 on the
//!   USB-A port) on ESP-IDF's USB host library: power, enumeration,
//!   streaming, volume, re-plugging.
//! - [`builtin`]: the built-in speaker (ES8388 codec over I2S): wheel
//!   clicks, and the player's sound when no USB speaker is connected.
//! - [`player`]: file playback (WAV, AIFF, FLAC, M4A/AAC/ALAC, MP3 from
//!   the SD card, converted to 48 kHz) and the built-in tone, as a sample
//!   [`Source`] for either speaker.
//! - [`output`]: routes the player to the USB speaker or the built-in one.
//!
//! File parsing, sample-rate conversion and mixing live in `wakey_core`.

pub mod builtin;
pub mod output;
pub mod player;
pub mod uac;

/// The sample rate of everything played: the Pebble takes only this, and
/// the built-in speaker's I2S runs at it too.
pub const SAMPLE_RATE: u32 = 48_000;

/// Fills interleaved stereo i16 samples at [`SAMPLE_RATE`] (`buf.len()` =
/// frames × 2).
pub type Source = Box<dyn FnMut(&mut [i16]) + Send>;
