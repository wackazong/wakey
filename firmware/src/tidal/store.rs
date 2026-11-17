//! What TIDAL keeps on the card, below `tidal/` ([`sync::DIR`]): the
//! tracks (`<id>.flac`, `<id>.flac.part` while downloading), the covers
//! (`covers/`), the playlist as last synced (`playlist.json`), the
//! playlist's id and version (`state.json`) and every track seen
//! (`known.json`). Readable without a session (offline, by the alarm).

use crate::board::sd;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use wakey_core::tidal::api::Track;
use wakey_core::tidal::{cover, sync};

/// The playlist found by name, kept across starts.
pub(super) const STATE_FILE: &str = "tidal/state.json";

/// Tracks seen in the playlist that an alarm may still use after they
/// left it (title, artist, cover for the list and the alarm page), by id.
pub(super) const KNOWN_FILE: &str = "tidal/known.json";

pub(super) type Known = BTreeMap<u64, Track>;

/// The playlist's id, kept so that it is searched by name only once.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct SyncState {
    /// The configured name it was found by.
    pub playlist: String,
    pub uuid: Option<String>,
    /// The playlist's `lastUpdated` when its tracks were last read.
    pub last_updated: Option<String>,
}

/// A track's file on the card (it may not be there).
pub(super) fn track_path(id: u64) -> PathBuf {
    sd::path(&sync::track_path(id))
}

/// A track's file while it downloads.
pub(super) fn part_path(id: u64) -> PathBuf {
    track_path(id).with_extension("flac.part")
}

fn covers_dir() -> PathBuf {
    sd::path(&format!("{}/covers", sync::DIR))
}

/// The tracks on the card.
pub(super) fn scan_tracks() -> BTreeSet<u64> {
    std::fs::read_dir(sd::path(sync::DIR))
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| sync::track_id_of(&e.file_name().to_string_lossy()))
                .collect()
        })
        .unwrap_or_default()
}

/// Removes the unfinished downloads (`<id>.flac.part`) of all tracks but
/// `keep`: nothing resumes them.
pub(super) fn remove_parts(keep: &BTreeSet<u64>) {
    let Ok(dir) = std::fs::read_dir(sd::path(sync::DIR)) else {
        return;
    };
    let stale = dir
        .flatten()
        .filter_map(|e| sync::part_id_of(&e.file_name().to_string_lossy()))
        .filter(|id| !keep.contains(id));
    // Collected first: no file is removed while the directory is read.
    for id in stale.collect::<Vec<_>>() {
        match std::fs::remove_file(part_path(id)) {
            Ok(()) => log::info!("tidal: removed the unfinished download of {id}"),
            Err(e) => log::warn!("tidal: removing the unfinished download of {id}: {e}"),
        }
    }
}

/// The covers on the card (ids).
pub(super) fn scan_covers() -> Vec<String> {
    std::fs::read_dir(covers_dir())
        .map(|dir| {
            dir.flatten()
                .filter_map(|e| {
                    sync::cover_id_of(&e.file_name().to_string_lossy()).map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `rel` on the card read as JSON, if it is there and readable.
fn load_json<T: serde::de::DeserializeOwned>(rel: &str) -> Option<T> {
    serde_json::from_slice(&std::fs::read(sd::path(rel)).ok()?).ok()
}

/// `value` as JSON into `rel` on the card, through a temporary file.
fn save_json(rel: &str, value: &impl serde::Serialize) -> std::io::Result<()> {
    let json = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    sd::replace(&sd::path(rel), &json)
}

pub(super) fn load_state() -> Option<SyncState> {
    load_json(STATE_FILE)
}

pub(super) fn save_state(state: &SyncState) {
    if let Err(e) = save_json(STATE_FILE, state) {
        log::warn!("tidal: {e}");
    }
}

/// The playlist as last synced (empty if none).
pub(super) fn load_playlist() -> Vec<Track> {
    load_json(sync::PLAYLIST_FILE).unwrap_or_default()
}

pub(super) fn save_playlist(tracks: &[Track]) -> std::io::Result<()> {
    std::fs::create_dir_all(sd::path(sync::DIR))?;
    save_json(sync::PLAYLIST_FILE, &tracks)
}

pub(super) fn load_known() -> Known {
    load_json(KNOWN_FILE).unwrap_or_default()
}

pub(super) fn save_known(known: &Known) {
    if let Err(e) = save_json(KNOWN_FILE, known) {
        log::warn!("tidal: {e}");
    }
}

/// Stores a decoded cover.
pub(super) fn save_cover(id: &str, rgb: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(covers_dir())?;
    sd::replace(&sd::path(&sync::cover_path(id)), rgb)
}

pub(super) fn remove_cover(id: &str) {
    let _ = std::fs::remove_file(sd::path(&sync::cover_path(id)));
}

/// The playlist's tracks that are on the card, in playlist order (for the
/// alarm: `tidal:random`).
pub fn downloaded() -> Vec<Track> {
    let have = scan_tracks();
    load_playlist()
        .into_iter()
        .filter(|t| have.contains(&t.id))
        .collect()
}

/// The file of a track, if it is on the card.
pub fn track_file(id: u64) -> Option<PathBuf> {
    let path = track_path(id);
    path.exists().then_some(path)
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

/// A cover as stored (RGB888, [`cover::COVER_BYTES`]), if it is on the
/// card.
pub fn cover_rgb(cover: &str) -> Option<Vec<u8>> {
    let rgb = std::fs::read(sd::path(&sync::cover_path(cover))).ok()?;
    (rgb.len() == cover::COVER_BYTES).then_some(rgb)
}
