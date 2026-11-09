//! File playback: a decoder thread reads an [`AudioFile`], converts it to
//! 48 kHz stereo i16 ([`Converter`] for other rates, mostly 44.1 kHz) and
//! fills a one-second ring buffer in PSRAM; the USB audio stream takes from
//! the ring through [`Player::source`]. When the ring runs empty the stream
//! plays silence and counts an underrun.

use super::uac::{self, Source};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use wakey_core::audio_file::AudioFile;
use wakey_core::resample::Converter;

/// Ring capacity: one second of 48 kHz stereo.
const RING_SAMPLES: usize = uac::SAMPLE_RATE as usize * 2;
/// Frames decoded per step.
const CHUNK_FRAMES: usize = 4096;

struct Ring {
    samples: Mutex<VecDeque<i16>>,
    space: Condvar,
    playing: AtomicBool,
    /// Set once the decoder has delivered data: underruns count from here.
    armed: AtomicBool,
    stop: AtomicBool,
}

/// Counters for logs.
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
            }),
        }
    }

    /// The sample source for the USB stream ([`uac::UsbAudio::start`]).
    pub fn source(&self) -> Source {
        let ring = self.ring.clone();
        Box::new(move |buf: &mut [i16]| {
            let mut q = ring.samples.lock().unwrap();
            let n = buf.len().min(q.len());
            for (dst, s) in buf.iter_mut().zip(q.drain(..n)) {
                *dst = s;
            }
            drop(q);
            ring.space.notify_one();
            buf[n..].fill(0);
            if n < buf.len() && ring.armed.load(Ordering::Relaxed) {
                UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            }
        })
    }

    pub fn is_playing(&self) -> bool {
        self.ring.playing.load(Ordering::Acquire)
    }

    /// Stops the current file (the ring drains within a second).
    pub fn stop(&self) {
        self.ring.stop.store(true, Ordering::Release);
        self.ring.space.notify_all();
    }

    /// Starts playing `path` on a decoder thread; returns at once.
    pub fn play(&self, path: impl Into<PathBuf>) {
        let path = path.into();
        let ring = self.ring.clone();
        ring.stop.store(false, Ordering::Release);
        ring.armed.store(false, Ordering::Release);
        ring.playing.store(true, Ordering::Release);
        std::thread::Builder::new()
            .name("decoder".into())
            .stack_size(8192)
            .spawn(move || {
                if let Err(e) = decode(&path, &ring) {
                    log::error!("player: {}: {e}", path.display());
                }
                // Let the ring drain before reporting the end; the tail
                // running dry is not an underrun.
                ring.armed.store(false, Ordering::Release);
                while !ring.samples.lock().unwrap().is_empty() && !ring.stop.load(Ordering::Acquire)
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                ring.armed.store(false, Ordering::Release);
                ring.playing.store(false, Ordering::Release);
            })
            .expect("decoder thread");
    }
}

fn decode(path: &std::path::Path, ring: &Ring) -> std::io::Result<()> {
    let mut file = AudioFile::open(path)?;
    let f = file.format.clone();
    log::info!(
        "player: {}: {:?}, {} Hz, {} ch, {} bit, {:.1} s",
        path.display(),
        f.container,
        f.sample_rate,
        f.channels,
        f.bits,
        f.duration_s()
    );
    let mut resampler = if f.sample_rate != uac::SAMPLE_RATE {
        let c = Converter::new(f.sample_rate, uac::SAMPLE_RATE).map_err(std::io::Error::other)?;
        log::info!(
            "player: converting {} Hz → {} Hz ({} frames per block)",
            f.sample_rate,
            uac::SAMPLE_RATE,
            c.block_frames()
        );
        Some(c)
    } else {
        None
    };
    let mut input = vec![0i16; CHUNK_FRAMES * 2];
    let mut converted = Vec::with_capacity(CHUNK_FRAMES * 2 * 2);
    loop {
        if ring.stop.load(Ordering::Acquire) {
            ring.samples.lock().unwrap().clear();
            return Ok(());
        }
        let frames = file.read(&mut input)?;
        if frames == 0 {
            return Ok(());
        }
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
