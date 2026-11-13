//! HTTPS requests over the Wi-Fi session (ESP-IDF's HTTP client; servers
//! checked against the certificate bundle). Used inside a
//! [`crate::net::Wifi::run`] job, while the station is connected.

use esp_idf_svc::http::Method as EspMethod;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use std::time::Duration;
use wakey_core::tidal::{Method, Request};

/// Largest body read into memory (API answers; downloads stream).
const MAX_BODY: usize = 2 << 20;

fn connection(timeout: Duration, rx_buffer: usize) -> anyhow::Result<EspHttpConnection> {
    Ok(EspHttpConnection::new(&Configuration {
        buffer_size: Some(rx_buffer),
        buffer_size_tx: Some(2048),
        timeout: Some(timeout),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    })?)
}

/// Sends `req`; the status and the body. Errors never include the URL or
/// the body (tokens).
pub fn send(req: &Request) -> anyhow::Result<(u16, Vec<u8>)> {
    let mut conn = connection(Duration::from_secs(30), 4096)?;
    let len = req.body.as_ref().map(|b| b.len().to_string());
    let mut headers: Vec<(&str, &str)> = req.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    if let Some(len) = &len {
        headers.push(("Content-Length", len));
    }
    let method = match req.method {
        Method::Get => EspMethod::Get,
        Method::Post => EspMethod::Post,
    };
    conn.initiate_request(method, &req.url, &headers)
        .map_err(|e| anyhow::anyhow!("HTTPS request: {e}"))?;
    if let Some(body) = &req.body {
        let mut rest = body.as_bytes();
        while !rest.is_empty() {
            let n = conn.write(rest).map_err(|e| anyhow::anyhow!("HTTPS send: {e}"))?;
            anyhow::ensure!(n > 0, "HTTPS send: connection closed");
            rest = &rest[n..];
        }
    }
    conn.initiate_response()
        .map_err(|e| anyhow::anyhow!("HTTPS answer: {e}"))?;
    let status = conn.status();
    let mut body = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        let n = conn.read(&mut buf).map_err(|e| anyhow::anyhow!("HTTPS read: {e}"))?;
        if n == 0 {
            break;
        }
        anyhow::ensure!(body.len() + n <= MAX_BODY, "HTTPS answer too large");
        body.extend_from_slice(&buf[..n]);
    }
    Ok((status, body))
}

/// Downloads `url` (a CDN, no authorization), handing the body to `sink`
/// in pieces as it arrives; the number of bytes. `progress` hears the bytes
/// so far and the Content-Length, if sent (which must then be met). Only
/// 200 counts.
pub fn download(
    url: &str,
    sink: &mut dyn FnMut(&[u8]) -> anyhow::Result<()>,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> anyhow::Result<u64> {
    download_range(url, None, sink, progress)
}

/// [`download`], of a byte range if `range` is given (`Range: bytes=…`,
/// e.g. `0-1023`; then only 206 counts).
pub fn download_range(
    url: &str,
    range: Option<&str>,
    sink: &mut dyn FnMut(&[u8]) -> anyhow::Result<()>,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> anyhow::Result<u64> {
    // A larger receive buffer for bulk data (4 KiB gave ~186 KiB/s).
    let mut conn = connection(Duration::from_secs(60), 16 * 1024)?;
    let range = range.map(|r| format!("bytes={r}"));
    let headers: Vec<(&str, &str)> = range.iter().map(|r| ("Range", r.as_str())).collect();
    conn.initiate_request(EspMethod::Get, url, &headers)
        .map_err(|e| anyhow::anyhow!("download request: {e}"))?;
    conn.initiate_response()
        .map_err(|e| anyhow::anyhow!("download answer: {e}"))?;
    let status = conn.status();
    let ok = if range.is_some() { 206 } else { 200 };
    anyhow::ensure!(status == ok, "download: HTTP {status}");
    let expected: Option<u64> = conn.header("Content-Length").and_then(|v| v.parse().ok());
    let mut buf = vec![0u8; 16 * 1024];
    let mut total = 0u64;
    loop {
        let n = conn.read(&mut buf).map_err(|e| anyhow::anyhow!("download read: {e}"))?;
        if n == 0 {
            break;
        }
        sink(&buf[..n])?;
        total += n as u64;
        progress(total, expected);
    }
    if let Some(len) = expected {
        anyhow::ensure!(total == len, "download cut off ({total} of {len} bytes)");
    }
    anyhow::ensure!(total > 0, "download empty");
    Ok(total)
}
