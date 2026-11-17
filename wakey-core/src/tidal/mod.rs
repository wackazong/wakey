//! TIDAL, the songs the alarms play: the user picks them in a playlist in
//! the TIDAL app; the clock logs in with a device code, syncs that playlist
//! and downloads the tracks as FLAC to the SD card.
//!
//! This module does no I/O. It describes each HTTP request ([`Request`])
//! and reads the responses (status and body); the firmware sends them over
//! Wi-Fi. The protocol is the one of TIDAL's apps; it is unofficial and
//! undocumented (see the `tidlers` crate):
//!
//! - [`auth`]: device code login, token refresh.
//! - [`api`]: playlists, their tracks, cover URLs, the stream manifest.
//! - [`cover`]: album covers, decoded to the size the list shows.
//! - [`manifest`]: BTS (mirrors of one file) and DASH (segments).
//! - [`flac_mp4`]: DASH lossless is FLAC in fragmented MP4; unpacked to a
//!   plain `.flac` while it downloads.
//! - [`sync`]: what to download and delete on the SD card.
//!
//! Error texts never contain response bodies or URLs: those can carry
//! tokens.

pub mod api;
pub mod auth;
pub mod cover;
pub mod flac_mp4;
pub mod manifest;
pub mod sync;

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// An HTTP request for the firmware to send (HTTPS; no redirects needed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    /// `application/x-www-form-urlencoded` when set.
    pub body: Option<String>,
}

impl Request {
    fn get(url: String) -> Self {
        Self {
            method: Method::Get,
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    fn post_form(url: String, fields: &[(&str, &str)]) -> Self {
        Self {
            method: Method::Post,
            url,
            headers: vec![(
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            )],
            body: Some(form(fields)),
        }
    }

    fn header(mut self, name: &'static str, value: String) -> Self {
        self.headers.push((name, value));
        self
    }
}

/// What went wrong, in words for the log or the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err($crate::tidal::Error(format!($($msg)+)));
        }
    };
}
pub(crate) use ensure;

fn err(msg: impl Into<String>) -> Error {
    Error(msg.into())
}

/// An error unless the HTTP status is 200.
fn expect_ok(stage: &str, status: u16) -> Result<()> {
    if status == 200 {
        Ok(())
    } else {
        Err(status_error(stage, status))
    }
}

/// An unexpected HTTP status, with a hint (no body: it may carry tokens).
fn status_error(stage: &str, status: u16) -> Error {
    let hint = match status {
        401 => "not logged in (log in again)",
        403 => "not allowed for this account or quality",
        404 => "not found",
        429 => "too many requests (try later)",
        500..=599 => "TIDAL server error (try later)",
        _ => "unexpected answer",
    };
    Error(format!("{stage}: HTTP {status}, {hint}"))
}

/// Parses a JSON body; errors name the stage, not the content.
fn json<T: serde::de::DeserializeOwned>(stage: &str, body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|_| Error(format!("{stage}: unexpected answer")))
}

/// Percent-encodes a form or query value (RFC 3986 unreserved kept).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding() {
        assert_eq!(
            form(&[("scope", "r_usr w_usr"), ("a", "x/y+z")]),
            "scope=r_usr%20w_usr&a=x%2Fy%2Bz"
        );
    }

    #[test]
    fn status_errors_carry_no_body() {
        assert_eq!(
            status_error("playlist", 401).0,
            "playlist: HTTP 401, not logged in (log in again)"
        );
    }
}
