//! TIDAL on the clock (protocol: [`wakey_core::tidal`]). For now the login:
//! when the sound page's TIDAL tab opens without a session, [`Tidal::open`]
//! asks for a Wi-Fi session ([`crate::net::Wifi::run`]) and, in it, a
//! device code; the page shows its link as a QR code. The user confirms on
//! the phone; the clock polls until then, then writes the session into
//! `config.json`. Leaving the tab stops the polling.
//!
//! The state is shared with the UI thread ([`Tidal::state`]); the work runs
//! on the `wifi` thread.

use crate::config::{self, TidalConfig};
use crate::http;
use crate::net::Wifi;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wakey_core::tidal::auth::{self, Poll};

/// Longest a login code is polled for (TIDAL's are valid for ~5 min).
const LOGIN_MAX: Duration = Duration::from_secs(600);

/// What the TIDAL tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// `config.json` has no app credentials.
    NoCredentials,
    /// No session; nothing under way.
    LoggedOut,
    /// Getting a login code (Wi-Fi coming up).
    Connecting,
    /// Waiting for the user to confirm `code` at `url`.
    Code { url: String, code: String },
    LoggedIn,
    /// The last attempt failed (in words).
    Failed(String),
}

struct Inner {
    cfg: TidalConfig,
    state: State,
    /// A login is under way (on the `wifi` thread).
    busy: bool,
    /// The TIDAL tab is open: the login goes on.
    wanted: bool,
}

/// Handle on the TIDAL state. Cheap to clone.
#[derive(Clone)]
pub struct Tidal {
    inner: Arc<Mutex<Inner>>,
    wifi: Wifi,
}

impl Tidal {
    pub fn new(cfg: TidalConfig, wifi: Wifi) -> Self {
        let state = if !cfg.has_credentials() {
            State::NoCredentials
        } else if cfg.session.is_some() {
            State::LoggedIn
        } else {
            State::LoggedOut
        };
        Self {
            inner: Arc::new(Mutex::new(Inner {
                cfg,
                state,
                busy: false,
                wanted: false,
            })),
            wifi,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> State {
        self.lock().state.clone()
    }

    /// A login is under way (code shown or coming).
    pub fn logging_in(&self) -> bool {
        matches!(self.lock().state, State::Connecting | State::Code { .. })
    }

    /// The TIDAL tab opened: logs in if there is no session.
    pub fn open(&self) {
        let mut inner = self.lock();
        inner.wanted = true;
        let start = matches!(inner.state, State::LoggedOut | State::Failed(_))
            && inner.cfg.has_credentials()
            && inner.cfg.session.is_none()
            && !inner.busy;
        if !start {
            return;
        }
        inner.busy = true;
        inner.state = State::Connecting;
        let cfg = inner.cfg.clone();
        drop(inner);
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            let result = net.and_then(|()| this.login(&cfg));
            let mut inner = this.lock();
            inner.busy = false;
            match result {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("tidal: login: {e}");
                    inner.state = State::Failed(e);
                }
            }
        }));
    }

    /// The TIDAL tab closed (another tab, or the page left).
    pub fn close(&self) {
        self.lock().wanted = false;
    }

    /// The device code login, on the `wifi` thread.
    fn login(&self, cfg: &TidalConfig) -> Result<(), String> {
        let (status, body) = http::send(&auth::device_authorization(cfg)).map_err(|e| e.to_string())?;
        let code = auth::parse_device_authorization(status, &body).map_err(|e| e.to_string())?;
        log::info!("tidal: login code {} (valid {} s)", code.user_code, code.expires_in);
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
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("Der Code ist abgelaufen.".into());
            }
            let (status, body) = http::send(&auth::poll(cfg, &code)).map_err(|e| e.to_string())?;
            match auth::parse_poll(status, &body, crate::clock::moment().unix).map_err(|e| e.to_string())? {
                Poll::Pending => {}
                Poll::SlowDown => interval += Duration::from_secs(5),
                Poll::Expired => return Err("Der Code ist abgelaufen.".into()),
                Poll::Failed(e) => return Err(format!("Anmeldung abgelehnt ({e}).")),
                Poll::Done(session) => {
                    config::save_tidal_session(Some(&session))
                        .map_err(|e| format!("Speichern: {e}"))?;
                    log::info!("tidal: logged in (user {}, {})", session.user_id, session.country_code);
                    let mut inner = self.lock();
                    inner.cfg.session = Some(session);
                    inner.state = State::LoggedIn;
                    return Ok(());
                }
            }
        }
    }
}
