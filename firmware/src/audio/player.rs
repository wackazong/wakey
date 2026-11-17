//! File playback: a decoder thread reads an [`AudioFile`], converts it to
//! 48 kHz stereo i16 ([`Converter`] for other rates, mostly 44.1 kHz) and
//! fills a one-second ring buffer in PSRAM; the speaker's stream (USB or
//! built-in, one at a time, see `audio::output`) takes from the ring
//! through [`Player::source`]. When the ring runs empty the stream plays
//! silence and counts an underrun.
//!
//! [`Player::ring`] is the alarm: a file in a loop, or the built-in
//! [`Beeper`] tone if there is no file or it cannot be read, with a volume
//! and a fade-in applied in [`Player::source`]. [`Player::preview`] is the
//! sound page's preview: the same, but silent instead of the tone when the
//! sound cannot be played; it also plays a [`Sound::Stream`] (a track still
//! downloading).

use super::{SAMPLE_RATE, Source};
use crate::runtime::Thread;
use esp_idf_svc::hal::cpu::Core;
use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use wakey_core::audio_file::AudioFile;
use wakey_core::resample::Converter;
use wakey_core::tone::{self, Beeper};

/// Ring capacity: one second of 48 kHz stereo.
const RING_SAMPLES: usize = SAMPLE_RATE as usize * 2;
/// Frames decoded per step.
const CHUNK_FRAMES: usize = 4096;

struct Ring {
    samples: Mutex<VecDeque<i16>>,
    space: Condvar,
    playing: AtomicBool,
    /// Set once the decoder has delivered data: underruns count from here.
    armed: AtomicBool,
    stop: AtomicBool,
    /// Output gain (f32 bits) and fade-in length in frames.
    gain: AtomicU32,
    fade_frames: AtomicU32,
    /// Set when a sound starts: the source restarts the fade.
    restart_fade: AtomicBool,
}

/// What [`Player::ring`] and [`Player::preview`] play.
pub enum Sound {
    /// A file, repeated; the tone if it cannot be played (alarm only).
    File(PathBuf),
    Tone,
    /// A file that is still being written (a name for the log, a reader
    /// that waits for more).
    Stream(String, Box<dyn ReadSeek + Send>),
}

/// A source for [`Sound::Stream`].
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Times the stream found the ring empty while a sound played, for logs.
pub static UNDERRUNS: AtomicU32 = AtomicU32::new(0);

pub struct Player {
    ring: Arc<Ring>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Self {
            ring: Arc::new(Ring {
                samples: Mutex::new(VecDeque::with_capacity(RING_SAMPLES)),
                space: Condvar::new(),
                playing: AtomicBool::new(false),
                armed: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                gain: AtomicU32::new(1f32.to_bits()),
                fade_frames: AtomicU32::new(0),
                restart_fade: AtomicBool::new(false),
            }),
        }
    }

    /// A sample source for a speaker's stream ([`super::uac::UsbAudio::start`],
    /// `BuiltinSpeaker::start`). Only one may take from it at a time.
    pub fn source(&self) -> Source {
        let ring = self.ring.clone();
        let mut frames: u64 = 0;
        Box::new(move |buf: &mut [i16]| {
            if ring.restart_fade.swap(false, Ordering::AcqRel) {
                frames = 0;
            }
            let full = f32::from_bits(ring.gain.load(Ordering::Relaxed));
            let fade = u64::from(ring.fade_frames.load(Ordering::Relaxed));
            let gain = tone::faded(full, frames, fade);
            let mut q = ring.samples.lock().unwrap();
            let n = buf.len().min(q.len());
            for (dst, s) in buf.iter_mut().zip(q.drain(..n)) {
                *dst = if gain == 1.0 {
                    s
                } else {
                    (f32::from(s) * gain) as i16
                };
            }
            frames += (n / 2) as u64;
            drop(q);
            ring.space.notify_one();
            buf[n..].fill(0);
            if n < buf.len() && ring.armed.load(Ordering::Relaxed) {
                UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            }
        })
    }

    fn is_playing(&self) -> bool {
        self.ring.playing.load(Ordering::Acquire)
    }

    /// Stops the current file (the ring drains within a second).
    pub fn stop(&self) {
        self.ring.stop.store(true, Ordering::Release);
        // The decoder checks `stop` and waits under the lock: taking it
        // here makes sure it is either before its check or already
        // waiting, so the notification is not lost.
        drop(self.ring.samples.lock().unwrap());
        self.ring.space.notify_all();
    }

    /// Plays `sound` until [`stop`](Self::stop), at `gain` (linear) after a
    /// fade-in of `fade_s` seconds.
    pub fn ring(&self, sound: Sound, gain: f32, fade_s: u16) {
        let fade_frames = u32::from(fade_s) * SAMPLE_RATE;
        self.start(gain, fade_frames, move |ring| {
            if let Err(e) = play_sound(sound, ring) {
                log::error!("player: {e}; playing the tone");
                beep(ring);
            }
        });
    }

    /// Plays `sound` until [`stop`](Self::stop), at `gain`, without fade-in;
    /// silent if it cannot be played (no tone in its place).
    pub fn preview(&self, sound: Sound, gain: f32) {
        self.start(gain, 0, move |ring| {
            if let Err(e) = play_sound(sound, ring) {
                log::warn!("player: preview: {e}; silent");
            }
        });
    }

    /// Changes the gain of what is playing (the volume slider during a
    /// preview).
    pub fn set_gain(&self, gain: f32) {
        self.ring.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Stops what is playing and starts `produce` on a new decoder thread.
    fn start(&self, gain: f32, fade_frames: u32, produce: impl FnOnce(&Ring) + Send + 'static) {
        let ring = self.ring.clone();
        // Let a previous decoder see the stop and clear the ring.
        if self.is_playing() {
            self.stop();
            while self.is_playing() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        ring.samples.lock().unwrap().clear();
        ring.gain.store(gain.to_bits(), Ordering::Relaxed);
        ring.fade_frames.store(fade_frames, Ordering::Relaxed);
        ring.restart_fade.store(true, Ordering::Release);
        ring.stop.store(false, Ordering::Release);
        ring.armed.store(false, Ordering::Release);
        ring.playing.store(true, Ordering::Release);
        // On core 1, away from the UI (core 0): decoding and resampling can
        // take a good part of a core, and the decoder's priority (5) is
        // above the UI task's (1). 64 KiB of stack in PSRAM: symphonia's
        // MP4 reader and AAC decoder overflow 8 KiB (stack protection
        // fault in `MediaSourceStream::refill_buffer`).
        Thread::new(c"decoder", 64 * 1024)
            .core(Core::Core1)
            .stack_in_psram()
            .spawn(move || {
                produce(&ring);
                // Let the ring drain before reporting the end; the tail
                // running dry is not an underrun.
                ring.armed.store(false, Ordering::Release);
                while !ring.samples.lock().unwrap().is_empty() && !ring.stop.load(Ordering::Acquire)
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                ring.playing.store(false, Ordering::Release);
            })
            .expect("decoder thread");
    }
}

/// Plays `sound` into the ring until it is stopped (files and streams
/// repeat); an error if it cannot be played.
fn play_sound(sound: Sound, ring: &Ring) -> Result<(), String> {
    match sound {
        Sound::Tone => {
            beep(ring);
            Ok(())
        }
        Sound::File(path) => decode(&path, ring).map_err(|e| format!("{}: {e}", path.display())),
        Sound::Stream(name, reader) => {
            AudioFile::from_reader(BufReader::with_capacity(32 * 1024, reader))
                .and_then(|file| decode_from(&name, file, ring))
                .map_err(|e| format!("{name}: {e}"))
        }
    }
}

/// Decodes `path` into the ring, from the start again at its end.
fn decode(path: &Path, ring: &Ring) -> io::Result<()> {
    let file = AudioFile::open(path)?;
    decode_from(&path.display().to_string(), file, ring)
}

/// [`decode`] from an opened file (`name` for the log).
fn decode_from<R: Read + Seek + Send + 'static>(
    name: &str,
    mut file: AudioFile<R>,
    ring: &Ring,
) -> io::Result<()> {
    let f = file.format.clone();
    log::info!(
        "player: {}: {:?}, {} Hz, {} ch, {} bit, {:.1} s",
        name,
        f.container,
        f.sample_rate,
        f.channels,
        f.bits,
        f.duration_s()
    );
    let mut resampler = if f.sample_rate != SAMPLE_RATE {
        let c = Converter::new(f.sample_rate, SAMPLE_RATE).map_err(io::Error::other)?;
        log::info!(
            "player: converting {} Hz → {} Hz ({} frames per block)",
            f.sample_rate,
            SAMPLE_RATE,
            c.block_frames()
        );
        Some(c)
    } else {
        None
    };
    let mut input = vec![0i16; CHUNK_FRAMES * 2];
    let mut converted = Vec::with_capacity(CHUNK_FRAMES * 2 * 2);
    // Frames read since the file was opened or rewound.
    let mut pass_frames = 0;
    loop {
        if ring.stop.load(Ordering::Acquire) {
            ring.samples.lock().unwrap().clear();
            return Ok(());
        }
        let frames = file.read(&mut input)?;
        if frames == 0 {
            if f.frames == 0 {
                return Ok(());
            }
            // A file that announces frames and yields none would be
            // rewound in a busy loop.
            if pass_frames == 0 {
                return Err(io::Error::other("no audio data"));
            }
            file.rewind()?;
            pass_frames = 0;
            continue;
        }
        pass_frames += frames;
        let out: &[i16] = match resampler.as_mut() {
            Some(r) => {
                converted.clear();
                r.process(&input[..frames * 2], &mut converted);
                &converted
            }
            None => &input[..frames * 2],
        };
        push(ring, out);
    }
}

/// The built-in tone until stopped.
fn beep(ring: &Ring) {
    log::info!("player: built-in tone");
    let mut beeper = Beeper::new(SAMPLE_RATE);
    let mut buf = vec![0i16; CHUNK_FRAMES * 2];
    while !ring.stop.load(Ordering::Acquire) {
        beeper.fill(&mut buf);
        push(ring, &buf);
    }
    ring.samples.lock().unwrap().clear();
}

/// Appends `samples`, waiting for space.
fn push(ring: &Ring, mut samples: &[i16]) {
    while !samples.is_empty() {
        let mut q = ring.samples.lock().unwrap();
        while q.len() >= RING_SAMPLES && !ring.stop.load(Ordering::Acquire) {
            q = ring.space.wait(q).unwrap();
        }
        if ring.stop.load(Ordering::Acquire) {
            return;
        }
        let n = samples.len().min(RING_SAMPLES - q.len());
        q.extend(&samples[..n]);
        samples = &samples[n..];
        // Half a buffer of lead before underruns count.
        if q.len() >= RING_SAMPLES / 2 {
            ring.armed.store(true, Ordering::Release);
        }
    }
}
