//! TIDAL on the clock (protocol: [`wakey_core::tidal`]).
//!
//! Login: when the sound page's TIDAL tab opens without a session, [`Tidal::open`]
//! asks for a Wi-Fi session ([`crate::net::Wifi::run`]) and, in it, a
//! device code; the page shows its link as a QR code. The user confirms on
//! the phone; the clock polls until then, then writes the session into
//! `config.json`. Leaving the tab stops the polling.
//!
//! Sync: when the tab opens with a session (once per boot), a session job
//! refreshes the token if it is about to expire (saved), finds the
//! playlist (`playlist` in `config.json`, "Wecker") among the user's own
//! and reads its tracks into `tidal/playlist.json` on the card, which
//! [`Tidal::playlist`] serves (offline too).
//!
//! The state is shared with the UI thread ([`Tidal::state`]); the work runs
//! on the `wifi` thread.

use crate::board::sd;
use crate::config::{self, TidalConfig, TidalSession};
use crate::http;
use crate::net::Wifi;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::path::PathBuf;
use wakey_core::tidal::api::{self, Track};
use wakey_core::tidal::auth::{self, Poll};
use wakey_core::tidal::sync;

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
    /// Reading the playlist.
    Syncing,
    /// Logged in; [`Tidal::playlist`] is as fresh as it gets.
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
    /// The playlist's tracks, in order (from the card, then synced).
    playlist: Vec<Track>,
    /// Synced since boot.
    synced: bool,
}

/// Sends `req`; on an error status logs TIDAL's reason (only `status`,
/// `subStatus` and `userMessage`: the rest of a body may carry tokens).
/// A 401: the session is no good (dropped; the next open logs in anew).
const SESSION_INVALID: &str = "Die Anmeldung ist ungültig.";

fn send(req: &wakey_core::tidal::Request, stage: &str) -> Result<(u16, Vec<u8>), String> {
    let (status, body) = http::send(req).map_err(|e| e.to_string())?;
    if status >= 400 {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Reason {
            sub_status: Option<i64>,
            user_message: Option<String>,
        }
        match serde_json::from_slice::<Reason>(&body) {
            Ok(r) => log::warn!(
                "tidal: {stage}: HTTP {status}, subStatus {:?}, {:?}",
                r.sub_status,
                r.user_message
            ),
            Err(_) => log::warn!("tidal: {stage}: HTTP {status} ({} bytes)", body.len()),
        }
        if status == 401 {
            return Err(SESSION_INVALID.into());
        }
    }
    Ok((status, body))
}

fn sd_path(rel: &str) -> PathBuf {
    PathBuf::from(sd::MOUNT_POINT).join(rel)
}

/// The playlist as last synced (empty if none).
fn load_playlist() -> Vec<Track> {
    std::fs::read(sd_path(sync::PLAYLIST_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Writes the playlist through a temporary file.
fn save_playlist(tracks: &[Track]) -> std::io::Result<()> {
    std::fs::create_dir_all(sd_path(sync::DIR))?;
    let path = sd_path(sync::PLAYLIST_FILE);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(tracks).map_err(std::io::Error::other)?)?;
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::rename(&tmp, &path)
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
                playlist: load_playlist(),
                synced: false,
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

    /// The playlist's tracks, in order.
    pub fn playlist(&self) -> Vec<Track> {
        self.lock().playlist.clone()
    }

    /// A login is under way (code shown or coming).
    pub fn logging_in(&self) -> bool {
        matches!(self.lock().state, State::Connecting | State::Code { .. })
    }

    /// The TIDAL tab opened: logs in if there is no session, else syncs the
    /// playlist (once per boot, or after a failure).
    pub fn open(&self) {
        let mut inner = self.lock();
        inner.wanted = true;
        if inner.busy || !inner.cfg.has_credentials() {
            return;
        }
        let sync = inner.cfg.session.is_some()
            && (!inner.synced || matches!(inner.state, State::Failed(_)));
        let login = inner.cfg.session.is_none()
            && matches!(inner.state, State::LoggedOut | State::Failed(_));
        if !sync && !login {
            return;
        }
        inner.busy = true;
        inner.state = if sync { State::Syncing } else { State::Connecting };
        drop(inner);
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            match net {
                Ok(()) if sync => this.sync_or_login(),
                Ok(()) => this.login_and_sync(),
                Err(e) => this.lock().state = State::Failed(e),
            }
            this.lock().busy = false;
        }));
    }

    /// In a session: syncs; if TIDAL rejects the session (401), drops it
    /// and logs in anew at once (the QR code shows while the tab is open).
    fn sync_or_login(&self) {
        match self.sync() {
            Err(e) if e == SESSION_INVALID => {
                log::warn!("tidal: session rejected (401), dropped; logging in anew");
                self.drop_session();
                if self.lock().wanted {
                    self.login_and_sync();
                } else {
                    self.lock().state = State::LoggedOut;
                }
            }
            result => self.synced(result),
        }
    }

    /// In a session: the device code login, then the playlist right away.
    fn login_and_sync(&self) {
        self.lock().state = State::Connecting;
        let cfg = self.lock().cfg.clone();
        match self.login(&cfg) {
            Ok(true) => {
                self.lock().state = State::Syncing;
                let result = self.sync();
                if matches!(&result, Err(e) if e == SESSION_INVALID) {
                    // A fresh session refused: not again in a loop.
                    self.drop_session();
                }
                self.synced(result);
            }
            Ok(false) => {}
            Err(e) => {
                log::warn!("tidal: login: {e}");
                self.lock().state = State::Failed(e);
            }
        }
    }

    fn drop_session(&self) {
        if let Err(e) = config::save_tidal_session(None) {
            log::error!("tidal: removing the session: {e}");
        }
        let mut inner = self.lock();
        inner.cfg.session = None;
        inner.synced = false;
    }

    /// After a sync: the state.
    fn synced(&self, result: Result<usize, String>) {
        match result {
            Ok(n) => {
                log::info!("tidal: playlist synced, {n} tracks");
                let mut inner = self.lock();
                inner.synced = true;
                inner.state = State::LoggedIn;
            }
            Err(e) => {
                log::warn!("tidal: sync: {e}");
                self.lock().state = State::Failed(e);
            }
        }
    }

    /// The TIDAL tab closed (another tab, or the page left).
    pub fn close(&self) {
        self.lock().wanted = false;
    }

    /// The session, refreshed (and saved) if its token is about to expire.
    fn session(&self) -> Result<TidalSession, String> {
        let (cfg, session) = {
            let inner = self.lock();
            let s = inner.cfg.session.clone().ok_or("Nicht bei TIDAL angemeldet.")?;
            (inner.cfg.clone(), s)
        };
        let now = crate::clock::moment().unix;
        log::info!(
            "tidal: token {} chars, valid {} s more",
            session.access_token.len(),
            session.expires_at - now
        );
        if !auth::needs_refresh(&session, now) {
            return Ok(session);
        }
        let (status, body) = send(&auth::refresh(&cfg, &session), "refresh")?;
        let fresh = auth::parse_refresh(status, &body, &session, now).map_err(|e| e.to_string())?;
        config::save_tidal_session(Some(&fresh)).map_err(|e| format!("Speichern: {e}"))?;
        log::info!("tidal: token refreshed");
        self.lock().cfg.session = Some(fresh.clone());
        Ok(fresh)
    }

    /// Reads the playlist's tracks into `tidal/playlist.json`, on the `wifi`
    /// thread. The number of tracks.
    fn sync(&self) -> Result<usize, String> {
        let s = self.session()?;
        // Binds an API session to the token; user calls fail without it.
        let (status, body) = send(&api::sessions(&s), "session")?;
        let (user, country) = api::parse_sessions(status, &body).map_err(|e| e.to_string())?;
        log::info!("tidal: session for user {user}, {country}");
        let name = self.lock().cfg.playlist.clone();
        let mut lists = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) = send(&api::playlists(&s, o), "playlists")?;
            let (page, next) = api::parse_playlists(status, &body, o).map_err(|e| e.to_string())?;
            lists.extend(page);
            offset = next;
        }
        let list = api::find_playlist(&lists, &name)
            .ok_or_else(|| format!("Keine Playlist „{name}“ bei TIDAL."))?
            .clone();
        let mut tracks = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) = send(&api::playlist_items(&s, &list.uuid, o), "playlist")?;
            let (page, next) =
                api::parse_playlist_items(status, &body, o).map_err(|e| e.to_string())?;
            tracks.extend(page);
            offset = next;
        }
        save_playlist(&tracks).map_err(|e| format!("Speichern: {e}"))?;
        let n = tracks.len();
        self.lock().playlist = tracks;
        Ok(n)
    }

    /// The device code login, on the `wifi` thread. True if logged in
    /// (false: the tab was closed).
    fn login(&self, cfg: &TidalConfig) -> Result<bool, String> {
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
                return Ok(false);
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
                    log::info!(
                        "tidal: logged in (user {}, {}; token {} chars, valid {} s)",
                        session.user_id,
                        session.country_code,
                        session.access_token.len(),
                        session.expires_at - crate::clock::moment().unix
                    );
                    let mut inner = self.lock();
                    inner.cfg.session = Some(session);
                    inner.state = State::LoggedIn;
                    return Ok(true);
                }
            }
        }
    }
}
