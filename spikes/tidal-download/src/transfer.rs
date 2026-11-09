use crate::manifest::MediaPlan;
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Response, Url};
use serde_json::Value;
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};

pub const MAX_BYTES: u64 = 512 * 1024 * 1024;

pub fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()?)
}

async fn response(client: &Client, url: &Url, token: Option<&str>) -> Result<Response> {
    for attempt in 0..3 {
        let mut request = client.get(url.clone());
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let result = request.send().await;
        match result {
            Ok(resp) if resp.status() == reqwest::StatusCode::OK => return Ok(resp),
            Ok(resp) => {
                let status = resp.status();
                if attempt == 2 || !(status.is_server_error() || status.as_u16() == 429) {
                    bail!(
                        "HTTP {status}; 401: run login again; 403: account/client/quality access; 404: track unavailable"
                    );
                }
                let delay = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1 << attempt)
                    .min(15);
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            Err(_) if attempt < 2 => tokio::time::sleep(Duration::from_secs(1 << attempt)).await,
            Err(_) => bail!("Network request failed after three attempts (URL omitted)"),
        }
    }
    unreachable!()
}

pub async fn json(client: &Client, url: &Url, token: &str) -> Result<Value> {
    let mut resp = response(client, url, Some(token)).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("API response interrupted"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= 2 * 1024 * 1024,
            "API response exceeds 2 MiB"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Unexpected TIDAL JSON response")
}

async fn one(client: &Client, url: &Url, file: &mut File, start: u64) -> Result<u64> {
    let mut last = None;
    for attempt in 0..3 {
        file.set_len(start)?;
        file.seek(SeekFrom::Start(start))?;
        let result = async {
            // CDN requests never contain the user's bearer token.
            let mut resp = response(client, url, None).await?;
            let expected = resp.content_length();
            ensure!(
                expected.is_none_or(|n| n <= MAX_BYTES - start),
                "Audio exceeds 512 MiB"
            );
            let mut written = 0;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("Media body interrupted"))?
            {
                written += chunk.len() as u64;
                ensure!(start + written <= MAX_BYTES, "Audio exceeds 512 MiB");
                file.write_all(&chunk)?;
            }
            ensure!(
                written > 0 && expected.is_none_or(|n| n == written),
                "Empty or incomplete media body"
            );
            Ok(written)
        }
        .await;
        match result {
            Ok(n) => return Ok(n),
            Err(e) => last = Some(e),
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
        }
    }
    Err(last.unwrap())
}

pub async fn fetch(client: &Client, plan: &MediaPlan, path: &Path) -> Result<u64> {
    let mut file = File::create(path)?;
    let total = match plan {
        MediaPlan::Mirrors(urls) => {
            let mut total = None;
            let mut last = None;
            for url in urls {
                match one(client, url, &mut file, 0).await {
                    Ok(n) => {
                        total = Some(n);
                        break;
                    }
                    Err(e) => last = Some(e),
                }
            }
            total.ok_or_else(|| last.unwrap_or_else(|| anyhow::anyhow!("No media mirrors")))?
        }
        MediaPlan::Segments(urls) => {
            let mut total = 0;
            for (index, url) in urls.iter().enumerate() {
                total += one(client, url, &mut file, total).await?;
                if index % 25 == 0 || index + 1 == urls.len() {
                    eprintln!("Downloaded segment {}/{}", index + 1, urls.len());
                }
            }
            total
        }
    };
    file.sync_all()?;
    Ok(total)
}

pub async fn ffmpeg(input: &Path, output: &Path, wav: bool) -> Result<()> {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-xerror",
        "-err_detect",
        "explode",
        "-protocol_whitelist",
        "file",
        "-i",
    ])
    .arg(input)
    .args(["-map", "0:a:0", "-vn", "-map_metadata", "-1"]);
    if wav {
        cmd.args(["-c:a", "pcm_s16le", "-ar", "48000", "-ac", "2", "-f", "wav"]);
    } else {
        cmd.args(["-c:a", "flac", "-f", "flac"]);
    }
    cmd.args(["-fs", &MAX_BYTES.to_string(), "-y"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .context("FFmpeg is missing; rebuild the dev container or install ffmpeg")?;
    let status = tokio::time::timeout(Duration::from_secs(300), child.wait())
        .await
        .context("Audio conversion exceeded five minutes")??;
    ensure!(
        status.success(),
        "FFmpeg could not decode the complete audio; encrypted, corrupt, or unsupported media"
    );
    Ok(())
}
