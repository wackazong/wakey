//! Talking to TIDAL's API: requests, the session (refresh, binding), the
//! device code login, and what can go wrong.

use super::{State, Tidal};
use crate::config::{self, TidalConfig, TidalSession};
use crate::http;
use crate::net::Unavailable;
use std::time::{Duration, Instant};
use wakey_core::tidal::Request;
use wakey_core::tidal::api;
use wakey_core::tidal::auth::{self, Poll};

/// Longest a login code is polled for (TIDAL's are valid for ~5 min).
const LOGIN_MAX: Duration = Duration::from_secs(600);

/// What went wrong in a TIDAL job. Details go to the log; the TIDAL tab
/// shows a [`Problem`].
#[derive(Debug)]
pub enum Error {
    /// TIDAL rejected the session (401): it is dropped, a login follows.
    SessionInvalid,
    /// TIDAL wants `/sessions` called for this token first (401, subStatus
    /// 6004): bound, then the request again (not a bad session).
    Unbound,
    /// A download stopped for another track or for AirDrop; it goes on
    /// later.
    Deferred,
    NotLoggedIn,
    /// No playlist by the configured name.
    NoPlaylist(String),
    /// The login code ran out before the user confirmed it.
    CodeExpired,
    /// TIDAL refused the login (its error code).
    LoginRefused(String),
    /// No connection to TIDAL, or the connection broke.
    Http(anyhow::Error),
    /// An answer that is not what the protocol expects.
    Api(wakey_core::tidal::Error),
    /// Reading or writing the card.
    Storage(std::io::Error),
    /// A downloaded file is not a usable FLAC.
    Unusable(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::SessionInvalid => f.write_str("session rejected (401)"),
            Error::Unbound => f.write_str("API session wanted (401, 6004)"),
            Error::Deferred => f.write_str("deferred"),
            Error::NotLoggedIn => f.write_str("not logged in"),
            Error::NoPlaylist(name) => write!(f, "no playlist \"{name}\""),
            Error::CodeExpired => f.write_str("login code expired"),
            Error::LoginRefused(code) => write!(f, "login refused ({code})"),
            Error::Http(e) => write!(f, "{e:#}"),
            Error::Api(e) => write!(f, "{e}"),
            Error::Storage(e) => write!(f, "card: {e}"),
            Error::Unusable(why) => write!(f, "file unusable: {why}"),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Http(e)
    }
}

impl From<wakey_core::tidal::Error> for Error {
    fn from(e: wakey_core::tidal::Error) -> Self {
        Error::Api(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Storage(e)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// What the TIDAL tab says after a failure (the app words it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// No Wi-Fi connection for the job.
    Network(Unavailable),
    /// TIDAL rejected even a fresh session.
    Session,
    NoPlaylist(String),
    CodeExpired,
    LoginRefused,
    /// TIDAL could not be reached.
    Unreachable,
    /// TIDAL answered something unexpected.
    Server,
    /// The card could not be written.
    Storage,
}

impl Error {
    pub fn problem(&self) -> Problem {
        match self {
            Error::SessionInvalid | Error::Unbound | Error::NotLoggedIn => Problem::Session,
            Error::NoPlaylist(name) => Problem::NoPlaylist(name.clone()),
            Error::CodeExpired => Problem::CodeExpired,
            Error::LoginRefused(_) => Problem::LoginRefused,
            Error::Http(_) | Error::Deferred => Problem::Unreachable,
            Error::Api(_) | Error::Unusable(_) => Problem::Server,
            Error::Storage(_) => Problem::Storage,
        }
    }
}

/// Sends `req`. On an error status logs TIDAL's reason (only `subStatus`
/// and `userMessage`: the rest of a body may carry tokens); a 401 is
/// [`Error::Unbound`] or [`Error::SessionInvalid`].
fn send(req: &Request, stage: &str) -> Result<(u16, Vec<u8>)> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Reason {
        sub_status: Option<i64>,
        user_message: Option<String>,
    }
    let (status, body) = http::send(req)?;
    if status >= 400 {
        let reason = serde_json::from_slice::<Reason>(&body).ok();
        match &reason {
            Some(r) => log::warn!(
                "tidal: {stage}: HTTP {status}, subStatus {:?}, {:?}",
                r.sub_status,
                r.user_message
            ),
            None => log::warn!("tidal: {stage}: HTTP {status} ({} bytes)", body.len()),
        }
        if status == 401 {
            let unbound = reason.is_some_and(|r| r.sub_status == Some(6004));
            return Err(if unbound {
                Error::Unbound
            } else {
                Error::SessionInvalid
            });
        }
    }
    Ok((status, body))
}

impl Tidal {
    /// The session, refreshed (and saved) if its token is about to expire.
    pub(super) fn session(&self) -> Result<TidalSession> {
        let (cfg, session) = {
            let inner = self.lock();
            let s = inner.cfg.session.clone().ok_or(Error::NotLoggedIn)?;
            (inner.cfg.clone(), s)
        };
        let now = crate::clock::moment().unix;
        if !auth::needs_refresh(&session, now) {
            return Ok(session);
        }
        let (status, body) = send(&auth::refresh(&cfg, &session), "refresh")?;
        let fresh = auth::parse_refresh(status, &body, &session, now)?;
        config::save_tidal_session(Some(&fresh))?;
        log::info!("tidal: token refreshed");
        self.lock().cfg.session = Some(fresh.clone());
        Ok(fresh)
    }

    /// Binds the API session to the token (`/sessions`), once per token.
    fn bind(&self, s: &TidalSession) -> Result<()> {
        if self.lock().bound.as_ref() == Some(&s.access_token) {
            return Ok(());
        }
        let (status, body) = send(&api::sessions(s), "session")?;
        api::parse_sessions(status, &body)?;
        self.lock().bound = Some(s.access_token.clone());
        Ok(())
    }

    /// An API call for `s`: bound first; if TIDAL still wants the binding
    /// (6004), bound again and the call repeated once.
    pub(super) fn call(
        &self,
        s: &TidalSession,
        req: &Request,
        stage: &str,
    ) -> Result<(u16, Vec<u8>)> {
        self.bind(s)?;
        match send(req, stage) {
            Err(Error::Unbound) => {
                log::info!("tidal: {stage}: API session wanted, binding again");
                self.lock().bound = None;
                self.bind(s)?;
                send(req, stage)
            }
            result => result,
        }
    }

    /// The configured playlist's id, found by name among the user's.
    pub(super) fn find_playlist(&self, s: &TidalSession) -> Result<String> {
        let name = self.lock().cfg.playlist.clone();
        let mut lists = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) = self.call(s, &api::playlists(s, o), "playlists")?;
            let (page, next) = api::parse_playlists(status, &body, o)?;
            lists.extend(page);
            offset = next;
        }
        let uuid = api::find_playlist(&lists, &name)
            .ok_or_else(|| Error::NoPlaylist(name.clone()))?
            .uuid
            .clone();
        log::info!("tidal: playlist „{name}“ found");
        Ok(uuid)
    }

    /// The device code login, on the `wifi` thread. True if logged in
    /// (false: the tab was closed).
    pub(super) fn login(&self, cfg: &TidalConfig) -> Result<bool> {
        let (status, body) = http::send(&auth::device_authorization(cfg))?;
        let code = auth::parse_device_authorization(status, &body)?;
        log::info!(
            "tidal: login code {} (valid {} s)",
            code.user_code,
            code.expires_in
        );
        self.lock().state = State::Code {
            url: code.url.clone(),
            code: code.user_code.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(code.expires_in).min(LOGIN_MAX);
        let mut interval = Duration::from_secs(code.interval);
        loop {
            std::thread::sleep(interval);
            if !self.lock().wanted {
                self.lock().state = State::LoggedOut;
                return Ok(false);
            }
            if Instant::now() > deadline {
                return Err(Error::CodeExpired);
            }
            let (status, body) = http::send(&auth::poll(cfg, &code))?;
            match auth::parse_poll(status, &body, crate::clock::moment().unix)? {
                Poll::Pending => {}
                Poll::SlowDown => interval += Duration::from_secs(5),
                Poll::Expired => return Err(Error::CodeExpired),
                Poll::Failed(e) => return Err(Error::LoginRefused(e)),
                Poll::Done(session) => {
                    config::save_tidal_session(Some(&session))?;
                    log::info!("tidal: logged in");
                    let mut inner = self.lock();
                    inner.cfg.session = Some(session);
                    inner.state = State::LoggedIn;
                    return Ok(true);
                }
            }
        }
    }
}
