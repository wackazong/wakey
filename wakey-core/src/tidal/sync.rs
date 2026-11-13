//! What a sync changes on the SD card. The playlist is kept as
//! `tidal/playlist.json` (the tracks, in order: shown offline), each track
//! as `tidal/<id>.flac`, each cover as `tidal/covers/<cover id>.rgb`
//! ([`COVER_PX`] square, RGB888, decoded once: [`super::cover`]).

use super::api::Track;
use std::collections::BTreeSet;

pub const DIR: &str = "tidal";
pub const PLAYLIST_FILE: &str = "tidal/playlist.json";
/// Cover edge in pixels, as stored and shown in the list (a row is 112 px
/// high). Stored at the size shown: Slint takes RGB888, and 160 px for
/// every track would not fit the heap.
pub const COVER_PX: u32 = 88;

/// Below the SD card root.
pub fn track_path(id: u64) -> String {
    format!("{DIR}/{id}.flac")
}

pub fn cover_path(cover: &str) -> String {
    format!("{DIR}/covers/{cover}.rgb")
}

/// A track id from a file name in `tidal/` (`"123.flac"`).
pub fn track_id_of(file_name: &str) -> Option<u64> {
    file_name.strip_suffix(".flac")?.parse().ok()
}

/// A cover id from a file name in `tidal/covers/`.
pub fn cover_id_of(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".rgb").filter(|s| !s.is_empty())
}

/// The changes, downloads in playlist order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub download_tracks: Vec<u64>,
    pub delete_tracks: Vec<u64>,
    pub download_covers: Vec<String>,
    pub delete_covers: Vec<String>,
}

/// Compares the playlist with what the card holds.
pub fn plan(playlist: &[Track], have_tracks: &[u64], have_covers: &[String]) -> Plan {
    let want_tracks: BTreeSet<u64> = playlist.iter().map(|t| t.id).collect();
    let want_covers: BTreeSet<&str> = playlist.iter().filter_map(|t| t.cover.as_deref()).collect();
    let have: BTreeSet<u64> = have_tracks.iter().copied().collect();
    let have_c: BTreeSet<&str> = have_covers.iter().map(String::as_str).collect();
    let mut seen_c = BTreeSet::new();
    let mut seen_t = BTreeSet::new();
    Plan {
        download_tracks: playlist
            .iter()
            .map(|t| t.id)
            .filter(|id| !have.contains(id) && seen_t.insert(*id))
            .collect(),
        delete_tracks: have.difference(&want_tracks).copied().collect(),
        download_covers: playlist
            .iter()
            .filter_map(|t| t.cover.as_deref())
            .filter(|c| !have_c.contains(c) && seen_c.insert(*c))
            .map(String::from)
            .collect(),
        delete_covers: have_c
            .difference(&want_covers)
            .map(|c| c.to_string())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, cover: Option<&str>) -> Track {
        Track {
            id,
            title: format!("T{id}"),
            artist: "A".into(),
            duration_s: 100,
            cover: cover.map(String::from),
        }
    }

    #[test]
    fn plans_downloads_in_order_and_deletions() {
        let list = [
            track(3, Some("c1")),
            track(1, Some("c1")),
            track(2, None),
            track(3, Some("c1")),
        ];
        let p = plan(&list, &[1, 9], &["c9".into()]);
        assert_eq!(p.download_tracks, vec![3, 2]);
        assert_eq!(p.delete_tracks, vec![9]);
        assert_eq!(p.download_covers, vec!["c1".to_string()]);
        assert_eq!(p.delete_covers, vec!["c9".to_string()]);
    }

    #[test]
    fn file_names() {
        assert_eq!(track_path(12), "tidal/12.flac");
        assert_eq!(track_id_of("12.flac"), Some(12));
        assert_eq!(track_id_of("12.flac.part"), None);
        assert_eq!(cover_id_of("ab-cd.rgb"), Some("ab-cd"));
    }
}
