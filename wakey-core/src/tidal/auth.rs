//! Login with a device code, and token refresh.
//!
//! 1. [`device_authorization`] → [`parse_device_authorization`]: a short
//!    code and a link (`link.tidal.com/XXXXX`), shown on the clock; the user
//!    opens it on the phone and confirms.
//! 2. Every [`DeviceCode::interval`] seconds [`poll`] → [`parse_poll`]
//!    until [`Poll::Done`] (HTTP 400 `authorization_pending` meanwhile).
//! 3. Before each use, [`needs_refresh`]; then [`refresh`] →
//!    [`parse_refresh`] (the refresh token is replaced only if a new one
//!    comes back).
//!
//! The device authorization sends only the client id; the polling sends the
//! secret in the form; the refresh uses HTTP Basic with both.

use super::{Request, Result, ensure, err, expect_ok, json, status_error};
use crate::config::{TidalConfig, TidalSession};
use base64::Engine;
use serde::Deserialize;

const OAUTH: &str = "https://auth.tidal.com/v1/oauth2";
const SCOPE: &str = "r_usr w_usr w_sub";
/// Refresh this long before the access token expires.
const REFRESH_MARGIN_S: i64 = 60;

/// The code the user confirms, and how to poll for the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    /// For the polling (secret: not shown).
    pub device_code: String,
    /// Shown on the clock.
    pub user_code: String,
    /// The link with the code in it, with `https://`.
    pub url: String,
    /// Seconds the code stays valid.
    pub expires_in: u64,
    /// Seconds between polls.
    pub interval: u64,
}

pub fn device_authorization(cfg: &TidalConfig) -> Request {
    Request::post_form(
        format!("{OAUTH}/device_authorization"),
        &[("client_id", &cfg.client_id), ("scope", SCOPE)],
    )
}

pub fn parse_device_authorization(status: u16, body: &[u8]) -> Result<DeviceCode> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer {
        device_code: String,
        user_code: String,
        verification_uri_complete: String,
        expires_in: u64,
        interval: u64,
    }
    expect_ok("login code", status)?;
    let a: Answer = json("login code", body)?;
    let url = if a.verification_uri_complete.contains("://") {
        a.verification_uri_complete
    } else {
        format!("https://{}", a.verification_uri_complete)
    };
    Ok(DeviceCode {
        device_code: a.device_code,
        user_code: a.user_code,
        url,
        expires_in: a.expires_in,
        interval: a.interval.clamp(1, 60),
    })
}

pub fn poll(cfg: &TidalConfig, code: &DeviceCode) -> Request {
    Request::post_form(
        format!("{OAUTH}/token"),
        &[
            ("client_id", &cfg.client_id),
            ("client_secret", &cfg.client_secret),
            ("device_code", &code.device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("scope", SCOPE),
        ],
    )
}

/// The state of a device code login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Not confirmed yet: poll again after the interval.
    Pending,
    /// Poll less often (interval + 5 s).
    SlowDown,
    Done(TidalSession),
    /// The code ran out: start again.
    Expired,
    /// Refused, or another error (the error code, no description).
    Failed(String),
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    user: Option<User>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    country_code: Option<String>,
}

impl TokenAnswer {
    /// The user's id, at the top level or in `user`.
    fn user_id(&self) -> Option<u64> {
        self.user_id
            .or_else(|| self.user.as_ref().and_then(|u| u.user_id))
    }

    fn country_code(&self) -> Option<String> {
        self.user.as_ref().and_then(|u| u.country_code.clone())
    }
}

pub fn parse_poll(status: u16, body: &[u8], now: i64) -> Result<Poll> {
    #[derive(Deserialize)]
    struct Refusal {
        error: String,
    }
    match status {
        200 => {
            let t: TokenAnswer = json("login", body)?;
            ensure!(!t.access_token.is_empty(), "login: no access token");
            ensure!(t.expires_in > 0, "login: no token lifetime");
            let (Some(user_id), Some(country_code)) = (t.user_id(), t.country_code()) else {
                return Err(err("login: no user or country"));
            };
            Ok(Poll::Done(TidalSession {
                access_token: t.access_token,
                refresh_token: t.refresh_token.unwrap_or_default(),
                expires_at: now + t.expires_in,
                user_id,
                country_code,
            }))
        }
        400 => {
            let r: Refusal = json("login", body)?;
            Ok(match r.error.as_str() {
                "authorization_pending" => Poll::Pending,
                "slow_down" => Poll::SlowDown,
                "expired_token" => Poll::Expired,
                other => Poll::Failed(other.to_string()),
            })
        }
        _ => Err(status_error("login", status)),
    }
}

/// True if the access token expires within a minute (or has).
pub fn needs_refresh(session: &TidalSession, now: i64) -> bool {
    session.expires_at <= now + REFRESH_MARGIN_S
}

pub fn refresh(cfg: &TidalConfig, session: &TidalSession) -> Request {
    let basic = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", cfg.client_id, cfg.client_secret));
    Request::post_form(
        format!("{OAUTH}/token"),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &session.refresh_token),
        ],
    )
    .header("Authorization", format!("Basic {basic}"))
}

/// The refreshed session: new access token and expiry; the refresh token,
/// user and country stay unless the answer brings new ones.
pub fn parse_refresh(
    status: u16,
    body: &[u8],
    old: &TidalSession,
    now: i64,
) -> Result<TidalSession> {
    expect_ok("token refresh", status)?;
    let t: TokenAnswer = json("token refresh", body)?;
    ensure!(!t.access_token.is_empty(), "token refresh: no access token");
    ensure!(t.expires_in > 0, "token refresh: no token lifetime");
    let user_id = t.user_id().unwrap_or(old.user_id);
    let country_code = t.country_code().unwrap_or_else(|| old.country_code.clone());
    Ok(TidalSession {
        access_token: t.access_token,
        refresh_token: t
            .refresh_token
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| old.refresh_token.clone()),
        expires_at: now + t.expires_in,
        user_id,
        country_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tidal::Method;

    fn cfg() -> TidalConfig {
        TidalConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            ..TidalConfig::default()
        }
    }

    fn session() -> TidalSession {
        TidalSession {
            access_token: "a1".into(),
            refresh_token: "r1".into(),
            expires_at: 1000,
            user_id: 7,
            country_code: "CH".into(),
        }
    }

    #[test]
    fn device_code_request_and_answer() {
        let r = device_authorization(&cfg());
        assert_eq!(r.method, Method::Post);
        assert_eq!(
            r.url,
            "https://auth.tidal.com/v1/oauth2/device_authorization"
        );
        assert_eq!(r.body.unwrap(), "client_id=id&scope=r_usr%20w_usr%20w_sub");
        let body = br#"{"deviceCode":"dc","userCode":"ABCDE","verificationUri":"link.tidal.com",
            "verificationUriComplete":"link.tidal.com/ABCDE","expiresIn":300,"interval":2}"#;
        let c = parse_device_authorization(200, body).unwrap();
        assert_eq!(c.user_code, "ABCDE");
        assert_eq!(c.url, "https://link.tidal.com/ABCDE");
        assert_eq!((c.expires_in, c.interval), (300, 2));
    }

    #[test]
    fn polling_states() {
        let pending = br#"{"status":400,"error":"authorization_pending","sub_status":1002,"error_description":"x"}"#;
        assert_eq!(parse_poll(400, pending, 0).unwrap(), Poll::Pending);
        assert_eq!(
            parse_poll(400, br#"{"error":"expired_token"}"#, 0).unwrap(),
            Poll::Expired
        );
        assert_eq!(
            parse_poll(400, br#"{"error":"access_denied"}"#, 0).unwrap(),
            Poll::Failed("access_denied".into())
        );
        let done = br#"{"access_token":"a","refresh_token":"r","token_type":"Bearer",
            "expires_in":3600,"user_id":7,"user":{"userId":7,"countryCode":"CH"}}"#;
        let Poll::Done(s) = parse_poll(200, done, 100).unwrap() else {
            panic!()
        };
        assert_eq!(s.expires_at, 3700);
        assert_eq!((s.user_id, s.country_code.as_str()), (7, "CH"));
        assert!(parse_poll(401, b"", 0).is_err());
    }

    #[test]
    fn refresh_keeps_what_does_not_come_back() {
        let r = refresh(&cfg(), &session());
        assert!(
            r.headers
                .contains(&("Authorization", "Basic aWQ6c2VjcmV0".into()))
        );
        assert_eq!(r.body.unwrap(), "grant_type=refresh_token&refresh_token=r1");
        let s = parse_refresh(
            200,
            br#"{"access_token":"a2","expires_in":100}"#,
            &session(),
            50,
        )
        .unwrap();
        assert_eq!(s.access_token, "a2");
        assert_eq!(s.refresh_token, "r1");
        assert_eq!(s.expires_at, 150);
        assert_eq!(s.country_code, "CH");
        let s = parse_refresh(
            200,
            br#"{"access_token":"a3","refresh_token":"r2","expires_in":100}"#,
            &session(),
            0,
        )
        .unwrap();
        assert_eq!(s.refresh_token, "r2");
    }

    #[test]
    fn refresh_is_due_a_minute_before_expiry() {
        assert!(!needs_refresh(&session(), 939));
        assert!(needs_refresh(&session(), 940));
    }
}
