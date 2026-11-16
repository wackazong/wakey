//! HTTPS requests over the Wi-Fi session (ESP-IDF's HTTP client; servers
//! checked against the certificate bundle). Used inside a
//! [`crate::net::Wifi::run`] job, while the station is connected.
//!
//! **Connections are kept** (HTTP keep-alive): one per host, in a small
//! pool of the calling thread (the `wifi` thread runs all TIDAL jobs), so
//! only the first request to a host pays for the TLS handshake. A
//! connection goes back to the pool only after a response read to its end;
//! one that failed or was left mid-body (a download stopped) is closed. A
//! request on a kept connection the server has closed meanwhile is tried
//! once more on a new one. [`close_all`] at the end of a Wi-Fi session
//! closes the connections but keeps the clients: their TLS session tickets
//! make the next session's handshakes short.

use esp_idf_svc::http::Method as EspMethod;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use std::cell::RefCell;
use std::time::{Duration, Instant};
use wakey_core::tidal::{Method, Request};

/// Largest body read into memory (API answers; downloads stream).
const MAX_BODY: usize = 2 << 20;
/// Connections kept (TIDAL: auth, api, resources and one CDN host).
const POOL_MAX: usize = 4;

thread_local! {
    // Kept connections by origin (`https://host`), most recent last.
    // Already `const`: the lint misfires on it.
    #[allow(clippy::missing_const_for_thread_local)]
    static POOL: RefCell<Vec<(String, EspHttpConnection)>> = const { RefCell::new(Vec::new()) };
}

fn connection() -> anyhow::Result<EspHttpConnection> {
    Ok(EspHttpConnection::new(&Configuration {
        // A larger receive buffer for bulk data (4 KiB gave ~186 KiB/s).
        buffer_size: Some(16 * 1024),
        buffer_size_tx: Some(2048),
        timeout: Some(Duration::from_secs(30)),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        // The next connection resumes the TLS session (~1.5 s handshake).
        save_client_session: true,
        ..Default::default()
    })?)
}

/// `scheme://host[:port]` of `url` (where a connection can be reused).
fn origin(url: &str) -> &str {
    let rest = url.find("://").map_or(0, |i| i + 3);
    let end = url[rest..].find('/').map_or(url.len(), |i| rest + i);
    &url[..end]
}

/// The host of `url`, for the log (paths and queries may carry tokens).
fn host(url: &str) -> &str {
    let o = origin(url);
    o.find("://").map_or(o, |i| &o[i + 3..])
}

/// A kept connection to `origin`, or a new one; true if kept.
fn take(origin: &str) -> anyhow::Result<(EspHttpConnection, bool)> {
    let kept = POOL.with(|p| {
        let mut pool = p.borrow_mut();
        let i = pool.iter().position(|(o, _)| o == origin)?;
        Some(pool.remove(i).1)
    });
    match kept {
        Some(c) => Ok((c, true)),
        None => Ok((connection()?, false)),
    }
}

/// Keeps `conn` (its response read to the end) for the next request to
/// `origin`; the oldest kept one goes if the pool is full.
fn give(origin: &str, conn: EspHttpConnection) {
    POOL.with(|p| {
        let mut pool = p.borrow_mut();
        if pool.len() >= POOL_MAX {
            pool.remove(0);
        }
        pool.push((origin.to_string(), conn));
    });
}

/// Closes the kept connections (the Wi-Fi session ends). The clients stay
/// with their TLS session tickets: the next session's first request to a
/// host resumes instead of a full handshake.
pub fn close_all() {
    POOL.with(|p| {
        for (_, conn) in p.borrow_mut().iter_mut() {
            conn.close();
        }
    });
}

fn method_name(m: Method) -> &'static str {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
    }
}

/// Sends `req`; the status and the body. Errors never include the URL or
/// the body (tokens).
pub fn send(req: &Request) -> anyhow::Result<(u16, Vec<u8>)> {
    let origin = origin(&req.url);
    let t = Instant::now();
    let mut retried = false;
    loop {
        let (mut conn, kept) = take(origin)?;
        match exchange(&mut conn, req) {
            Ok((status, body, headers_ms)) => {
                log::info!(
                    "http: {} {} → {status}, {} bytes in {} ms (answer after {headers_ms} ms){}",
                    method_name(req.method),
                    host(&req.url),
                    body.len(),
                    t.elapsed().as_millis(),
                    if kept { " (kept connection)" } else { "" }
                );
                give(origin, conn);
                return Ok((status, body));
            }
            // The server closed the kept connection meanwhile.
            Err(e) if kept && !retried => {
                log::info!(
                    "http: {}: kept connection gone ({e}), new one",
                    host(&req.url)
                );
                retried = true;
            }
            Err(e) => return Err(e),
        }
    }
}

/// One request and its whole answer on `conn`; also when the answer began
/// (ms, for the log: connection and server).
fn exchange(conn: &mut EspHttpConnection, req: &Request) -> anyhow::Result<(u16, Vec<u8>, u128)> {
    let t = Instant::now();
    let len = req.body.as_ref().map(|b| b.len().to_string());
    let mut headers: Vec<(&str, &str)> =
        req.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
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
            let n = conn
                .write(rest)
                .map_err(|e| anyhow::anyhow!("HTTPS send: {e}"))?;
            anyhow::ensure!(n > 0, "HTTPS send: connection closed");
            rest = &rest[n..];
        }
    }
    conn.initiate_response()
        .map_err(|e| anyhow::anyhow!("HTTPS answer: {e}"))?;
    let headers_ms = t.elapsed().as_millis();
    let status = conn.status();
    let mut body = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        let n = conn
            .read(&mut buf)
            .map_err(|e| anyhow::anyhow!("HTTPS read: {e}"))?;
        if n == 0 {
            break;
        }
        anyhow::ensure!(body.len() + n <= MAX_BODY, "HTTPS answer too large");
        body.extend_from_slice(&buf[..n]);
    }
    Ok((status, body, headers_ms))
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
    let origin = origin(url);
    let t = Instant::now();
    let mut retried = false;
    loop {
        let (mut conn, kept) = take(origin)?;
        let mut delivered = false;
        let mut counting = |b: &[u8]| {
            delivered = true;
            sink(b)
        };
        let result = fetch(&mut conn, url, range, &mut counting, progress);
        match result {
            Ok(total) => {
                log::info!(
                    "http: GET {} → {} KiB in {} ms{}",
                    host(url),
                    total / 1024,
                    t.elapsed().as_millis(),
                    if kept { " (kept connection)" } else { "" }
                );
                give(origin, conn);
                return Ok(total);
            }
            // The server closed the kept connection before any data.
            Err(e) if kept && !retried && !delivered => {
                log::info!("http: {}: kept connection gone ({e}), new one", host(url));
                retried = true;
            }
            // `conn` is dropped: closed (it may be mid-body).
            Err(e) => return Err(e),
        }
    }
}

/// One download on `conn`, read to its end.
fn fetch(
    conn: &mut EspHttpConnection,
    url: &str,
    range: Option<&str>,
    sink: &mut dyn FnMut(&[u8]) -> anyhow::Result<()>,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> anyhow::Result<u64> {
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
        let n = conn
            .read(&mut buf)
            .map_err(|e| anyhow::anyhow!("download read: {e}"))?;
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
