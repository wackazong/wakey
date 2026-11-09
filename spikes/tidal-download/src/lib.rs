pub mod auth;
pub mod manifest;
pub mod transfer;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

#[derive(Debug, Serialize, Deserialize)]
pub struct AudioCheck {
    pub duration_seconds: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
    pub bytes: u64,
    pub sha256: String,
}

pub fn check_duration(actual: f64, expected: f64) -> Result<()> {
    ensure!(
        expected.is_finite() && expected > 0.0,
        "Invalid expected duration"
    );
    // Allow catalog rounding and codec padding, capped at two seconds.
    // Short clips get a smaller tolerance so a tiny fragment cannot pass.
    let tolerance = 2.0_f64.min(expected * 0.05);
    ensure!(
        (actual - expected).abs() <= tolerance,
        "Incomplete/wrong audio: decoded {actual:.3}s, expected {expected:.3}s (tolerance {tolerance:.3}s)"
    );
    Ok(())
}

/// Decodes every sample, checking FLAC frame CRCs and declared sample count.
/// No network calls: also usable after disconnecting Wi-Fi and restarting.
pub fn verify_flac(path: &Path, expected: f64) -> Result<AudioCheck> {
    let mut reader = claxon::FlacReader::open(path).context("Not a readable native FLAC")?;
    let info = reader.streaminfo();
    ensure!(
        info.sample_rate > 0 && info.channels > 0,
        "Invalid FLAC stream info"
    );
    let mut count = 0u64;
    for sample in reader.samples() {
        sample.context("FLAC decoding/CRC failure")?;
        count += 1;
    }
    ensure!(
        count > 0 && count.is_multiple_of(u64::from(info.channels)),
        "Empty/incomplete FLAC frames"
    );
    let frames = count / u64::from(info.channels);
    if let Some(declared) = info.samples {
        ensure!(
            frames == declared,
            "Truncated FLAC: decoded sample count differs from STREAMINFO"
        );
    }
    let duration = frames as f64 / f64::from(info.sample_rate);
    check_duration(duration, expected)?;
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(AudioCheck {
        duration_seconds: duration,
        sample_rate: info.sample_rate,
        channels: info.channels,
        bits_per_sample: info.bits_per_sample,
        bytes: file.metadata()?.len(),
        sha256: format!("{:x}", hash.finalize()),
    })
}

/// Verify every sample of the Tab5-compatible PCM WAV and record its checksum.
pub fn verify_wav(path: &Path, expected: f64) -> Result<AudioCheck> {
    let mut reader = hound::WavReader::open(path).context("Invalid WAV")?;
    let spec = reader.spec();
    ensure!(
        spec.sample_rate == 48000
            && spec.channels == 2
            && spec.bits_per_sample == 16
            && spec.sample_format == hound::SampleFormat::Int,
        "WAV must be 48 kHz, 16-bit stereo PCM"
    );
    let declared = reader.len();
    let mut count = 0u64;
    for sample in reader.samples::<i16>() {
        sample.context("Truncated WAV")?;
        count += 1;
    }
    ensure!(
        count > 0 && count == u64::from(declared) && count.is_multiple_of(2),
        "Incomplete WAV"
    );
    let duration = count as f64 / 2.0 / 48000.0;
    check_duration(duration, expected)?;
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(AudioCheck {
        duration_seconds: duration,
        sample_rate: 48000,
        channels: 2,
        bits_per_sample: 16,
        bytes: file.metadata()?.len(),
        sha256: format!("{:x}", hash.finalize()),
    })
}
