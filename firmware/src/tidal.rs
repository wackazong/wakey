//! TIDAL on the clock (protocol: [`wakey_core::tidal`]).
//!
//! Login: when the sound page's TIDAL tab opens without a session, [`Tidal::open`]
//! asks for a Wi-Fi session ([`crate::net::Wifi::run`]) and, in it, a
//! device code; the page shows its link as a QR code. The user confirms on
//! the phone; the clock polls until then, then writes the session into
//! `config.json`. Leaving the tab stops the polling.
//!
//! Sync: whenever the tab opens with a session, a session job refreshes
//! the token if it is about to expire (saved), finds the playlist
//! (`playlist` in `config.json`, "Wecker") among the user's own and reads
//! its tracks into `tidal/playlist.json` on the card, which
//! [`Tidal::playlist`] serves (offline too). While the tab stays open the
//! job reads the tracks again every [`WATCH`] (edits in the TIDAL app show
//! up); Wi-Fi stays on until the tab closes.
//!
//! Downloads: only selected tracks ([`Tidal::set_selected`]: those of the
//! saved alarms and the one picked in the alarm being edited; all of the
//! playlist while an alarm uses „Zufällig“). A change of the selection
//! changes the queue; the download under way finishes. The job
//! fetches the selected tracks the card lacks, one at a time in playlist
//! order, to `tidal/<id>.flac` (via `<id>.flac.part`,
//! renamed once complete and readable as FLAC; lossless arrives as DASH
//! segments of FLAC in MP4, unpacked on the way: `wakey_core::tidal::flac_mp4`), and
//! deletes the files of tracks no longer in the playlist. It keeps going
//! after the tab closes until nothing is missing. A track that fails is
//! not tried again in that job.
//!
//! The state is shared with the UI thread ([`Tidal::state`]); the work runs
//! on the `wifi` thread.

use crate::board::sd;
use crate::config::{self, TidalConfig, TidalSession};
use crate::http;
use crate::net::Wifi;
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::path::PathBuf;
use wakey_core::tidal::api::{self, Track};
use wakey_core::audio_file::{AudioFile, Container};
use wakey_core::tidal::auth::{self, Poll};
use wakey_core::tidal::flac_mp4::Unpacker;
use wakey_core::tidal::manifest::MediaPlan;
use wakey_core::tidal::sync;

/// While the tab is open: the playlist read again this often.
const WATCH: Duration = Duration::from_secs(5);

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
    /// Counts changes of `playlist` (the tab is filled again).
    version: u32,
    /// The playlist's id, once found in this Wi-Fi session.
    uuid: Option<String>,
    /// Tracks on the card (`tidal/<id>.flac`).
    have: BTreeSet<u64>,
    /// Tracks to have on the card (chosen in alarms).
    selected: BTreeSet<u64>,
    /// An alarm uses „Zufällig“: the whole playlist.
    all: bool,
    /// The track being downloaded, and how far (0–1).
    downloading: Option<(u64, f32)>,
}

impl Inner {
    /// The track is to be on the card.
    fn wants(&self, id: u64) -> bool {
        self.all || self.selected.contains(&id)
    }
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
}


/// The tracks on the card.
fn scan_tracks() -> BTreeSet<u64> {
    std::fs::read_dir(sd_path(sync::DIR))
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| sync::track_id_of(&e.file_name().to_string_lossy()))
                .collect()
        })
        .unwrap_or_default()
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

/// The playlist's tracks that are on the card, in playlist order (for the
/// alarm: `tidal:random`), and the file of one track if it is there.
pub fn downloaded() -> Vec<Track> {
    let have = scan_tracks();
    load_playlist().into_iter().filter(|t| have.contains(&t.id)).collect()
}

pub fn track_file(id: u64) -> Option<PathBuf> {
    let path = sd_path(&sync::track_path(id));
    path.exists().then_some(path)
}

/// A track's title and artist from the stored playlist.
pub fn track_title(id: u64) -> Option<(String, String)> {
    load_playlist()
        .into_iter()
        .find(|t| t.id == id)
        .map(|t| (t.title, t.artist))
}

/// Writes a file on its own thread, in whole 128 KiB blocks from cache-line
/// aligned buffers, so the network and the card work at the same time.
///
/// Aligned: FatFs hands such a block to the SD driver as is, and the driver
/// sends it by DMA straight from PSRAM; from an unaligned `Vec` it copied
/// every write through a freshly allocated temporary buffer (card writes
/// took 58 % of a download, ~380 KB/s). Own thread: writing took half of
/// the remaining time, one after the other with receiving. [`BLOCKS`]
/// blocks circulate between this side and the writer (back-pressure when
/// the card falls behind). The writer runs on core 1 below the audio
/// decoder.
struct CardWriter {
    /// Full blocks (pointer, length) to the writer; `None`: finish.
    to_writer: Option<std::sync::mpsc::SyncSender<Option<(usize, usize)>>>,
    /// Written blocks back.
    free: std::sync::mpsc::Receiver<usize>,
    writer: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    /// The block being filled (address), and how full.
    cur: usize,
    len: usize,
    /// All blocks, one allocation.
    region: *mut u8,
}

const BLOCK: usize = 128 * 1024;
const BLOCKS: usize = 4;

impl CardWriter {
    fn create(path: &std::path::Path) -> std::io::Result<Self> {
        let mut file = std::fs::File::create(path)?;
        let region = unsafe {
            esp_idf_svc::sys::heap_caps_aligned_alloc(
                128,
                BLOCK * BLOCKS,
                esp_idf_svc::sys::MALLOC_CAP_SPIRAM | esp_idf_svc::sys::MALLOC_CAP_8BIT,
            )
        }
        .cast::<u8>();
        if region.is_null() {
            return Err(std::io::Error::other("no memory for the write buffers"));
        }
        let (to_writer, from_main) = std::sync::mpsc::sync_channel::<Option<(usize, usize)>>(BLOCKS);
        let (to_main, free) = std::sync::mpsc::sync_channel::<usize>(BLOCKS);
        for k in 1..BLOCKS {
            let _ = to_main.send(region as usize + k * BLOCK);
        }
        let previous = esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration::get()
            .unwrap_or_default();
        let _ = esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration {
            name: Some(c"card-writer"),
            pin_to_core: Some(esp_idf_svc::hal::cpu::Core::Core1),
            priority: 4,
            ..Default::default()
        }
        .set();
        let writer = std::thread::Builder::new()
            .name("card-writer".into())
            .stack_size(6144)
            .spawn(crate::taskstats::named("card-writer", move || {
                while let Ok(Some((addr, len))) = from_main.recv() {
                    // SAFETY: the block is this thread's until sent back.
                    let data = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
                    file.write_all(data)?;
                    let _ = to_main.send(addr);
                }
                file.flush()
            }));
        let _ = previous.set();
        Ok(Self {
            to_writer: Some(to_writer),
            free,
            writer: Some(writer?),
            cur: region as usize,
            len: 0,
            region,
        })
    }

    /// Hands the current block to the writer and takes a free one.
    fn pass(&mut self) -> std::io::Result<()> {
        let gone = || std::io::Error::other("card writer stopped");
        let tx = self.to_writer.as_ref().ok_or_else(gone)?;
        if tx.send(Some((self.cur, self.len))).is_err() {
            return Err(self.join().err().unwrap_or_else(gone));
        }
        self.len = 0;
        match self.free.recv() {
            Ok(b) => {
                self.cur = b;
                Ok(())
            }
            Err(_) => Err(self.join().err().unwrap_or_else(gone)),
        }
    }

    /// Ends the writer thread; its result.
    fn join(&mut self) -> std::io::Result<()> {
        self.to_writer = None;
        match self.writer.take() {
            Some(h) => h.join().unwrap_or_else(|_| Err(std::io::Error::other("card writer panicked"))),
            None => Ok(()),
        }
    }
}

impl Write for CardWriter {
    fn write(&mut self, mut data: &[u8]) -> std::io::Result<usize> {
        let n = data.len();
        while !data.is_empty() {
            let take = (BLOCK - self.len).min(data.len());
            // SAFETY: within the current block.
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), (self.cur as *mut u8).add(self.len), take)
            };
            self.len += take;
            data = &data[take..];
            if self.len == BLOCK {
                self.pass()?;
            }
        }
        Ok(n)
    }

    /// Writes what is left and waits until all is on the card.
    fn flush(&mut self) -> std::io::Result<()> {
        if self.len > 0 {
            let tx = self.to_writer.as_ref().ok_or_else(|| std::io::Error::other("card writer stopped"))?;
            let _ = tx.send(Some((self.cur, self.len)));
            self.len = 0;
        }
        if let Some(tx) = &self.to_writer {
            let _ = tx.send(None);
        }
        self.join()
    }
}

impl Drop for CardWriter {
    fn drop(&mut self) {
        // The writer must be done with the blocks before they go.
        let _ = self.join();
        unsafe { esp_idf_svc::sys::heap_caps_free(self.region.cast()) };
    }
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
                version: 0,
                uuid: None,
                have: scan_tracks(),
                selected: BTreeSet::new(),
                all: false,
                downloading: None,
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

    /// Changes when [`Self::playlist`] or a track's status does.
    pub fn version(&self) -> u32 {
        self.lock().version
    }

    pub fn track_status(&self, id: u64) -> TrackStatus {
        let inner = self.lock();
        match inner.downloading {
            Some((d, _)) if d == id => TrackStatus::Downloading,
            _ if inner.have.contains(&id) => TrackStatus::Ready,
            _ if inner.all || inner.selected.contains(&id) => TrackStatus::Missing,
            _ => TrackStatus::Remote,
        }
    }

    /// A login is under way (code shown or coming).
    pub fn logging_in(&self) -> bool {
        matches!(self.lock().state, State::Connecting | State::Code { .. })
    }

    /// The tracks to have on the card. Starts their downloads (a Wi-Fi
    /// session) unless one is under way, which takes them on.
    /// `all`: the whole playlist (an alarm uses „Zufällig“).
    pub fn set_selected(&self, ids: BTreeSet<u64>, all: bool) {
        let mut inner = self.lock();
        if inner.selected != ids || inner.all != all {
            inner.selected = ids;
            inner.all = all;
            inner.version = inner.version.wrapping_add(1);
        }
        let missing = inner
            .playlist
            .iter()
            .any(|t| inner.wants(t.id) && !inner.have.contains(&t.id));
        if !missing || inner.busy || inner.cfg.session.is_none() {
            return;
        }
        inner.busy = true;
        drop(inner);
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            match net {
                Ok(()) => this.keep_up(),
                Err(e) => log::warn!("tidal: downloads: {e}"),
            }
            this.lock().busy = false;
        }));
    }

    /// The TIDAL tab opened: logs in if there is no session, else syncs the
    /// playlist, and keeps it fresh while the tab is open.
    pub fn open(&self) {
        let mut inner = self.lock();
        inner.wanted = true;
        if inner.busy || !inner.cfg.has_credentials() {
            return;
        }
        let sync = inner.cfg.session.is_some();
        let login = inner.cfg.session.is_none()
            && matches!(inner.state, State::LoggedOut | State::Failed(_));
        if !sync && !login {
            return;
        }
        inner.busy = true;
        inner.uuid = None;
        inner.state = if sync { State::Syncing } else { State::Connecting };
        drop(inner);
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            match net {
                Ok(()) if sync => this.sync_or_login(),
                Ok(()) => this.login_and_sync(),
                Err(e) => this.lock().state = State::Failed(e),
            }
            this.keep_up();
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

    /// After a sync: downloads what the card lacks, and while the tab is
    /// open reads the playlist again every [`WATCH`]. Ends when the tab is
    /// closed and nothing is left to download (or on an error).
    fn keep_up(&self) {
        let mut failed = BTreeSet::new();
        let mut last_sync = Instant::now();
        loop {
            if self.lock().state != State::LoggedIn {
                return;
            }
            let wanted = self.lock().wanted;
            let due = wanted && last_sync.elapsed() >= WATCH;
            if !due && let Some(track) = self.next_missing(&failed) {
                if let Err(e) = self.download(&track) {
                    log::warn!("tidal: download {}: {e}", track.id);
                    failed.insert(track.id);
                    if e == SESSION_INVALID {
                        self.lock().state = State::Failed(e);
                        return;
                    }
                }
                continue;
            }
            if !wanted {
                return;
            }
            if let Some(rest) = WATCH.checked_sub(last_sync.elapsed()) {
                std::thread::sleep(rest);
            }
            if !self.lock().wanted {
                continue; // downloads may remain
            }
            last_sync = Instant::now();
            if let Err(e) = self.sync() {
                log::warn!("tidal: watch: {e}");
                self.lock().state = State::Failed(e);
                return;
            }
        }
    }

    /// The first track of the playlist the card lacks (not failed before).
    fn next_missing(&self, failed: &BTreeSet<u64>) -> Option<Track> {
        let inner = self.lock();
        inner
            .playlist
            .iter()
            .find(|t| inner.wants(t.id) && !inner.have.contains(&t.id) && !failed.contains(&t.id))
            .cloned()
    }

    /// The download under way and how far (0–1), for the progress bar.
    pub fn progress(&self) -> Option<(u64, f32)> {
        self.lock().downloading
    }

    /// Records progress; a new track also refills the list.
    fn set_progress(&self, id: u64, fraction: f32) {
        let mut inner = self.lock();
        let new = inner.downloading.is_none_or(|(d, _)| d != id);
        inner.downloading = Some((id, fraction.clamp(0.0, 1.0)));
        if new {
            inner.version = inner.version.wrapping_add(1);
        }
    }

    /// Downloads one track to `tidal/<id>.flac`.
    fn download(&self, track: &Track) -> Result<(), String> {
        let s = self.session()?;
        let (status, body) = send(&api::playback_info(&s, track.id), "stream")?;
        let plan = api::parse_playback_info(status, &body, track.id, f64::from(track.duration_s))
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(sd_path(sync::DIR)).map_err(|e| e.to_string())?;
        let path = sd_path(&sync::track_path(track.id));
        let part = path.with_extension("flac.part");
        let t = Instant::now();
        self.set_progress(track.id, 0.0);
        // The first download only: each start takes a timer and a buffer.
        // 1 kHz: the buffer lasts 16 s, about one track.
        #[cfg(feature = "profile-download")]
        let profiled = {
            static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            let first = !DONE.swap(true, std::sync::atomic::Ordering::Relaxed);
            if first {
                crate::profiler::start_at(1000);
            }
            first
        };
        let result = self.fetch(track.id, &plan, &part);
        #[cfg(feature = "profile-download")]
        if profiled {
            crate::profiler::report(300);
        }
        self.lock().downloading = None;
        let bytes = match result {
            Ok(b) => b,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                self.bump();
                return Err(e);
            }
        };
        // Readable as FLAC, or it does not count.
        let check = AudioFile::open(&part).map_err(|e| e.to_string()).and_then(|f| {
            (f.format.container == Container::Flac && f.format.sample_rate > 0)
                .then_some(())
                .ok_or_else(|| "kein FLAC".to_string())
        });
        if let Err(e) = check {
            let _ = std::fs::remove_file(&part);
            self.bump();
            return Err(format!("Datei unbrauchbar: {e}"));
        }
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        log::info!(
            "tidal: downloaded {} ({} KiB in {} s)",
            track.id,
            bytes / 1024,
            t.elapsed().as_secs()
        );
        let mut inner = self.lock();
        inner.have.insert(track.id);
        inner.version = inner.version.wrapping_add(1);
        Ok(())
    }

    /// Feature `download-bench`: downloads the playlist's first track to a
    /// scratch file twice, then once more without writing it, and logs the
    /// speed of each and the Wi-Fi link (`bench:` lines).
    #[cfg(feature = "download-bench")]
    pub fn bench(&self) {
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            if let Err(e) = net {
                log::warn!("bench: no network: {e}");
                return;
            }
            let Some(track) = this.lock().playlist.first().cloned() else {
                log::warn!("bench: no playlist");
                return;
            };
            let plan = match this.session().and_then(|s| {
                let (status, body) = send(&api::playback_info(&s, track.id), "stream")?;
                api::parse_playback_info(status, &body, track.id, f64::from(track.duration_s))
                    .map_err(|e| e.to_string())
            }) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("bench: {e}");
                    return;
                }
            };
            let _ = std::fs::create_dir_all(sd_path(sync::DIR));
            let part = sd_path("tidal/bench.part");
            for round in 1..=2 {
                // Round 2 sampled whole (56 s at 250 Hz fit the buffer).
                #[cfg(feature = "profile-download")]
                if round == 2 {
                    crate::profiler::start_at(250);
                }
                let t = Instant::now();
                let result = this.fetch(track.id, &plan, &part);
                let ms = t.elapsed().as_millis().max(1) as u64;
                #[cfg(feature = "profile-download")]
                if round == 2 {
                    crate::profiler::report(800);
                }
                match result {
                    Ok(bytes) => log::info!(
                        "bench: round {round}: {} KiB in {ms} ms, {} KiB/s",
                        bytes / 1024,
                        bytes * 1000 / 1024 / ms
                    ),
                    Err(e) => log::warn!("bench: round {round}: {e}"),
                }
                let _ = std::fs::remove_file(&part);
            }
            // The link: signal and Wi-Fi mode.
            let mut ap: esp_idf_svc::sys::wifi_ap_record_t = unsafe { core::mem::zeroed() };
            // SAFETY: fills `ap`.
            if unsafe { esp_idf_svc::sys::esp_wifi_sta_get_ap_info(&mut ap) } == 0 {
                log::info!(
                    "bench: link: RSSI {} dBm, channel {}, 11b/g/n/ax {}{}{}{}, bandwidth {}",
                    ap.rssi,
                    ap.primary,
                    ap.phy_11b(),
                    ap.phy_11g(),
                    ap.phy_11n(),
                    ap.phy_11ax(),
                    ap.second
                );
            }
            // Round 3: the network alone, nothing written to the card.
            if let MediaPlan::Mirrors(urls) = &plan {
                let t = Instant::now();
                let result = http::download(&urls[0], &mut |_| Ok(()), &mut |_, _| {});
                let ms = t.elapsed().as_millis().max(1) as u64;
                match result {
                    Ok(bytes) => log::info!(
                        "bench: round 3 (no card): {} KiB in {ms} ms, {} KiB/s",
                        bytes / 1024,
                        bytes * 1000 / 1024 / ms
                    ),
                    Err(e) => log::warn!("bench: round 3: {e}"),
                }
            }
            this.lock().downloading = None;
            this.bump();
        }));
    }

    fn bump(&self) {
        let mut inner = self.lock();
        inner.version = inner.version.wrapping_add(1);
    }

    /// Writes the track to `part`; the bytes written.
    fn fetch(&self, id: u64, plan: &MediaPlan, part: &std::path::Path) -> Result<u64, String> {
        let open = || CardWriter::create(part).map_err(|e| e.to_string());
        match plan {
            MediaPlan::Mirrors(urls) => {
                log::info!("tidal: download {id}: one file, {} mirrors", urls.len());
                let mut last = String::from("keine Quelle");
                for url in urls {
                    let mut file = open()?;
                    let mut written = 0u64;
                    // Time spent writing to the card (the rest: network,
                    // TLS, waiting).
                    let writing = std::cell::Cell::new(Duration::ZERO);
                    let mut sink = |b: &[u8]| -> anyhow::Result<()> {
                        let w = Instant::now();
                        file.write_all(b)?;
                        writing.set(writing.get() + w.elapsed());
                        written += b.len() as u64;
                        Ok(())
                    };
                    let t = Instant::now();
                    let mut logged = 0u64;
                    let mut progress = |done: u64, total: Option<u64>| {
                        if let Some(total) = total.filter(|t| *t > 0) {
                            self.set_progress(id, done as f32 / total as f32);
                        }
                        if done >= logged + (2 << 20) {
                            logged = done;
                            log::info!(
                                "tidal: download {id}: {} KiB of {:?} KiB, {} KiB/s, {} of {} ms writing to the card",
                                done / 1024,
                                total.map(|t| t / 1024),
                                done / 1024 / t.elapsed().as_secs().max(1),
                                writing.get().as_millis(),
                                t.elapsed().as_millis()
                            );
                        }
                    };
                    match http::download(url, &mut sink, &mut progress) {
                        Ok(_) => {
                            file.flush().map_err(|e| e.to_string())?;
                            return Ok(written);
                        }
                        Err(e) => last = e.to_string(),
                    }
                }
                Err(last)
            }
            MediaPlan::Segments(urls) => {
                log::info!("tidal: download {id}: {} DASH segments", urls.len());
                let t = Instant::now();
                let mut file = open()?;
                let mut unpacker = Unpacker::new();
                let mut out = Vec::new();
                let written = std::cell::Cell::new(0u64);
                // Bytes fed to the unpacker (it may take some without
                // giving any out).
                let fed = std::cell::Cell::new(0u64);
                for (i, url) in urls.iter().enumerate() {
                    self.set_progress(id, i as f32 / urls.len() as f32);
                    if i % 10 == 0 {
                        log::info!(
                            "tidal: download {id}: segment {i} of {}, {} KiB, {} s",
                            urls.len(),
                            written.get() / 1024,
                            t.elapsed().as_secs()
                        );
                    }
                    let mut sink = |b: &[u8]| -> anyhow::Result<()> {
                        fed.set(fed.get() + b.len() as u64);
                        out.clear();
                        unpacker.push(b, &mut out).map_err(|e| anyhow::anyhow!("{e}"))?;
                        file.write_all(&out)?;
                        written.set(written.get() + out.len() as u64);
                        Ok(())
                    };
                    // A segment that fails is tried once more only if it
                    // gave nothing yet (the unpacker cannot go back).
                    let before = fed.get();
                    let mut quiet = |_: u64, _: Option<u64>| {};
                    if let Err(e) = http::download(url, &mut sink, &mut quiet) {
                        log::warn!("tidal: download {id}: segment {i}: {e}");
                        if fed.get() != before {
                            return Err(e.to_string());
                        }
                        http::download(url, &mut sink, &mut quiet).map_err(|e| e.to_string())?;
                    }
                }
                unpacker.finish().map_err(|e| e.to_string())?;
                file.flush().map_err(|e| e.to_string())?;
                Ok(written.get())
            }
        }
    }

    fn drop_session(&self) {
        if let Err(e) = config::save_tidal_session(None) {
            log::error!("tidal: removing the session: {e}");
        }
        let mut inner = self.lock();
        inner.cfg.session = None;
        inner.uuid = None;
    }

    /// After a sync: the state.
    fn synced(&self, result: Result<usize, String>) {
        match result {
            Ok(n) => {
                log::info!("tidal: playlist synced, {n} tracks");
                self.lock().state = State::LoggedIn;
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

    /// Reads the playlist's tracks, on the `wifi` thread; a change goes to
    /// `tidal/playlist.json`. The number of tracks. The first sync in a
    /// Wi-Fi session also binds the API session and finds the playlist.
    fn sync(&self) -> Result<usize, String> {
        let s = self.session()?;
        let known = self.lock().uuid.clone();
        let uuid = match known {
            Some(u) => u,
            None => {
                // Binds an API session to the token; user calls fail
                // without it.
                let (status, body) = send(&api::sessions(&s), "session")?;
                api::parse_sessions(status, &body).map_err(|e| e.to_string())?;
                let name = self.lock().cfg.playlist.clone();
                let mut lists = Vec::new();
                let mut offset = Some(0);
                while let Some(o) = offset {
                    let (status, body) = send(&api::playlists(&s, o), "playlists")?;
                    let (page, next) =
                        api::parse_playlists(status, &body, o).map_err(|e| e.to_string())?;
                    lists.extend(page);
                    offset = next;
                }
                let uuid = api::find_playlist(&lists, &name)
                    .ok_or_else(|| format!("Keine Playlist „{name}“ bei TIDAL."))?
                    .uuid
                    .clone();
                self.lock().uuid = Some(uuid.clone());
                uuid
            }
        };
        let mut tracks = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) = send(&api::playlist_items(&s, &uuid, o), "playlist")?;
            let (page, next) =
                api::parse_playlist_items(status, &body, o).map_err(|e| e.to_string())?;
            tracks.extend(page);
            offset = next;
        }
        let n = tracks.len();
        // Files of tracks no longer in the playlist go.
        let have: Vec<u64> = self.lock().have.iter().copied().collect();
        for id in sync::plan(&tracks, &have, &[]).delete_tracks {
            match std::fs::remove_file(sd_path(&sync::track_path(id))) {
                Ok(()) => log::info!("tidal: removed {id} (not in the playlist)"),
                Err(e) => log::warn!("tidal: removing {id}: {e}"),
            }
            self.lock().have.remove(&id);
        }
        if self.lock().playlist != tracks {
            save_playlist(&tracks).map_err(|e| format!("Speichern: {e}"))?;
            log::info!("tidal: playlist changed, {n} tracks");
            let mut inner = self.lock();
            inner.playlist = tracks;
            inner.version = inner.version.wrapping_add(1);
        }
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
