use flacenc::{component::BitRepr, error::Verify};
use std::{fs, process::Command};
use tidal_download::verify_flac;

fn audio() -> Vec<u8> {
    let config = flacenc::config::Encoder::default().into_verified().unwrap();
    let samples: Vec<i32> = (0..48000).map(|i| ((i % 100) - 50) * 100).collect();
    let source = flacenc::source::MemSource::from_samples(&samples, 1, 16, 48000);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).unwrap();
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).unwrap();
    sink.into_inner()
}

#[test]
fn validates_real_audio_and_rejects_truncation_corruption_and_preview() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("test.flac");
    let bytes = audio();
    fs::write(&file, &bytes).unwrap();
    let result = verify_flac(&file, 1.0).unwrap();
    assert_eq!(result.duration_seconds, 1.0);
    assert_eq!(result.sample_rate, 48000);
    assert_eq!(result.bits_per_sample, 16);
    assert_eq!(result.sha256.len(), 64);
    assert!(verify_flac(&file, 240.0).is_err());
    fs::write(&file, &bytes[..bytes.len() / 2]).unwrap();
    assert!(verify_flac(&file, 1.0).is_err());
    let mut corrupt = bytes;
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xff;
    fs::write(&file, corrupt).unwrap();
    assert!(verify_flac(&file, 1.0).is_err());
}

#[test]
fn fresh_cli_process_verifies_without_authentication() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("test.flac");
    fs::write(&file, audio()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tidal-download"))
        .env_remove("TIDAL_ACCESS_TOKEN")
        .env_remove("TIDAL_CLIENT_ID")
        .env_remove("TIDAL_CLIENT_SECRET")
        .args(["verify", file.to_str().unwrap(), "--expected-seconds", "1"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["duration_seconds"], 1.0);
}
