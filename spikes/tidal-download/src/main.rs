use anyhow::{Context, Result, bail, ensure};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use reqwest::Url;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command as Process,
    time::Duration,
};
use tidal_download::{auth, manifest::Playback, transfer, verify_flac};

#[derive(Parser)]
#[command(version, about = "Download TIDAL audio for local/offline playback")]
struct Cli {
    #[arg(long, global = true, default_value = ".local/tidal/session.json")]
    session: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Sign in through your host browser and save a refreshable session.
    Login {
        /// Use upstream's device-code profile instead of the default PKCE profile.
        #[arg(long)]
        device: bool,
    },
    /// Remove the locally saved tokens (does not revoke the server-side session).
    Logout,
    /// Check the saved session against TIDAL.
    Status,
    /// Check Rust CLI, FFmpeg and saved-session availability without network requests.
    Doctor,
    /// Download one complete TIDAL track URL or numeric ID.
    Download {
        track: String,
        #[arg(long, value_enum, default_value = "auto")]
        quality: Quality,
        #[arg(long, default_value = ".local/sounds")]
        output: PathBuf,
        /// Also create alarm.wav: 48 kHz, 16-bit stereo PCM for the planned Tab5 output.
        #[arg(long)]
        wav: bool,
    },
    /// Decode a FLAC offline; for a download directory also check its recorded hash.
    Verify {
        path: PathBuf,
        #[arg(long)]
        expected_seconds: Option<f64>,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum Quality {
    Auto,
    Lossless,
    Hires,
    High,
}
impl Quality {
    fn candidates(self) -> &'static [&'static str] {
        match self {
            Self::Auto => &["LOSSLESS", "HIGH"],
            Self::Lossless => &["LOSSLESS"],
            Self::Hires => &["HI_RES_LOSSLESS"],
            Self::High => &["HIGH"],
        }
    }
}

fn track_id(value: &str) -> Result<u64> {
    let id = if value.bytes().all(|b| b.is_ascii_digit()) {
        value.to_owned()
    } else {
        let url = Url::parse(value).context("Use a TIDAL track URL or numeric track ID")?;
        ensure!(
            url.scheme() == "https"
                && matches!(
                    url.host_str(),
                    Some("tidal.com" | "www.tidal.com" | "listen.tidal.com")
                ),
            "Expected a tidal.com track URL"
        );
        let parts: Vec<_> = url
            .path_segments()
            .context("Invalid track URL")?
            .filter(|p| !p.is_empty())
            .collect();
        ensure!(
            parts.len() >= 2 && parts[parts.len() - 2] == "track",
            "Use a track URL, not an album, playlist or share short-link"
        );
        parts[parts.len() - 1].to_owned()
    };
    let id: u64 = id.parse().context("Invalid numeric track ID")?;
    ensure!(id > 0, "Track ID must be positive");
    Ok(id)
}

fn doctor(session: &Path) -> Result<()> {
    println!("tidal-download {}", env!("CARGO_PKG_VERSION"));
    let ffmpeg = Process::new("ffmpeg")
        .arg("-version")
        .output()
        .context("FFmpeg missing: rebuild the dev container")?;
    ensure!(ffmpeg.status.success(), "FFmpeg cannot start");
    println!(
        "{}",
        String::from_utf8_lossy(&ffmpeg.stdout)
            .lines()
            .next()
            .unwrap_or("FFmpeg available")
    );
    println!(
        "Saved session: {}\nSession present: {}",
        session.display(),
        session.is_file()
    );
    println!("No network requests made. Use `status` to check login or `login` to sign in.");
    Ok(())
}

async fn download(
    session: &Path,
    id: u64,
    quality: Quality,
    output: &Path,
    wav: bool,
) -> Result<()> {
    // Detect local setup problems before any account or media requests.
    let ffmpeg = Process::new("ffmpeg")
        .arg("-version")
        .output()
        .context("FFmpeg missing: rebuild the dev container")?;
    ensure!(ffmpeg.status.success(), "FFmpeg cannot start");
    fs::create_dir_all(output)?;
    let _output_lock = auth::lock_session(&output.join("download.json"))?;
    let final_dir = output.join(id.to_string());
    ensure!(
        !final_dir.exists(),
        "Track directory already exists; verify it or use --output with another directory"
    );
    let mut client = auth::load(session).await?;
    let http = transfer::http_client()?;
    let user = account(&http, &mut client, session).await?;
    let country = user["countryCode"]
        .as_str()
        .context("TIDAL session has no country code")?;
    let token = client
        .session
        .auth
        .access_token
        .as_deref()
        .context("Missing access token")?;
    let mut url = Url::parse(&format!("https://api.tidal.com/v1/tracks/{id}"))?;
    url.query_pairs_mut().append_pair("countryCode", country);
    let track = transfer::json(&http, &url, token).await?;
    ensure!(
        track["id"].as_u64() == Some(id),
        "Wrong track metadata returned"
    );
    let seconds = track["duration"]
        .as_f64()
        .context("Missing track duration")?;
    ensure!(
        seconds > 0.0 && seconds <= 86_400.0,
        "Invalid track duration"
    );
    let title = track["title"].as_str().unwrap_or("Unknown title");
    eprintln!("Selected: {title} ({seconds}s), account region {country}");
    let mut failures = Vec::new();
    for &requested in quality.candidates() {
        eprintln!("Trying {requested}...");
        let stage = tempfile::Builder::new()
            .prefix(".download-")
            .tempdir_in(output)?;
        let attempt = async {
            let mut url = Url::parse(&format!(
                "https://api.tidal.com/v1/tracks/{id}/playbackinfopostpaywall"
            ))?;
            url.query_pairs_mut().extend_pairs([
                ("countryCode", country),
                ("audioquality", requested),
                ("playbackmode", "STREAM"),
                ("assetpresentation", "FULL"),
            ]);
            let playback: Playback =
                serde_json::from_value(transfer::json(&http, &url, token).await?)
                    .context("Unexpected playback response")?;
            if !matches!(quality, Quality::Auto) {
                ensure!(
                    playback.audio_quality == requested,
                    "TIDAL returned a different quality than requested"
                );
            }
            let plan = playback.plan(id, seconds)?;
            let raw = stage.path().join("source.media");
            let bytes = transfer::fetch(&http, &plan, &raw).await?;
            let flac = stage.path().join("audio.flac");
            transfer::ffmpeg(&raw, &flac, false).await?;
            let audio = verify_flac(&flac, seconds)?;
            let wav_check = if wav {
                let path = stage.path().join("alarm.wav");
                transfer::ffmpeg(&flac, &path, true).await?;
                Some(tidal_download::verify_wav(&path, seconds)?)
            } else {
                None
            };
            fs::remove_file(raw)?;
            let lossy = matches!(playback.audio_quality.as_str(), "LOW" | "HIGH");
            let report = json!({
                "schema_version": 1, "status": "download_verified", "track_id": id,
                "title": title, "artist": track["artist"]["name"], "country": country,
                "expected_seconds": seconds, "requested_quality": requested,
                "source_quality": playback.audio_quality, "source_lossy": lossy,
                "source_bytes": bytes, "manifest_type": playback.manifest_mime_type,
                "audio": audio, "wav": wav_check,
                "backend": "tidlers-0.5.0-auth/legacy-v1-media",
                "offline_listening_test": "pending"
            });
            let mut report_file = fs::File::create(stage.path().join("report.json"))?;
            serde_json::to_writer_pretty(&mut report_file, &report)?;
            report_file.write_all(b"\n")?;
            report_file.sync_all()?;
            fs::File::open(&flac)?.sync_all()?;
            // Stage resides on the output filesystem. Only validated files are published.
            ensure!(
                !final_dir.exists(),
                "Output appeared during download; refusing to replace it"
            );
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                stage.path(),
                rustix::fs::CWD,
                &final_dir,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .context("Cannot publish track directory without replacing existing output")?;
            if lossy {
                eprintln!(
                    "Source is AAC/lossy. The FLAC output preserves decoded audio; it does not restore lost detail."
                );
            }
            println!(
                "Saved {}\nVerify offline: cargo run -- verify {}",
                final_dir.display(),
                final_dir.display()
            );
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(Duration::from_secs(1800), attempt).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => {
                eprintln!("{requested}: {e}");
                failures.push(format!("{requested}: {e}"));
            }
            Err(_) => failures.push(format!("{requested}: exceeded 30 minute limit")),
        }
    }
    bail!("No complete track was saved. {}", failures.join("; "))
}

async fn account(
    http: &reqwest::Client,
    client: &mut tidlers::TidalClient,
    path: &Path,
) -> Result<serde_json::Value> {
    let url = Url::parse("https://api.tidal.com/v1/sessions")?;
    let token = client
        .session
        .auth
        .access_token
        .as_deref()
        .context("Missing access token")?;
    match transfer::json(http, &url, token).await {
        Ok(value) => Ok(value),
        Err(_) => {
            auth::refresh(client, true).await?;
            auth::save(client, path)?;
            transfer::json(
                http,
                &url,
                client
                    .session
                    .auth
                    .access_token
                    .as_deref()
                    .context("Missing refreshed token")?,
            )
            .await
        }
    }
}

fn verify(path: &Path, expected: Option<f64>) -> Result<()> {
    if path.is_dir() {
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(path.join("report.json"))?)?;
        let seconds = report["expected_seconds"]
            .as_f64()
            .context("Report has no duration")?;
        let audio = verify_flac(&path.join("audio.flac"), expected.unwrap_or(seconds))?;
        ensure!(
            report["audio"]["sha256"].as_str() == Some(audio.sha256.as_str()),
            "FLAC hash differs from download report"
        );
        if !report["wav"].is_null() {
            let wav =
                tidal_download::verify_wav(&path.join("alarm.wav"), expected.unwrap_or(seconds))?;
            ensure!(
                report["wav"]["sha256"].as_str() == Some(wav.sha256.as_str()),
                "WAV hash differs from download report"
            );
        }
        println!(
            "Verified complete audio and recorded hashes: {}",
            path.display()
        );
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&verify_flac(
                path,
                expected.context("A standalone FLAC requires --expected-seconds")?
            )?)?
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Verify {
            path,
            expected_seconds,
        } => verify(&path, expected_seconds),
        Command::Doctor => doctor(&cli.session),
        command => {
            let _lock = auth::lock_session(&cli.session)?;
            match command {
                Command::Login { device } => auth::login(&cli.session, device).await,
                Command::Logout => {
                    match fs::remove_file(&cli.session) {
                        Ok(()) => (),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into()),
                    }
                    println!("Local session removed. This does not revoke the login at TIDAL.");
                    Ok(())
                }
                Command::Status => {
                    let mut client = auth::load(&cli.session).await?;
                    let value =
                        account(&transfer::http_client()?, &mut client, &cli.session).await?;
                    println!(
                        "Session accepted by TIDAL. Country: {}. Download entitlement still needs a track test.",
                        value["countryCode"].as_str().unwrap_or("unknown")
                    );
                    Ok(())
                }
                Command::Download {
                    track,
                    quality,
                    output,
                    wav,
                } => download(&cli.session, track_id(&track)?, quality, &output, wav).await,
                _ => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_track_links_only() {
        for input in [
            "123",
            "https://tidal.com/browse/track/123?u",
            "https://listen.tidal.com/track/123",
            "https://tidal.com/track/123/",
        ] {
            assert_eq!(track_id(input).unwrap(), 123);
        }
        for input in [
            "0",
            "https://evil.test/track/123",
            "https://tidal.com/album/123",
            "not-a-track",
        ] {
            assert!(track_id(input).is_err());
        }
    }
}
