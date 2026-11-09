//! USB audio test: brings up the USB Audio Class 1.0 speaker on the USB-A
//! port (Creative Pebble V3) and plays
//! - every WAV/AIFF file in the microSD root and `sounds/` in a loop (a
//!   44.1 kHz test tone is written to the card first, if missing), or
//! - a quiet 440 Hz sine when there is no card or no file.
//!
//! Unplugging and re-plugging the speaker is expected to work. Logs packet
//! and underrun counters every 2 s.
//!
//! `cargo run --release --bin audio_test`

use embassy_time::{Duration, Timer};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use wakey::audio::uac::{self, UsbAudio};
use wakey::audio::{player, player::Player};
use wakey::board::ioexp::Pin;
use wakey::board::{Board, sd};

/// Sine frequency and level (fraction of full scale; 0.1 = −20 dBFS).
const TONE_HZ: f32 = 440.0;
const LEVEL: f32 = 0.1;

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app().unwrap()))
}

fn audio_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in [
        sd::MOUNT_POINT.to_string(),
        format!("{}/sounds", sd::MOUNT_POINT),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            log::info!("sd: {} ({} KiB)", path.display(), size / 1024);
            // macOS metadata (`._name`) and other hidden files.
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|x| x.to_str())
                .map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("wav" | "aif" | "aiff" | "aifc")) {
                files.push(path);
            }
        }
    }
    files.sort();
    // The 44.1 kHz test tone first: it exercises the sample-rate converter.
    files.sort_by_key(|p| !p.to_string_lossy().contains("test-44100"));
    files
}

/// Writes a 5 s, 44.1 kHz, 16-bit stereo WAV (440 Hz left, 660 Hz right)
/// to the card unless it exists.
fn write_test_tone() -> std::io::Result<()> {
    use std::io::Write;
    let path = format!("{}/test-44100.wav", sd::MOUNT_POINT);
    if std::path::Path::new(&path).exists() {
        return Ok(());
    }
    const RATE: u32 = 44_100;
    const SECONDS: u32 = 5;
    let frames = RATE * SECONDS;
    let data_len = frames * 4;
    let mut w = std::io::BufWriter::new(std::fs::File::create(&path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&2u16.to_le_bytes())?; // stereo
    w.write_all(&RATE.to_le_bytes())?;
    w.write_all(&(RATE * 4).to_le_bytes())?;
    w.write_all(&4u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    let tau = core::f32::consts::TAU / RATE as f32;
    for i in 0..frames {
        let t = i as f32;
        // Fade in/out over 50 ms to avoid clicks; −12 dBFS.
        let env = (t / 2205.0).min((frames - 1 - i) as f32 / 2205.0).min(1.0) * 0.25;
        let l = ((t * 440.0 * tau).sin() * env * 32767.0) as i16;
        let r = ((t * 660.0 * tau).sin() * env * 32767.0) as i16;
        w.write_all(&l.to_le_bytes())?;
        w.write_all(&r.to_le_bytes())?;
    }
    w.flush()?;
    log::info!("audio_test: wrote {path}");
    Ok(())
}

fn sine_source() -> uac::Source {
    let mut phase = 0.0f32;
    let step = TONE_HZ / uac::SAMPLE_RATE as f32 * core::f32::consts::TAU;
    Box::new(move |buf: &mut [i16]| {
        for frame in buf.as_chunks_mut::<{ uac::CHANNELS }>().0 {
            let s = (phase.sin() * LEVEL * i16::MAX as f32) as i16;
            frame.fill(s);
            phase += step;
            if phase > core::f32::consts::TAU {
                phase -= core::f32::consts::TAU;
            }
        }
    })
}

#[embassy_executor::task]
async fn app() {
    let Board { ioexp, sd, .. } = Board::init().expect("board init");

    let files = if sd.is_some() {
        write_test_tone().unwrap_or_else(|e| log::warn!("audio_test: test tone not written: {e}"));
        audio_files()
    } else {
        log::warn!("audio_test: no microSD card");
        Vec::new()
    };
    log::info!("audio_test: {} audio files", files.len());

    let player = Player::new();
    let source = if files.is_empty() {
        log::info!("audio_test: no files, playing a {TONE_HZ} Hz sine");
        sine_source()
    } else {
        player.source()
    };
    let usb = UsbAudio::start(ioexp.switch(Pin::Usb5vEn), source).expect("USB host");
    usb.set_streaming(true);

    let mut stats = embassy_time::Ticker::every(Duration::from_secs(2));
    loop {
        for path in &files {
            player.play(path.clone());
            while player.is_playing() {
                stats.next().await;
                log_stats(&usb);
            }
            Timer::after(Duration::from_secs(1)).await;
        }
        if files.is_empty() {
            stats.next().await;
            log_stats(&usb);
        }
    }
}

fn log_stats(usb: &UsbAudio) {
    log::info!(
        "audio_test: speaker {} packets ok {} failed {} underruns {}",
        if usb.connected() {
            "connected"
        } else {
            "absent"
        },
        uac::PACKETS_OK.load(Ordering::Relaxed),
        uac::PACKETS_ERR.load(Ordering::Relaxed),
        player::UNDERRUNS.load(Ordering::Relaxed)
    );
}
