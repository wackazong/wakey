//! TIDAL on the clock (protocol: [`wakey_core::tidal`]).
//!
//! Login: when the sound page's TIDAL tab opens without a session,
//! [`Tidal::open`] asks for a Wi-Fi session ([`crate::net::Wifi::run`])
//! and, in it, a device code; the page shows its link as a QR code. The
//! user confirms on the phone; the clock polls until then, then writes the
//! session into `config.json`. Leaving the tab stops the polling.
//!
//! Sync: whenever the tab opens with a session, a session job refreshes
//! the token if it is about to expire (saved), finds the playlist
//! (`playlist` in `config.json`, "Wakey") and reads its tracks into
//! `tidal/playlist.json` on the card, which [`Tidal::listed`] serves
//! (offline too). The playlist's id is kept in `tidal/state.json`: it is
//! searched by name among the user's playlists only when unknown, gone or
//! renamed, and its tracks are read only when it changed. While the tab
//! stays open the job syncs again every [`WATCH`] (edits in the TIDAL app
//! show up); Wi-Fi stays on until the tab closes.
//!
//! Downloads: only selected tracks ([`Tidal::set_selected`]: those of the
//! saved alarms and the one picked in the alarm being edited; all of the
//! playlist while an alarm uses "Random"). A change of the selection
//! changes the queue; the download under way finishes. The job fetches the
//! selected tracks the card lacks, one at a time in playlist order, to
//! `tidal/<id>.flac` ([`download`]), and deletes the files of tracks no
//! longer in the playlist. It keeps going after the tab closes until
//! nothing is missing. A track that fails is not tried again until the
//! next start (its row says so, it cannot be picked;
//! [`TrackStatus::Failed`]).
//!
//! The state is shared with the UI thread ([`Tidal::state`]); the work runs
//! on the `wifi` thread.

mod card_writer;
mod download;
mod growing;
mod playlist;
mod session;
mod store;

pub use growing::Growing;
pub use session::Problem;
pub use store::{cover_rgb, downloaded, track_file, track_title};

use crate::airdrop;
use crate::board::sd;
use crate::config::{self, TidalConfig};
use crate::net::{Unavailable, Wifi};
use session::Error;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wakey_core::tidal::api::Track;

/// While the tab is open: the playlist read again this often.
const WATCH: Duration = Duration::from_secs(5);

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
    /// Logged in; the playlist ([`Tidal::listed`]) is as fresh as it gets.
    LoggedIn,
    /// The last attempt failed.
    Failed(Problem),
}

/// Where a track of the playlist stands, for the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackStatus {
    /// On the card.
    Ready,
    Downloading,
    /// Selected, waiting for its download.
    Missing,
    /// Neither selected nor on the card.
    Remote,
    /// Its download failed; not tried again until the next start.
    Failed,
}

/// An alarm's settings pages are open (`app::show_page`): a Wi-Fi session
/// started for them ([`Tidal::warm`]) stays up, so the TIDAL tab finds the
/// playlist synced and the network ready.
static EDITING: AtomicBool = AtomicBool::new(false);

/// The alarm, sound or name page is shown (or not).
pub fn set_editing(on: bool) {
    EDITING.store(on, Ordering::Relaxed);
}

struct Inner {
    cfg: TidalConfig,
    state: State,
    /// A job is queued or running (on the `wifi` thread).
    busy: bool,
    /// The TIDAL tab is open: the login goes on.
    wanted: bool,
    /// The playlist's tracks, in order (from the card, then synced).
    playlist: Vec<Track>,
    /// Counts changes of `playlist` (the tab is filled again).
    version: u32,
    /// The playlist's id (kept in [`store::STATE_FILE`]: no search by name
    /// once found) and its `lastUpdated` when its tracks were last read
    /// (the tracks are read again only when it changed).
    uuid: Option<String>,
    last_updated: Option<String>,
    /// The access token bound with `/sessions` (once per token).
    bound: Option<String>,
    /// Tracks on the card (`tidal/<id>.flac`).
    have: BTreeSet<u64>,
    /// Tracks to have on the card (chosen in alarms).
    selected: BTreeSet<u64>,
    /// An alarm uses "Random": the whole playlist.
    all: bool,
    /// The track being downloaded, and how far (0–1).
    downloading: Option<(u64, f32)>,
    /// Tracks whose download failed: not tried again until the next start.
    failed: BTreeSet<u64>,
    /// Covers whose download failed: not tried again until the next start
    /// (a sync runs every 5 s while the tab is open).
    covers_failed: BTreeSet<String>,
    /// Downloaded first when missing: the track picked or previewed.
    prefer: Option<u64>,
    /// Tracks whose download stopped for another one with their
    /// `.flac.part` kept (one file, not DASH): resumed where they stopped.
    resumable: BTreeSet<u64>,
    /// Tracks seen in the playlist ([`store::KNOWN_FILE`]).
    known: store::Known,
    /// Tracks no longer in the playlist that stay listed while the tab is
    /// open (the alarm being edited used them when it opened, or since):
    /// one can go back to them until the tab closes.
    keep: BTreeSet<u64>,
    /// The track of the alarm being edited (listed even if it left the
    /// playlist).
    draft: Option<u64>,
}

impl Inner {
    /// The track is to be on the card.
    fn wants(&self, id: u64) -> bool {
        self.all || self.selected.contains(&id)
    }

    /// Tracks of `ids` no longer in the playlist, as last seen.
    fn orphans_of<'a>(&self, ids: impl Iterator<Item = &'a u64>) -> Vec<Track> {
        ids.collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|id| !self.playlist.iter().any(|t| t.id == **id))
            .filter_map(|id| self.known.get(id).cloned())
            .collect()
    }

    /// Listed after the playlist: the edited alarm's track and those kept
    /// while the tab is open, if they left the playlist.
    fn listed_orphans(&self) -> Vec<Track> {
        self.orphans_of(self.draft.iter().chain(&self.keep))
    }

    /// The playlist, then [`Self::listed_orphans`]: what the tab lists.
    fn with_listed_orphans(&self) -> Vec<Track> {
        let mut listed = self.playlist.clone();
        listed.extend(self.listed_orphans());
        listed
    }

    /// The tracks to have (downloads): the playlist and the tracks that
    /// left it but an alarm uses or the tab lists.
    fn tracks(&self) -> Vec<Track> {
        let mut all = self.playlist.clone();
        all.extend(self.orphans_of(self.selected.iter().chain(&self.keep)));
        all
    }

    /// While the tab is open: the edited alarm's track, if it left the
    /// playlist, stays listed until the tab closes.
    fn keep_draft(&mut self) {
        if self.wanted
            && let Some(id) = self.draft
            && !self.playlist.iter().any(|t| t.id == id)
        {
            self.keep.insert(id);
        }
    }

    /// A track to download: wanted, not on the card, not failed before.
    fn lacks(&self, id: u64) -> bool {
        self.wants(id) && !self.have.contains(&id) && !self.failed.contains(&id)
    }

    /// Some track of [`Self::tracks`] is to be downloaded.
    fn missing(&self) -> bool {
        self.tracks().iter().any(|t| self.lacks(t.id))
    }

    /// The picked track, if it is to be downloaded and can be: one of
    /// [`Self::tracks`] (a track whose title was never seen is not).
    fn preferred(&self) -> Option<u64> {
        let p = self.prefer?;
        let in_tracks = self.playlist.iter().any(|t| t.id == p)
            || ((self.selected.contains(&p) || self.keep.contains(&p))
                && self.known.contains_key(&p));
        (in_tracks && self.lacks(p)).then_some(p)
    }

    /// The list is to be filled again.
    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }
}

/// What a TIDAL job does in its Wi-Fi session, before
/// [`Tidal::keep_up`].
type Work = Box<dyn FnOnce(&Tidal, Result<(), Unavailable>) + Send>;

/// Why [`Tidal::keep_up`] ended.
#[derive(PartialEq, Eq)]
enum Ended {
    Done,
    /// AirDrop wants the C6: the Wi-Fi session ends early.
    ForAirDrop,
}

/// Handle on the TIDAL state. Cheap to clone.
#[derive(Clone)]
pub struct Tidal {
    inner: Arc<Mutex<Inner>>,
    /// Wakes the download job (`keep_up`) when the selection changes.
    kick: Arc<Condvar>,
    wifi: Wifi,
}

impl Tidal {
    pub fn new(cfg: TidalConfig, wifi: Wifi) -> Self {
        // The playlist found before, if it is still the one configured.
        let kept = store::load_state()
            .filter(|s| s.playlist == cfg.playlist)
            .unwrap_or_default();
        let state = if !cfg.has_credentials() {
            State::NoCredentials
        } else if cfg.session.is_some() {
            State::LoggedIn
        } else {
            State::LoggedOut
        };
        // Unfinished downloads of the last run: none is resumed (what can
        // be is known only while running, `Inner::resumable`).
        store::remove_parts(&BTreeSet::new());
        let playlist = store::load_playlist();
        let mut known = store::load_known();
        known.extend(playlist.iter().map(|t| (t.id, t.clone())));
        Self {
            inner: Arc::new(Mutex::new(Inner {
                cfg,
                state,
                busy: false,
                wanted: false,
                playlist,
                version: 0,
                uuid: kept.uuid,
                last_updated: kept.last_updated,
                bound: None,
                have: store::scan_tracks(),
                selected: BTreeSet::new(),
                all: false,
                downloading: None,
                failed: BTreeSet::new(),
                covers_failed: BTreeSet::new(),
                prefer: None,
                resumable: BTreeSet::new(),
                known,
                keep: BTreeSet::new(),
                draft: None,
            })),
            kick: Arc::new(Condvar::new()),
            wifi,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> State {
        self.lock().state.clone()
    }

    /// The tracks the TIDAL tab lists: the playlist, then tracks that left
    /// it but the alarm being edited uses (or used since the tab opened),
    /// each with `true` if it is one of those.
    pub fn listed(&self) -> Vec<(Track, bool)> {
        let inner = self.lock();
        let mut list: Vec<(Track, bool)> =
            inner.playlist.iter().cloned().map(|t| (t, false)).collect();
        list.extend(inner.listed_orphans().into_iter().map(|t| (t, true)));
        list
    }

    /// Changes when [`Self::listed`] or a track's status does.
    pub fn version(&self) -> u32 {
        self.lock().version
    }

    pub fn track_status(&self, id: u64) -> TrackStatus {
        let inner = self.lock();
        match inner.downloading {
            Some((d, _)) if d == id => TrackStatus::Downloading,
            _ if inner.have.contains(&id) => TrackStatus::Ready,
            _ if inner.failed.contains(&id) => TrackStatus::Failed,
            _ if inner.wants(id) => TrackStatus::Missing,
            _ => TrackStatus::Remote,
        }
    }

    /// A login is under way (code shown or coming).
    pub fn logging_in(&self) -> bool {
        matches!(self.lock().state, State::Connecting | State::Code { .. })
    }

    /// The tracks to have on the card. Starts their downloads (a Wi-Fi
    /// session) unless a job is under way, which takes them on.
    /// `all`: the whole playlist (an alarm uses "Random").
    ///
    /// `draft`: the track of the alarm being edited (the tab lists it even
    /// if it left the playlist).
    pub fn set_selected(&self, ids: BTreeSet<u64>, all: bool, draft: Option<u64>) {
        let mut inner = self.lock();
        if inner.selected != ids || inner.all != all || inner.draft != draft {
            inner.selected = ids;
            inner.all = all;
            inner.draft = draft;
            inner.bump();
        }
        inner.keep_draft();
        let missing = inner.missing();
        self.kick.notify_all();
        if !missing || inner.busy || inner.cfg.session.is_none() {
            return;
        }
        inner.busy = true;
        drop(inner);
        self.run_job(|_, net| {
            if let Err(e) = net {
                log::warn!("tidal: downloads: {e}");
            }
        });
    }

    /// The TIDAL tab opened: logs in if there is no session, else syncs the
    /// playlist, and keeps it fresh while the tab is open.
    pub fn open(&self) {
        let mut inner = self.lock();
        inner.wanted = true;
        inner.keep.clear();
        inner.keep_draft();
        // A job already up (started with the alarm page) follows at once.
        self.kick.notify_all();
        if inner.busy || !inner.cfg.has_credentials() {
            return;
        }
        let sync = inner.cfg.session.is_some();
        let login = !sync && matches!(inner.state, State::LoggedOut | State::Failed(_));
        if !sync && !login {
            return;
        }
        inner.busy = true;
        inner.state = if sync {
            State::Syncing
        } else {
            State::Connecting
        };
        drop(inner);
        self.run_job(move |this, net| match net {
            Ok(()) if sync => this.sync_or_login(),
            Ok(()) => this.login_and_sync(),
            Err(e) => this.lock().state = State::Failed(Problem::Network(e)),
        });
    }

    /// The alarm page opened: a Wi-Fi session now, and the playlist synced,
    /// so the TIDAL tab finds both ready (the session takes ~7 s to come
    /// up). Held while the settings pages are open ([`set_editing`]). Only
    /// with a session: a login waits for the tab (it shows the code).
    pub fn warm(&self) {
        let mut inner = self.lock();
        if inner.busy || !inner.cfg.has_credentials() || inner.cfg.session.is_none() {
            return;
        }
        inner.busy = true;
        inner.state = State::Syncing;
        drop(inner);
        self.run_job(|this, net| match net {
            Ok(()) => this.sync_or_login(),
            Err(e) => this.lock().state = State::Failed(Problem::Network(e)),
        });
    }

    /// The TIDAL tab closed (another tab, or the page left).
    pub fn close(&self) {
        let mut inner = self.lock();
        inner.wanted = false;
        // Tracks that left the playlist and no alarm uses go at the next
        // sync.
        inner.keep.clear();
    }

    /// `id` was picked: if it is not on the card, it is downloaded next,
    /// and a download of another track stops for it (tried again later).
    pub fn prefer(&self, id: u64) {
        self.lock().prefer = Some(id);
        self.kick.notify_all();
    }

    /// A track to preview that is not on the card yet but coming (being
    /// downloaded or waiting for it, then first in line): reads what has
    /// arrived and waits for more ([`Growing`]). `None` if it is not
    /// coming (failed, not selected).
    pub fn stream(&self, id: u64, cancel: Arc<AtomicBool>) -> Option<Growing> {
        match self.track_status(id) {
            TrackStatus::Downloading | TrackStatus::Missing => {}
            _ => return None,
        }
        self.prefer(id);
        Some(Growing::new(id, self.clone(), cancel))
    }

    /// The download under way and how far (0–1), for the progress bar.
    pub fn progress(&self) -> Option<(u64, f32)> {
        self.lock().downloading
    }

    /// Runs `work` in a Wi-Fi session (with the reason if there is no
    /// connection), then [`Self::keep_up`]; `busy` until both are done.
    /// When AirDrop ends the session, the rest is queued again: the `wifi`
    /// thread holds jobs while AirDrop has the C6 and runs them after.
    fn run_job(&self, work: impl FnOnce(&Tidal, Result<(), Unavailable>) + Send + 'static) {
        self.queue_job(Box::new(work));
    }

    fn queue_job(&self, work: Work) {
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            let connected = net.is_ok();
            work(&this, net);
            if connected && this.keep_up() == Ended::ForAirDrop {
                log::info!("tidal: AirDrop takes the C6; going on after it");
                // The tab may have opened meanwhile: synced at once then.
                this.queue_job(Box::new(|this, net| match net {
                    Ok(()) if this.lock().wanted => this.sync_or_login(),
                    Ok(()) => {}
                    Err(e) => log::warn!("tidal: downloads: {e}"),
                }));
            } else {
                this.lock().busy = false;
            }
        }));
    }

    /// In a session: syncs; if TIDAL rejects the session (401), drops it
    /// and logs in anew at once (the QR code shows while the tab is open).
    fn sync_or_login(&self) {
        match self.sync() {
            Err(Error::SessionInvalid) => {
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
                if matches!(result, Err(Error::SessionInvalid)) {
                    // A fresh session refused: not again in a loop.
                    self.drop_session();
                }
                self.synced(result);
            }
            Ok(false) => {}
            Err(e) => {
                log::warn!("tidal: login: {e}");
                self.lock().state = State::Failed(e.problem());
            }
        }
    }

    /// After a sync: the state.
    fn synced(&self, result: session::Result<usize>) {
        match result {
            Ok(n) => {
                log::info!("tidal: playlist synced, {n} tracks");
                self.lock().state = State::LoggedIn;
            }
            Err(e) => {
                log::warn!("tidal: sync: {e}");
                self.lock().state = State::Failed(e.problem());
            }
        }
    }

    fn drop_session(&self) {
        if let Err(e) = config::save_tidal_session(None) {
            log::error!("tidal: removing the session: {e}");
        }
        let mut inner = self.lock();
        inner.cfg.session = None;
        // Another login may be another account.
        inner.uuid = None;
        inner.last_updated = None;
        inner.bound = None;
        let _ = std::fs::remove_file(sd::path(store::STATE_FILE));
    }

    /// After a sync: downloads what the card lacks, and while the tab is
    /// open reads the playlist again every [`WATCH`]. Ends when the tab is
    /// closed and nothing is left to download, on an error, or when AirDrop
    /// wants the C6.
    fn keep_up(&self) -> Ended {
        let mut last_sync = Instant::now();
        loop {
            if airdrop::wanted() {
                return Ended::ForAirDrop;
            }
            let (wanted, urgent) = {
                let inner = self.lock();
                if inner.state != State::LoggedIn {
                    return Ended::Done;
                }
                (inner.wanted, inner.preferred().is_some())
            };
            let due = wanted && last_sync.elapsed() >= WATCH;
            // A picked track that is missing goes before the sync.
            if (!due || urgent)
                && let Some(track) = self.next_missing()
            {
                match self.download(&track) {
                    Ok(()) => {}
                    Err(Error::Deferred) => {
                        log::info!("tidal: download {}: stopped for another track", track.id);
                    }
                    Err(e @ Error::SessionInvalid) => {
                        // The session, not the track: tried again after
                        // the next login.
                        log::warn!("tidal: download {}: {e}", track.id);
                        self.lock().state = State::Failed(e.problem());
                        return Ended::Done;
                    }
                    Err(e) => {
                        log::warn!("tidal: download {}: {e}", track.id);
                        self.lock().failed.insert(track.id);
                        self.bump();
                    }
                }
                continue;
            }
            if !wanted && !EDITING.load(Ordering::Relaxed) {
                return Ended::Done;
            }
            if !wanted {
                // Settings pages open, the tab not: the session waits for
                // it (checked every second; a pick or the tab wakes it).
                let inner = self.lock();
                if !inner.wanted && !inner.missing() {
                    let _ = self.kick.wait_timeout(inner, Duration::from_secs(1));
                }
                continue;
            }
            if let Some(rest) = WATCH.checked_sub(last_sync.elapsed()) {
                // Until the next sync, or a pick (checked under the lock
                // the pick takes: no wake-up lost).
                let inner = self.lock();
                if !inner.missing() {
                    let _ = self.kick.wait_timeout(inner, rest);
                }
                continue;
            }
            if !self.lock().wanted {
                continue; // closed meanwhile; downloads may remain
            }
            last_sync = Instant::now();
            if let Err(e) = self.sync() {
                log::warn!("tidal: watch: {e}");
                self.lock().state = State::Failed(e.problem());
                return Ended::Done;
            }
        }
    }

    /// The first track of the playlist the card lacks (not failed before).
    fn next_missing(&self) -> Option<Track> {
        let inner = self.lock();
        let preferred = inner.preferred();
        let tracks = inner.tracks();
        tracks
            .iter()
            .find(|t| Some(t.id) == preferred)
            .or_else(|| tracks.iter().find(|t| inner.lacks(t.id)))
            .cloned()
    }

    /// The download of `id` is to stop: another track was picked and is
    /// still missing, or AirDrop wants the C6 (the download resumes later).
    fn deferred(&self, id: u64) -> bool {
        if airdrop::wanted() {
            return true;
        }
        self.lock().preferred().is_some_and(|p| p != id)
    }

    /// Records progress; a new track also refills the list.
    fn set_progress(&self, id: u64, fraction: f32) {
        let mut inner = self.lock();
        let new = inner.downloading.is_none_or(|(d, _)| d != id);
        inner.downloading = Some((id, fraction.clamp(0.0, 1.0)));
        if new {
            inner.bump();
        }
    }

    /// The list is to be filled again.
    fn bump(&self) {
        self.lock().bump();
    }
}
