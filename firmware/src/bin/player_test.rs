//! SD card + player test: lists the card, then plays every WAV/AIFF file in
//! its root (and `sounds/`) on the USB speaker, one after the other, forever.
//!
//! `cargo run --release --bin player_test`

use embassy_time::{Duration, Timer};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use wakey::audio::{player, player::Player, uac};
use wakey::board::ioexp::Pin;
use wakey::board::{Board, sd};

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
/// to the card unless it exists, so the sample-rate conversion can be heard
/// without a 44.1 kHz file at hand.
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
    log::info!("player_test: wrote {path}");
    Ok(())
}

#[embassy_executor::task]
async fn app() {
    let Board {
        mut i2c,
        mut ioexp,
        sd,
        ..
    } = Board::init().expect("board init");
    let _sd = sd.expect("no microSD card");
    write_test_tone().unwrap_or_else(|e| log::warn!("player_test: test tone not written: {e}"));
    let files = audio_files();
    log::info!("player_test: {} audio files", files.len());

    uac::install_host().expect("USB host");
    ioexp.set(&mut i2c, Pin::Usb5vEn, true).expect("USB5V_EN");
    Timer::after(Duration::from_secs(1)).await;
    let player = Player::new();
    uac::play(player.source());
    for attempt in 1..=5 {
        uac::set_port_power(true).expect("port power");
        Timer::after(Duration::from_secs(3)).await;
        if uac::DEVICE_FOUND.load(Ordering::Acquire) {
            break;
        }
        log::warn!("player_test: no speaker yet (attempt {attempt}), cycling the root port");
        uac::set_port_power(false).expect("port power");
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        for path in &files {
            player.play(path.clone());
            while player.is_playing() {
                Timer::after(Duration::from_secs(2)).await;
                log::info!(
                    "player_test: packets {} underruns {}",
                    uac::PACKETS_OK.load(Ordering::Relaxed),
                    player::UNDERRUNS.load(Ordering::Relaxed)
                );
            }
            Timer::after(Duration::from_secs(1)).await;
        }
        if files.is_empty() {
            Timer::after(Duration::from_secs(5)).await;
        }
    }
}
