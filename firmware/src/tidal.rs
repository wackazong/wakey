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
//! [`Tidal::listed`] serves (offline too). While the tab stays open the
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
//! not tried again until the next start (its row says so, it cannot be
//! picked; [`TrackStatus::Failed`]).
//!
//! The state is shared with the UI thread ([`Tidal::state`]); the work runs
//! on the `wifi` thread.

use crate::board::sd;
use crate::config::{self, TidalConfig, TidalSession};
use crate::http;
use crate::net::Wifi;
use crate::psram::PsramBuffer;
use crate::runtime::Thread;
use esp_idf_svc::hal::cpu::Core;
use std::collections::BTreeSet;
use std::io::Write;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wakey_core::audio_file::{AudioFile, Container};
use wakey_core::tidal::api::{self, Track};
use wakey_core::tidal::auth::{self, Poll};
use wakey_core::tidal::flac_mp4::Unpacker;
use wakey_core::tidal::manifest::MediaPlan;
use wakey_core::tidal::{cover, sync};

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
    /// Logged in; the playlist ([`Tidal::listed`]) is as fresh as it gets.
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
    /// The playlist's id (kept in [`STATE_FILE`]: no search by name once
    /// found) and its `lastUpdated` when its tracks were last read (the
    /// tracks are read again only when it changed).
    uuid: Option<String>,
    last_updated: Option<String>,
    /// The access token (hash) bound with `/sessions` (once per token).
    bound: Option<u64>,
    /// Tracks on the card (`tidal/<id>.flac`).
    have: BTreeSet<u64>,
    /// Tracks to have on the card (chosen in alarms).
    selected: BTreeSet<u64>,
    /// An alarm uses „Zufällig“: the whole playlist.
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
    /// Tracks seen in the playlist ([`KNOWN_FILE`]).
    known: Known,
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

/// The tracks on the card.
fn scan_tracks() -> BTreeSet<u64> {
    std::fs::read_dir(sd::path(sync::DIR))
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
/// A download stopped for a track picked meanwhile; tried again later.
const DEFERRED: &str = "zurückgestellt";
/// TIDAL wants `/sessions` called for this token first (401, subStatus
/// 6004): bound, then the request again (not a bad session).
const UNBOUND: &str = "API-Sitzung fehlt";

/// The playlist found by name, kept across starts.
const STATE_FILE: &str = "tidal/state.json";

/// An alarm's settings pages are open (`app::show_page`): a Wi-Fi session
/// started for them ([`Tidal::warm`]) stays up, so the TIDAL tab finds the
/// playlist synced and the network ready.
static EDITING: AtomicBool = AtomicBool::new(false);

/// The alarm, sound or name page is shown (or not).
pub fn set_editing(on: bool) {
    EDITING.store(on, Ordering::Relaxed);
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SyncState {
    /// The configured name it was found by.
    playlist: String,
    uuid: Option<String>,
    last_updated: Option<String>,
}

fn load_state() -> Option<SyncState> {
    serde_json::from_slice(&std::fs::read(sd::path(STATE_FILE)).ok()?).ok()
}

fn save_state(state: &SyncState) {
    if let Err(e) = save_json(STATE_FILE, state) {
        log::warn!("tidal: {e}");
    }
}

/// `value` as JSON into `rel` on the card, through a temporary file.
fn save_json(rel: &str, value: &impl serde::Serialize) -> std::io::Result<()> {
    let json = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    sd::replace(&sd::path(rel), &json)
}

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
            let unbound =
                serde_json::from_slice::<Reason>(&body).is_ok_and(|r| r.sub_status == Some(6004));
            return Err(if unbound { UNBOUND } else { SESSION_INVALID }.into());
        }
    }
    Ok((status, body))
}

/// The playlist as last synced (empty if none).
fn load_playlist() -> Vec<Track> {
    std::fs::read(sd::path(sync::PLAYLIST_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The playlist's tracks that are on the card, in playlist order (for the
/// alarm: `tidal:random`), and the file of one track if it is there.
pub fn downloaded() -> Vec<Track> {
    let have = scan_tracks();
    load_playlist()
        .into_iter()
        .filter(|t| have.contains(&t.id))
        .collect()
}

/// A cover as stored (RGB888, [`cover::COVER_BYTES`]), if it is on the
/// card.
pub fn cover_rgb(cover: &str) -> Option<Vec<u8>> {
    let rgb = std::fs::read(sd::path(&sync::cover_path(cover))).ok()?;
    (rgb.len() == cover::COVER_BYTES).then_some(rgb)
}

/// The covers on the card (ids).
fn scan_covers() -> Vec<String> {
    std::fs::read_dir(sd::path(&format!("{}/covers", sync::DIR)))
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| {
                    sync::cover_id_of(&e.file_name().to_string_lossy()).map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// [`cover::decode`] on a thread of its own with a 64 KiB stack in PSRAM:
/// the JPEG decoder keeps its Huffman tables in itself (~25 KB on the
/// stack), more than the `wifi` thread's 16 KiB (stack protection fault).
fn decode_cover(jpeg: &[u8]) -> Result<Vec<u8>, String> {
    std::thread::scope(|s| {
        Thread::new(c"cover", 64 * 1024)
            .stack_in_psram()
            .spawn_scoped(s, || cover::decode(jpeg).map_err(|e| e.to_string()))
            .map_err(|e| format!("cover thread: {e}"))?
            .join()
            .map_err(|_| "cover: decoder panicked".to_string())?
    })
}

/// Downloads one cover (TIDAL's JPEG, no login needed) and stores it
/// decoded ([`cover::decode`]).
fn fetch_cover(id: &str) -> Result<(), String> {
    let mut jpeg = Vec::new();
    http::download(
        &api::cover_url(id, cover::SOURCE_PX),
        &mut |b| {
            jpeg.extend_from_slice(b);
            Ok(())
        },
        &mut |_, _| {},
    )
    .map_err(|e| e.to_string())?;
    let rgb = decode_cover(&jpeg)?;
    let _ = std::fs::create_dir_all(sd::path(&format!("{}/covers", sync::DIR)));
    sd::replace(&sd::path(&sync::cover_path(id)), &rgb).map_err(|e| e.to_string())
}

pub fn track_file(id: u64) -> Option<PathBuf> {
    let path = sd::path(&sync::track_path(id));
    path.exists().then_some(path)
}

/// Tracks seen in the playlist that an alarm may still use after they
/// left it (title, artist, cover for the list and the alarm page), by id.
const KNOWN_FILE: &str = "tidal/known.json";

type Known = std::collections::BTreeMap<u64, Track>;

fn load_known() -> Known {
    std::fs::read(sd::path(KNOWN_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_known(known: &Known) {
    if let Err(e) = save_json(KNOWN_FILE, known) {
        log::warn!("tidal: {e}");
    }
}

/// A track's title and artist: from the stored playlist, else from the
/// tracks seen before ([`KNOWN_FILE`]: an alarm keeps its track's name
/// after the track left the playlist).
pub fn track_title(id: u64) -> Option<(String, String)> {
    load_playlist()
        .into_iter()
        .find(|t| t.id == id)
        .or_else(|| load_known().remove(&id))
        .map(|t| (t.title, t.artist))
}

/// A track being downloaded, read as it arrives (preview while
/// downloading): `tidal/<id>.flac.part` while it grows, the finished
/// `tidal/<id>.flac` once renamed. At the end of what has arrived it opens
/// the file again (FatFs fixes the size at opening; the writer syncs every
/// 256 KiB) and waits while the download goes on. Ends (no more data) when
/// the download fails, and with an error once `cancel` is set (the preview
/// stopped: the player's decoder must not wait for data).
pub struct Growing {
    id: u64,
    tidal: Tidal,
    cancel: Arc<AtomicBool>,
    file: Option<std::fs::File>,
    /// `file` is the finished track: its end is the end.
    done: bool,
    pos: u64,
}

impl Growing {
    /// Opens the finished file, else the part if it has more than `pos`.
    fn reopen(&mut self) -> std::io::Result<bool> {
        let path = sd::path(&sync::track_path(self.id));
        if let Ok(mut f) = std::fs::File::open(&path) {
            f.seek(SeekFrom::Start(self.pos))?;
            self.file = Some(f);
            self.done = true;
            return Ok(true);
        }
        if let Ok(mut f) = std::fs::File::open(path.with_extension("flac.part"))
            && f.metadata()?.len() > self.pos
        {
            f.seek(SeekFrom::Start(self.pos))?;
            self.file = Some(f);
            return Ok(true);
        }
        Ok(false)
    }
}

impl Read for Growing {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Err(std::io::Error::other("preview stopped"));
            }
            if let Some(f) = &mut self.file {
                let n = f.read(buf)?;
                if n > 0 {
                    self.pos += n as u64;
                    return Ok(n);
                }
                if self.done {
                    return Ok(0);
                }
                self.file = None;
            }
            if self.reopen()? {
                continue;
            }
            match self.tidal.track_status(self.id) {
                TrackStatus::Downloading | TrackStatus::Missing => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                // Renamed between the two looks: the finished file is there.
                TrackStatus::Ready if sd::path(&sync::track_path(self.id)).exists() => {}
                _ => return Ok(0),
            }
        }
    }
}

impl Seek for Growing {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => match (&self.file, self.done) {
                (Some(f), true) => f.metadata()?.len().checked_add_signed(d),
                _ => return Err(std::io::Error::other("end of a file still downloading")),
            },
        }
        .ok_or_else(|| std::io::Error::other("seek out of range"))?;
        self.pos = pos;
        if let Some(f) = &mut self.file {
            f.seek(SeekFrom::Start(pos))?;
        }
        Ok(pos)
    }
}

/// Writes a file on its own thread, in whole 32 KiB blocks from cache-line
/// aligned buffers, so the network and the card work at the same time.
///
/// 32 KiB: the card and the C6 share the SDMMC controller, one transfer at
/// a time; a 128 KiB write held the C6 off long enough to slow the download
/// (1250 against 1420 KiB/s; 16 KiB: as 32).
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
    /// All blocks, one allocation (freed after the writer is joined:
    /// fields drop after [`Drop::drop`]).
    _region: PsramBuffer,
}

const BLOCK: usize = 32 * 1024;
const BLOCKS: usize = 16;

impl CardWriter {
    /// Writes `path` from the start.
    fn create(path: &std::path::Path) -> std::io::Result<Self> {
        Self::open_at(path, 0)
    }

    /// Writes `path` from byte `at` on (a resumed download; what follows
    /// `at` goes). `at` a multiple of [`BLOCK`] keeps the card writes
    /// aligned.
    fn open_at(path: &std::path::Path, at: u64) -> std::io::Result<Self> {
        let mut file = if at == 0 {
            std::fs::File::create(path)?
        } else {
            let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
            f.set_len(at)?;
            f.seek(SeekFrom::Start(at))?;
            f
        };
        let region = PsramBuffer::new(BLOCK * BLOCKS, esp_idf_svc::sys::MALLOC_CAP_8BIT)
            .ok_or_else(|| std::io::Error::other("no memory for the write buffers"))?;
        let (to_writer, from_main) =
            std::sync::mpsc::sync_channel::<Option<(usize, usize)>>(BLOCKS);
        let (to_main, free) = std::sync::mpsc::sync_channel::<usize>(BLOCKS);
        for k in 1..BLOCKS {
            let _ = to_main.send(region.as_mut_ptr() as usize + k * BLOCK);
        }
        let writer = Thread::new(c"card-writer", 6144)
            .core(Core::Core1)
            .priority(4)
            .spawn(move || {
                let mut blocks = 0u32;
                while let Ok(Some((addr, len))) = from_main.recv() {
                    // SAFETY: the block is this thread's until sent back.
                    let data = unsafe { core::slice::from_raw_parts(addr as *const u8, len) };
                    file.write_all(data)?;
                    let _ = to_main.send(addr);
                    // FatFs updates a file's size on the card only on
                    // sync; a reader opening it (preview while
                    // downloading, [`Growing`]) sees that size. After the
                    // first block, then every 256 KiB.
                    blocks += 1;
                    if blocks == 1 || blocks.is_multiple_of(8) {
                        file.sync_all()?;
                    }
                }
                file.flush()
            });
        Ok(Self {
            to_writer: Some(to_writer),
            free,
            writer: Some(writer?),
            cur: region.as_mut_ptr() as usize,
            len: 0,
            _region: region,
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
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("card writer panicked"))),
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
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    (self.cur as *mut u8).add(self.len),
                    take,
                )
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
            let tx = self
                .to_writer
                .as_ref()
                .ok_or_else(|| std::io::Error::other("card writer stopped"))?;
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
    }
}

fn save_playlist(tracks: &[Track]) -> std::io::Result<()> {
    std::fs::create_dir_all(sd::path(sync::DIR))?;
    save_json(sync::PLAYLIST_FILE, &tracks)
}

/// Handle on the TIDAL state. Cheap to clone.
#[derive(Clone)]
pub struct Tidal {
    inner: Arc<Mutex<Inner>>,
    /// Wakes the download job (`keep_up`) when the selection changes.
    kick: Arc<std::sync::Condvar>,
    wifi: Wifi,
}

impl Tidal {
    pub fn new(cfg: TidalConfig, wifi: Wifi) -> Self {
        // The playlist found before, if it is still the one configured.
        let kept = load_state()
            .filter(|s| s.playlist == cfg.playlist)
            .unwrap_or_default();
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
                uuid: kept.uuid,
                last_updated: kept.last_updated,
                bound: None,
                have: scan_tracks(),
                selected: BTreeSet::new(),
                all: false,
                downloading: None,
                failed: BTreeSet::new(),
                covers_failed: BTreeSet::new(),
                prefer: None,
                resumable: BTreeSet::new(),
                known: {
                    let mut known = load_known();
                    known.extend(load_playlist().into_iter().map(|t| (t.id, t)));
                    known
                },
                keep: BTreeSet::new(),
                draft: None,
            })),
            kick: Arc::new(std::sync::Condvar::new()),
            wifi,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
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
    ///
    /// `draft`: the track of the alarm being edited (the tab lists it even
    /// if it left the playlist).
    pub fn set_selected(&self, ids: BTreeSet<u64>, all: bool, draft: Option<u64>) {
        let mut inner = self.lock();
        if inner.selected != ids || inner.all != all || inner.draft != draft {
            inner.selected = ids;
            inner.all = all;
            inner.draft = draft;
            inner.version = inner.version.wrapping_add(1);
        }
        inner.keep_draft();
        let missing = inner.tracks().iter().any(|t| inner.lacks(t.id));
        self.kick.notify_all();
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
        inner.keep.clear();
        inner.keep_draft();
        // A job already up (started with the alarm page) follows at once.
        self.kick.notify_all();
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
        inner.state = if sync {
            State::Syncing
        } else {
            State::Connecting
        };
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
        let this = self.clone();
        self.wifi.run(Box::new(move |net| {
            match net {
                Ok(()) => this.sync_or_login(),
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
        let mut last_sync = Instant::now();
        loop {
            // AirDrop takes the C6: the session ends; the next one, after
            // it, starts again where this stopped.
            if self.lock().state != State::LoggedIn || crate::airdrop::wanted() {
                return;
            }
            let wanted = self.lock().wanted;
            let due = wanted && last_sync.elapsed() >= WATCH;
            // A picked track that is missing goes before the sync.
            let urgent = {
                let inner = self.lock();
                inner.prefer.is_some_and(|p| inner.lacks(p))
            };
            if (!due || urgent)
                && let Some(track) = self.next_missing()
            {
                if let Err(e) = self.download(&track) {
                    if e == DEFERRED {
                        log::info!("tidal: download {}: stopped for another track", track.id);
                        continue;
                    }
                    log::warn!("tidal: download {}: {e}", track.id);
                    if e == SESSION_INVALID {
                        // The session, not the track: tried again after
                        // the next login.
                        self.lock().state = State::Failed(e);
                        return;
                    }
                    self.lock().failed.insert(track.id);
                    self.bump();
                }
                continue;
            }
            if !wanted && !EDITING.load(Ordering::Relaxed) {
                return;
            }
            if !wanted {
                // Settings pages open, the tab not: the session waits for
                // it (checked every second; a pick or the tab wakes it).
                let inner = self.lock();
                if !inner.tracks().iter().any(|t| inner.lacks(t.id)) {
                    let _ = self.kick.wait_timeout(inner, Duration::from_secs(1));
                }
                continue;
            }
            if let Some(rest) = WATCH.checked_sub(last_sync.elapsed()) {
                // Until the next sync, or a pick (checked under the lock
                // the pick takes: no wake-up lost).
                let inner = self.lock();
                if !inner.tracks().iter().any(|t| inner.lacks(t.id)) {
                    let _ = self.kick.wait_timeout(inner, rest);
                }
                continue;
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
    fn next_missing(&self) -> Option<Track> {
        let inner = self.lock();
        let preferred = inner.prefer.filter(|&id| inner.lacks(id));
        let tracks = inner.tracks();
        tracks
            .iter()
            .find(|t| Some(t.id) == preferred)
            .or_else(|| tracks.iter().find(|t| inner.lacks(t.id)))
            .cloned()
    }

    /// `id` was picked: if it is not on the card, it is downloaded next,
    /// and a download of another track stops for it (tried again later).
    pub fn prefer(&self, id: u64) {
        self.lock().prefer = Some(id);
        self.kick.notify_all();
    }

    /// The download of `id` is to stop: another track was picked and is
    /// still missing, or AirDrop wants the C6 (the download resumes later).
    fn deferred(&self, id: u64) -> bool {
        if crate::airdrop::wanted() {
            return true;
        }
        let inner = self.lock();
        inner.prefer.is_some_and(|p| p != id && inner.lacks(p))
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
        Some(Growing {
            id,
            tidal: self.clone(),
            cancel,
            file: None,
            done: false,
            pos: 0,
        })
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
        let (status, body) = self.call(&s, &api::playback_info(&s, track.id), "stream")?;
        let plan = api::parse_playback_info(status, &body, track.id, f64::from(track.duration_s))
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(sd::path(sync::DIR)).map_err(|e| e.to_string())?;
        let path = sd::path(&sync::track_path(track.id));
        let part = path.with_extension("flac.part");
        let t = Instant::now();
        self.set_progress(track.id, 0.0);
        let result = self.fetch(track.id, &plan, &part).map_err(|e| {
            if self.deferred(track.id) {
                DEFERRED.into()
            } else {
                e
            }
        });
        self.lock().downloading = None;
        let bytes = match result {
            Ok(b) => b,
            // Stopped for another track: one file goes on later where it
            // stopped (DASH segments start over).
            Err(e) if e == DEFERRED && matches!(plan, MediaPlan::Mirrors(_)) => {
                self.lock().resumable.insert(track.id);
                self.bump();
                return Err(e);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                self.bump();
                return Err(e);
            }
        };
        // Readable as FLAC, or it does not count.
        let check = AudioFile::open(&part)
            .map_err(|e| e.to_string())
            .and_then(|f| {
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
                // A download stopped for another track goes on where it
                // stopped (whole blocks: aligned writes).
                let mut offset = if self.lock().resumable.remove(&id) {
                    let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
                    have / BLOCK as u64 * BLOCK as u64
                } else {
                    0
                };
                if offset > 0 {
                    log::info!("tidal: download {id}: resuming at {} KiB", offset / 1024);
                }
                let mut last = String::from("keine Quelle");
                let mut tries = urls
                    .iter()
                    .map(|u| (u, false))
                    .collect::<std::collections::VecDeque<_>>();
                while let Some((url, retried)) = tries.pop_front() {
                    if self.deferred(id) {
                        return Err(DEFERRED.into());
                    }
                    let mut file = if offset > 0 {
                        CardWriter::open_at(part, offset).map_err(|e| e.to_string())?
                    } else {
                        open()?
                    };
                    let mut written = 0u64;
                    // Time spent writing to the card (the rest: network,
                    // TLS, waiting).
                    let writing = std::cell::Cell::new(Duration::ZERO);
                    let mut sink = |b: &[u8]| -> anyhow::Result<()> {
                        anyhow::ensure!(!self.deferred(id), DEFERRED);
                        let w = Instant::now();
                        file.write_all(b)?;
                        writing.set(writing.get() + w.elapsed());
                        written += b.len() as u64;
                        Ok(())
                    };
                    let t = Instant::now();
                    let at = offset;
                    let mut logged = 0u64;
                    // Counted over the whole file when resumed.
                    let mut progress = |done: u64, total: Option<u64>| {
                        let (done, total) = (done + at, total.map(|t| t + at));
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
                    let result = if at > 0 {
                        http::download_range(url, Some(&format!("{at}-")), &mut sink, &mut progress)
                    } else {
                        http::download(url, &mut sink, &mut progress)
                    };
                    match result {
                        Ok(_) => {
                            file.flush().map_err(|e| e.to_string())?;
                            return Ok(at + written);
                        }
                        // The rest refused (no range, another file): this
                        // mirror once more from the start.
                        Err(e) if at > 0 && !retried && !self.deferred(id) => {
                            log::warn!(
                                "tidal: download {id}: resuming failed ({e}), from the start"
                            );
                            offset = 0;
                            tries.push_front((url, true));
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
                        anyhow::ensure!(!self.deferred(id), DEFERRED);
                        fed.set(fed.get() + b.len() as u64);
                        out.clear();
                        unpacker
                            .push(b, &mut out)
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
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
        // Another login may be another account.
        inner.uuid = None;
        inner.last_updated = None;
        inner.bound = None;
        let _ = std::fs::remove_file(sd::path(STATE_FILE));
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
        let mut inner = self.lock();
        inner.wanted = false;
        // Tracks that left the playlist and no alarm uses go at the next
        // sync.
        inner.keep.clear();
    }

    /// The session, refreshed (and saved) if its token is about to expire.
    fn session(&self) -> Result<TidalSession, String> {
        let (cfg, session) = {
            let inner = self.lock();
            let s = inner
                .cfg
                .session
                .clone()
                .ok_or("Nicht bei TIDAL angemeldet.")?;
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

    /// Binds the API session to the token (`/sessions`), once per token.
    fn bind(&self, s: &TidalSession) -> Result<(), String> {
        let token = {
            use std::hash::{Hash, Hasher};
            let mut h = std::hash::DefaultHasher::new();
            s.access_token.hash(&mut h);
            h.finish()
        };
        if self.lock().bound == Some(token) {
            return Ok(());
        }
        let (status, body) = send(&api::sessions(s), "session")?;
        api::parse_sessions(status, &body).map_err(|e| e.to_string())?;
        self.lock().bound = Some(token);
        Ok(())
    }

    /// An API call for `s`: bound first; if TIDAL still wants the binding
    /// (6004), bound again and the call repeated once.
    fn call(
        &self,
        s: &TidalSession,
        req: &wakey_core::tidal::Request,
        stage: &str,
    ) -> Result<(u16, Vec<u8>), String> {
        self.bind(s)?;
        match send(req, stage) {
            Err(e) if e == UNBOUND => {
                log::info!("tidal: {stage}: API session wanted, binding again");
                self.lock().bound = None;
                self.bind(s)?;
                send(req, stage)
            }
            result => result,
        }
    }

    /// The configured playlist's id: the one kept, else found by name.
    fn find_playlist(&self, s: &TidalSession) -> Result<String, String> {
        let name = self.lock().cfg.playlist.clone();
        let mut lists = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) = self.call(s, &api::playlists(s, o), "playlists")?;
            let (page, next) = api::parse_playlists(status, &body, o).map_err(|e| e.to_string())?;
            lists.extend(page);
            offset = next;
        }
        let uuid = api::find_playlist(&lists, &name)
            .ok_or_else(|| format!("Keine Playlist „{name}“ bei TIDAL."))?
            .uuid
            .clone();
        log::info!("tidal: playlist „{name}“ found");
        Ok(uuid)
    }

    /// Reads the playlist's tracks, on the `wifi` thread; a change goes to
    /// `tidal/playlist.json`. The number of tracks. Usually two requests on
    /// kept connections: the playlist's `lastUpdated`, and its tracks only
    /// if that changed; the playlist is searched by name only when its id
    /// is not known (or no longer valid).
    fn sync(&self) -> Result<usize, String> {
        let t = Instant::now();
        let s = self.session()?;
        let name = self.lock().cfg.playlist.clone();
        let mut uuid = self.lock().uuid.clone();
        // The kept id: still there, and still that name?
        let mut info = None;
        if let Some(u) = &uuid {
            let (status, body) = self.call(&s, &api::playlist(&s, u), "playlist")?;
            match api::parse_playlist(status, &body).map_err(|e| e.to_string())? {
                Some(i) if i.title.trim().eq_ignore_ascii_case(name.trim()) => info = Some(i),
                _ => {
                    log::info!("tidal: kept playlist gone or renamed, searching by name");
                    uuid = None;
                }
            }
        }
        let uuid = match uuid {
            Some(u) => u,
            None => {
                let u = self.find_playlist(&s)?;
                let (status, body) = self.call(&s, &api::playlist(&s, &u), "playlist")?;
                info = api::parse_playlist(status, &body).map_err(|e| e.to_string())?;
                let mut inner = self.lock();
                inner.uuid = Some(u.clone());
                inner.last_updated = None;
                u
            }
        };
        let last_updated = info.map(|i| i.last_updated);
        let unchanged = {
            let inner = self.lock();
            last_updated.is_some()
                && inner.last_updated == last_updated
                && !inner.playlist.is_empty()
        };
        if unchanged {
            let (n, listed) = {
                let inner = self.lock();
                let mut listed = inner.playlist.clone();
                listed.extend(inner.listed_orphans());
                (inner.playlist.len(), listed)
            };
            log::info!(
                "tidal: playlist unchanged ({n} tracks), {} ms",
                t.elapsed().as_millis()
            );
            self.sync_covers(&listed);
            return Ok(n);
        }
        let mut tracks = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) =
                self.call(&s, &api::playlist_items(&s, &uuid, o), "playlist items")?;
            let (page, next) =
                api::parse_playlist_items(status, &body, o).map_err(|e| e.to_string())?;
            tracks.extend(page);
            offset = next;
        }
        {
            let mut inner = self.lock();
            inner.last_updated = last_updated.clone();
        }
        save_state(&SyncState {
            playlist: name,
            uuid: Some(uuid),
            last_updated,
        });
        log::info!("tidal: playlist read, {} ms", t.elapsed().as_millis());
        let n = tracks.len();
        // Every track seen is remembered (an alarm may keep it after it
        // leaves the playlist).
        let mut known_changed = false;
        {
            let mut inner = self.lock();
            for t in &tracks {
                if inner.known.get(&t.id) != Some(t) {
                    inner.known.insert(t.id, t.clone());
                    known_changed = true;
                }
            }
        }
        // Files of tracks no longer in the playlist go, unless an alarm
        // uses them or the open tab still lists them.
        let (have, spare): (Vec<u64>, BTreeSet<u64>) = {
            let inner = self.lock();
            (
                inner.have.iter().copied().collect(),
                inner.selected.union(&inner.keep).copied().collect(),
            )
        };
        let gone = sync::plan(&tracks, &have, &[]).delete_tracks;
        for id in gone.into_iter().filter(|id| !spare.contains(id)) {
            match std::fs::remove_file(sd::path(&sync::track_path(id))) {
                Ok(()) => log::info!("tidal: removed {id} (not in the playlist)"),
                Err(e) => log::warn!("tidal: removing {id}: {e}"),
            }
            self.lock().have.remove(&id);
        }
        if self.lock().playlist != tracks {
            save_playlist(&tracks).map_err(|e| format!("Speichern: {e}"))?;
            log::info!("tidal: playlist changed, {n} tracks");
            let mut inner = self.lock();
            inner.playlist = tracks.clone();
            inner.version = inner.version.wrapping_add(1);
        }
        // Remembered: the playlist, what alarms use, what is on the card.
        let listed = {
            let mut inner = self.lock();
            let before = inner.known.len();
            let Inner {
                known,
                playlist,
                selected,
                keep,
                have,
                ..
            } = &mut *inner;
            known.retain(|id, _| {
                playlist.iter().any(|t| t.id == *id)
                    || selected.contains(id)
                    || keep.contains(id)
                    || have.contains(id)
            });
            if known_changed || inner.known.len() != before {
                save_known(&inner.known);
            }
            let mut listed = inner.playlist.clone();
            listed.extend(inner.listed_orphans());
            listed
        };
        self.sync_covers(&listed);
        Ok(n)
    }

    /// The covers of `tracks` the card lacks, one by one (the list is filled
    /// again after each); covers of tracks no longer in the playlist go.
    fn sync_covers(&self, tracks: &[Track]) {
        let plan = sync::plan(tracks, &[], &scan_covers());
        for id in plan.delete_covers {
            let _ = std::fs::remove_file(sd::path(&sync::cover_path(&id)));
        }
        let wanted: BTreeSet<String> = plan.download_covers.into_iter().collect();
        for id in wanted {
            if self.lock().covers_failed.contains(&id) {
                continue;
            }
            let t = Instant::now();
            match fetch_cover(&id) {
                Ok(()) => {
                    log::info!("tidal: cover {id} in {} ms", t.elapsed().as_millis());
                    self.bump();
                }
                Err(e) => {
                    log::warn!("tidal: cover {id}: {e}");
                    self.lock().covers_failed.insert(id);
                }
            }
        }
    }

    /// The device code login, on the `wifi` thread. True if logged in
    /// (false: the tab was closed).
    fn login(&self, cfg: &TidalConfig) -> Result<bool, String> {
        let (status, body) =
            http::send(&auth::device_authorization(cfg)).map_err(|e| e.to_string())?;
        let code = auth::parse_device_authorization(status, &body).map_err(|e| e.to_string())?;
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
                return Err("Der Code ist abgelaufen.".into());
            }
            let (status, body) = http::send(&auth::poll(cfg, &code)).map_err(|e| e.to_string())?;
            match auth::parse_poll(status, &body, crate::clock::moment().unix)
                .map_err(|e| e.to_string())?
            {
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
