//! Syncing the playlist and its covers.

use super::session::{Error, Result};
use super::store::{self, SyncState};
use super::{Inner, Tidal};
use crate::config::TidalSession;
use crate::http;
use crate::runtime::Thread;
use esp_idf_svc::hal::cpu::Core;
use std::collections::BTreeSet;
use std::time::Instant;
use wakey_core::tidal::api::{self, PlaylistInfo, Track};
use wakey_core::tidal::{cover, sync};

impl Tidal {
    /// Reads the playlist's tracks, on the `wifi` thread; a change goes to
    /// `tidal/playlist.json`. The number of tracks. Usually one request on
    /// a kept connection, the playlist's `lastUpdated`; its tracks are read
    /// only if that changed, and the playlist is searched by name only when
    /// its id is not known (or no longer valid).
    pub(super) fn sync(&self) -> Result<usize> {
        let t = Instant::now();
        let s = self.session()?;
        let (uuid, info) = self.resolve_playlist(&s)?;
        let last_updated = info.map(|i| i.last_updated);
        let unchanged = {
            let inner = self.lock();
            let same = last_updated.is_some()
                && inner.last_updated == last_updated
                && !inner.playlist.is_empty();
            same.then(|| (inner.playlist.len(), inner.with_listed_orphans()))
        };
        if let Some((n, listed)) = unchanged {
            log::info!(
                "tidal: playlist unchanged ({n} tracks), {} ms",
                t.elapsed().as_millis()
            );
            self.sync_covers(&listed);
            return Ok(n);
        }
        let tracks = self.read_tracks(&s, &uuid)?;
        log::info!("tidal: playlist read, {} ms", t.elapsed().as_millis());
        let n = tracks.len();
        self.take_tracks(tracks)?;
        // Only once the tracks are stored: a playlist that could not be
        // written is read again by the next sync.
        self.lock().last_updated = last_updated.clone();
        store::save_state(&SyncState {
            playlist: self.lock().cfg.playlist.clone(),
            uuid: Some(uuid),
            last_updated,
        });
        let listed = self.lock().with_listed_orphans();
        self.sync_covers(&listed);
        Ok(n)
    }

    /// The playlist's id and its data: the kept id if it is still there
    /// under the configured name, else the playlist found by name.
    fn resolve_playlist(&self, s: &TidalSession) -> Result<(String, Option<PlaylistInfo>)> {
        let (name, kept) = {
            let inner = self.lock();
            (inner.cfg.playlist.clone(), inner.uuid.clone())
        };
        if let Some(uuid) = kept {
            let (status, body) = self.call(s, &api::playlist(s, &uuid), "playlist")?;
            match api::parse_playlist(status, &body)? {
                Some(i) if i.title.trim().eq_ignore_ascii_case(name.trim()) => {
                    return Ok((uuid, Some(i)));
                }
                _ => log::info!("tidal: kept playlist gone or renamed, searching by name"),
            }
        }
        let uuid = self.find_playlist(s)?;
        let (status, body) = self.call(s, &api::playlist(s, &uuid), "playlist")?;
        let info = api::parse_playlist(status, &body)?;
        let mut inner = self.lock();
        inner.uuid = Some(uuid.clone());
        inner.last_updated = None;
        Ok((uuid, info))
    }

    /// All tracks of the playlist, page by page.
    fn read_tracks(&self, s: &TidalSession, uuid: &str) -> Result<Vec<Track>> {
        let mut tracks = Vec::new();
        let mut offset = Some(0);
        while let Some(o) = offset {
            let (status, body) =
                self.call(s, &api::playlist_items(s, uuid, o), "playlist items")?;
            let (page, next) = api::parse_playlist_items(status, &body, o)?;
            tracks.extend(page);
            offset = next;
        }
        Ok(tracks)
    }

    /// The playlist as read: remembered ([`store::KNOWN_FILE`]), the files
    /// of tracks that left it removed unless still used, stored.
    fn take_tracks(&self, tracks: Vec<Track>) -> Result<()> {
        // Every track seen is remembered (an alarm may keep it after it
        // leaves the playlist).
        let known_changed = {
            let mut inner = self.lock();
            let mut changed = false;
            for t in &tracks {
                if inner.known.get(&t.id) != Some(t) {
                    inner.known.insert(t.id, t.clone());
                    changed = true;
                }
            }
            changed
        };
        self.prune_files(&tracks);
        if self.lock().playlist != tracks {
            store::save_playlist(&tracks)?;
            log::info!("tidal: playlist changed, {} tracks", tracks.len());
            let mut inner = self.lock();
            inner.playlist = tracks;
            inner.bump();
        }
        // Remembered: the playlist, what alarms use, what is on the card.
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
            store::save_known(&inner.known);
        }
        Ok(())
    }

    /// Removes the files of tracks no longer in `tracks`, unless an alarm
    /// uses them or the open tab still lists them. Unfinished downloads
    /// (`.flac.part`) go too, except those to be resumed of tracks that
    /// stay.
    fn prune_files(&self, tracks: &[Track]) {
        let (have, spare): (Vec<u64>, BTreeSet<u64>) = {
            let inner = self.lock();
            (
                inner.have.iter().copied().collect(),
                inner.selected.union(&inner.keep).copied().collect(),
            )
        };
        let gone = sync::plan(tracks, &have, &[]).delete_tracks;
        for id in gone.into_iter().filter(|id| !spare.contains(id)) {
            match std::fs::remove_file(store::track_path(id)) {
                Ok(()) => log::info!("tidal: removed {id} (not in the playlist)"),
                Err(e) => log::warn!("tidal: removing {id}: {e}"),
            }
            self.lock().have.remove(&id);
        }
        let keep = {
            let mut inner = self.lock();
            inner
                .resumable
                .retain(|id| spare.contains(id) || tracks.iter().any(|t| t.id == *id));
            // Syncs and downloads share the `wifi` thread, so none is under
            // way; kept all the same.
            let mut keep = inner.resumable.clone();
            keep.extend(inner.downloading.map(|(id, _)| id));
            keep
        };
        store::remove_parts(&keep);
    }

    /// The covers of `tracks` the card lacks, one by one (the list is filled
    /// again after each); covers of tracks no longer listed go.
    fn sync_covers(&self, tracks: &[Track]) {
        let plan = sync::plan(tracks, &[], &store::scan_covers());
        for id in plan.delete_covers {
            store::remove_cover(&id);
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
}

/// Downloads one cover (TIDAL's JPEG, no login needed) and stores it
/// decoded ([`cover::decode`]).
fn fetch_cover(id: &str) -> Result<()> {
    let mut jpeg = Vec::new();
    http::download(
        &api::cover_url(id, cover::SOURCE_PX),
        &mut |b| {
            jpeg.extend_from_slice(b);
            Ok(())
        },
        &mut |_, _| {},
    )?;
    let rgb = decode_cover(&jpeg)?;
    store::save_cover(id, &rgb)?;
    Ok(())
}

/// [`cover::decode`] on a thread of its own with a 64 KiB stack in PSRAM:
/// the JPEG decoder keeps its Huffman tables in itself (~25 KB on the
/// stack), more than the `wifi` thread's 16 KiB. On core 1 at priority 1:
/// unpinned it could run on core 0 above the UI and make animations
/// stutter.
fn decode_cover(jpeg: &[u8]) -> Result<Vec<u8>> {
    std::thread::scope(|s| {
        Thread::new(c"cover", 64 * 1024)
            .core(Core::Core1)
            .priority(1)
            .stack_in_psram()
            .spawn_scoped(s, || cover::decode(jpeg))?
            .join()
            .map_err(|_| Error::Unusable("cover decoder panicked".into()))?
            .map_err(Error::from)
    })
}
