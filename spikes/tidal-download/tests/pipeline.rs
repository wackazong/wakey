use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    thread,
    time::Duration,
};
use tidal_download::{
    manifest::{MediaPlan, dash_urls},
    transfer, verify_flac, verify_wav,
};

// Local deterministic server: real HTTP transfer, no account or external network.
fn server(replies: Vec<(u16, Vec<u8>, Option<usize>)>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        for (status, body, declared) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(
                !String::from_utf8_lossy(&request)
                    .to_lowercase()
                    .contains("authorization:")
            );
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\nRetry-After: 0\r\n\r\n", declared.unwrap_or(body.len())).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (address, handle)
}
fn generate(path: &Path, extra: &[&str]) {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=5",
        ])
        .args(extra)
        .arg(path)
        .output()
        .expect("FFmpeg must be installed (included in the dev container)");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
#[tokio::test]
async fn retries_truncated_bodies_without_duplicating_bytes_and_converts_wav() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("original.flac");
    generate(&source, &["-c:a", "flac"]);
    let bytes = fs::read(source).unwrap();
    let (address, handle) = server(vec![
        (503, vec![], None),
        (200, bytes[..50].to_vec(), Some(bytes.len())),
        (200, bytes.clone(), None),
    ]);
    let raw = dir.path().join("raw");
    transfer::fetch(
        &client(),
        &MediaPlan::Mirrors(vec![address.parse().unwrap()]),
        &raw,
    )
    .await
    .unwrap();
    handle.join().unwrap();
    assert_eq!(fs::read(&raw).unwrap(), bytes);
    let flac = dir.path().join("audio.flac");
    transfer::ffmpeg(&raw, &flac, false).await.unwrap();
    verify_flac(&flac, 5.0).unwrap();
    let wav = dir.path().join("alarm.wav");
    transfer::ffmpeg(&flac, &wav, true).await.unwrap();
    let info = verify_wav(&wav, 5.0).unwrap();
    assert_eq!(
        (info.sample_rate, info.channels, info.bits_per_sample),
        (48000, 2, 16)
    );
    assert!(verify_flac(&flac, 240.0).is_err());
}
#[tokio::test]
async fn downloads_real_dash_aac_segments_and_decodes_the_full_track() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("audio.mpd");
    generate(
        &manifest,
        &[
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-f",
            "dash",
            "-seg_duration",
            "1",
        ],
    );
    let xml = fs::read_to_string(manifest)
        .unwrap()
        .replace("<Period", "<BaseURL>https://fixture.test/</BaseURL><Period");
    let urls = dash_urls(&xml, 5.0).unwrap();
    assert!(urls.len() > 2);
    let bodies: Vec<_> = urls
        .iter()
        .map(|url| fs::read(dir.path().join(url.path().trim_start_matches('/'))).unwrap())
        .collect();
    let (address, handle) = server(bodies.into_iter().map(|bytes| (200, bytes, None)).collect());
    let urls = urls
        .iter()
        .map(|url| format!("{address}{}", url.path()).parse().unwrap())
        .collect();
    let raw = dir.path().join("raw.mp4");
    transfer::fetch(&client(), &MediaPlan::Segments(urls), &raw)
        .await
        .unwrap();
    handle.join().unwrap();
    let flac = dir.path().join("audio.flac");
    transfer::ffmpeg(&raw, &flac, false).await.unwrap();
    let check = verify_flac(&flac, 5.0).unwrap();
    assert!((check.duration_seconds - 5.0).abs() < 0.1);
    let corrupt = dir.path().join("corrupt.media");
    fs::write(&corrupt, b"not audio").unwrap();
    assert!(
        transfer::ffmpeg(&corrupt, &dir.path().join("bad.flac"), false)
            .await
            .is_err()
    );
}
#[test]
fn saved_session_is_private_and_existing_output_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("session.json");
    let _lock = tidal_download::auth::lock_session(&session).unwrap();
    assert!(tidal_download::auth::lock_session(&session).is_err());
    let client = tidlers::TidalClient::new(&tidlers::auth::TidalAuth::with_access_token(
        "test-only".into(),
    ));
    tidal_download::auth::save(&client, &session).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&session).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let output = dir.path().join("sounds");
    fs::create_dir_all(output.join("123")).unwrap();
    fs::write(output.join("123/keep.txt"), b"original").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_tidal-download"))
        .args([
            "--session",
            dir.path().join("other.json").to_str().unwrap(),
            "download",
            "123",
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already exists"));
    assert_eq!(fs::read(output.join("123/keep.txt")).unwrap(), b"original");
}

#[tokio::test]
async fn decodes_high_resolution_flac_in_dash_fragments() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("audio.mpd");
    generate(
        &manifest,
        &[
            "-ar",
            "96000",
            "-c:a",
            "flac",
            "-strict",
            "experimental",
            "-f",
            "dash",
            "-seg_duration",
            "1",
        ],
    );
    let xml = fs::read_to_string(manifest)
        .unwrap()
        .replace("<Period", "<BaseURL>https://fixture.test/</BaseURL><Period");
    let urls = dash_urls(&xml, 5.0).unwrap();
    let mut bytes = Vec::new();
    for url in urls {
        bytes.extend(fs::read(dir.path().join(url.path().trim_start_matches('/'))).unwrap());
    }
    let raw = dir.path().join("raw.mp4");
    fs::write(&raw, bytes).unwrap();
    let flac = dir.path().join("audio.flac");
    transfer::ffmpeg(&raw, &flac, false).await.unwrap();
    let audio = verify_flac(&flac, 5.0).unwrap();
    assert_eq!(audio.sample_rate, 96000);
}

#[test]
fn offline_directory_verification_detects_a_modified_report_hash() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("audio.flac");
    generate(&audio, &["-c:a", "flac"]);
    let check = verify_flac(&audio, 5.0).unwrap();
    let mut report = serde_json::json!({"expected_seconds":5.0, "audio":check, "wav":null});
    fs::write(dir.path().join("report.json"), report.to_string()).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_tidal-download"))
            .args([
                "--session",
                "/nonexistent/session.json",
                "verify",
                dir.path().to_str().unwrap(),
            ])
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    report["audio"]["sha256"] = "wrong".into();
    fs::write(dir.path().join("report.json"), report.to_string()).unwrap();
    let result = run();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("hash differs"));
}
