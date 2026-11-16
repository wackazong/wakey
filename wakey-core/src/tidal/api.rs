//! The TIDAL API (v1, `Authorization: Bearer`, `countryCode` on every
//! call): the user's playlists, a playlist's tracks, covers, and the stream
//! manifest of a track.

use super::manifest::{self, MediaPlan};
use super::{Request, Result, ensure, json, percent_encode, status_error};
use crate::config::TidalSession;
use serde::{Deserialize, Serialize};

pub const API: &str = "https://api.tidal.com/v1";
/// Playlists and tracks per page (the API's maximum for tracks is 100).
const PLAYLISTS_PAGE: u32 = 50;
const TRACKS_PAGE: u32 = 100;

fn authed(url: String, session: &TidalSession) -> Request {
    Request::get(url).header("Authorization", format!("Bearer {}", session.access_token))
}

/// The API session for the token. Called first: without it TIDAL refuses
/// user calls with this client's tokens ("Invalid access token without
/// sessionId", HTTP 401, subStatus 6004).
pub fn sessions(session: &TidalSession) -> Request {
    authed(format!("{API}/sessions"), session)
}

/// Checks the answer to [`sessions`]: the user and country it reports.
pub fn parse_sessions(status: u16, body: &[u8]) -> Result<(u64, String)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer {
        user_id: u64,
        country_code: String,
    }
    ensure!(status == 200, "{}", status_error("session", status));
    let a: Answer = json("session", body)?;
    Ok((a.user_id, a.country_code))
}

/// One page of the user's own playlists.
pub fn playlists(session: &TidalSession, offset: u32) -> Request {
    authed(
        format!(
            "{API}/users/{}/playlists?countryCode={}&limit={PLAYLISTS_PAGE}&offset={offset}",
            session.user_id,
            percent_encode(&session.country_code)
        ),
        session,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub uuid: String,
    pub title: String,
    pub tracks: u32,
}

/// A page of playlists, and the offset of the next page (`None`: last).
pub fn parse_playlists(
    status: u16,
    body: &[u8],
    offset: u32,
) -> Result<(Vec<Playlist>, Option<u32>)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Page {
        /// Absent: this is the last page.
        total_number_of_items: Option<u32>,
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        uuid: String,
        title: String,
        #[serde(default)]
        number_of_tracks: u32,
    }
    ensure!(status == 200, "{}", status_error("playlists", status));
    let page: Page = json("playlists", body)?;
    let n = page.items.len() as u32;
    let next = offset + n;
    let more = n > 0 && page.total_number_of_items.is_some_and(|t| next < t);
    let items = page
        .items
        .into_iter()
        .map(|i| Playlist {
            uuid: i.uuid,
            title: i.title,
            tracks: i.number_of_tracks,
        })
        .collect();
    Ok((items, more.then_some(next)))
}

/// The playlist called `name` (case and surrounding spaces ignored).
pub fn find_playlist<'a>(playlists: &'a [Playlist], name: &str) -> Option<&'a Playlist> {
    let want = name.trim().to_lowercase();
    playlists
        .iter()
        .find(|p| p.title.trim().to_lowercase() == want)
}

/// A playlist's own data (no tracks): one small request that tells whether
/// its tracks changed ([`PlaylistInfo::last_updated`]).
pub fn playlist(session: &TidalSession, uuid: &str) -> Request {
    authed(
        format!(
            "{API}/playlists/{}?countryCode={}",
            percent_encode(uuid),
            percent_encode(&session.country_code)
        ),
        session,
    )
}

/// What [`playlist`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistInfo {
    pub title: String,
    pub tracks: u32,
    /// Changes whenever the playlist does (tracks added, removed, moved).
    pub last_updated: String,
}

/// Reads the answer to [`playlist`]; `Ok(None)` for 404 (the playlist is
/// gone: look it up by name again).
pub fn parse_playlist(status: u16, body: &[u8]) -> Result<Option<PlaylistInfo>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer {
        title: String,
        #[serde(default)]
        number_of_tracks: u32,
        #[serde(default)]
        last_updated: String,
    }
    if status == 404 {
        return Ok(None);
    }
    ensure!(status == 200, "{}", status_error("playlist", status));
    let a: Answer = json("playlist", body)?;
    Ok(Some(PlaylistInfo {
        title: a.title,
        tracks: a.number_of_tracks,
        last_updated: a.last_updated,
    }))
}

/// One page of a playlist's items, in the playlist's order.
pub fn playlist_items(session: &TidalSession, uuid: &str, offset: u32) -> Request {
    authed(
        format!(
            "{API}/playlists/{}/items?countryCode={}&limit={TRACKS_PAGE}&offset={offset}&order=INDEX&orderDirection=ASC",
            percent_encode(uuid),
            percent_encode(&session.country_code)
        ),
        session,
    )
}

/// A song in the playlist, as the clock lists and stores it
/// (`tidal/playlist.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    /// With its version, if any ("Song (Live)").
    pub title: String,
    /// The main artist.
    pub artist: String,
    pub duration_s: u32,
    /// The album cover's image id, for [`cover_url`].
    pub cover: Option<String>,
}

/// A page of tracks (videos and unplayable tracks left out), and the offset
/// of the next page.
pub fn parse_playlist_items(
    status: u16,
    body: &[u8],
    offset: u32,
) -> Result<(Vec<Track>, Option<u32>)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Page {
        /// Absent: this is the last page.
        total_number_of_items: Option<u32>,
        items: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "type")]
        kind: String,
        item: serde_json::Value,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Item {
        id: u64,
        title: String,
        #[serde(default)]
        version: Option<String>,
        duration: u32,
        #[serde(default)]
        artist: Option<Name>,
        #[serde(default)]
        artists: Vec<Name>,
        #[serde(default)]
        album: Option<Album>,
        #[serde(default = "yes")]
        allow_streaming: bool,
        #[serde(default = "yes")]
        stream_ready: bool,
    }
    #[derive(Deserialize)]
    struct Name {
        name: String,
    }
    #[derive(Deserialize)]
    struct Album {
        #[serde(default)]
        cover: Option<String>,
    }
    fn yes() -> bool {
        true
    }
    ensure!(status == 200, "{}", status_error("playlist", status));
    let page: Page = json("playlist", body)?;
    let n = page.items.len() as u32;
    let next = offset + n;
    let mut tracks = Vec::new();
    for e in page.items {
        if e.kind != "track" {
            continue;
        }
        let Ok(i) = serde_json::from_value::<Item>(e.item) else {
            continue;
        };
        if !i.allow_streaming || !i.stream_ready || i.duration == 0 {
            continue;
        }
        let artist = i
            .artist
            .or_else(|| i.artists.into_iter().next())
            .map(|a| a.name)
            .unwrap_or_default();
        let title = match i.version.filter(|v| !v.trim().is_empty()) {
            Some(v) => format!("{} ({})", i.title, v.trim()),
            None => i.title,
        };
        tracks.push(Track {
            id: i.id,
            title,
            artist,
            duration_s: i.duration,
            cover: i.album.and_then(|a| a.cover).filter(|c| !c.is_empty()),
        });
    }
    let more = n > 0 && page.total_number_of_items.is_some_and(|t| next < t);
    Ok((tracks, more.then_some(next)))
}

/// A cover image (JPEG) of `px` × `px` (80, 160, 320, 640, 1280).
pub fn cover_url(cover: &str, px: u32) -> String {
    format!(
        "https://resources.tidal.com/images/{}/{px}x{px}.jpg",
        cover.replace('-', "/")
    )
}

/// The stream manifest of a track in lossless stereo (the clock decodes
/// FLAC only; `HIGH` would be AAC). `immersiveaudio=false`: for a track
/// with a Dolby Atmos mix, the device-login client otherwise gets that
/// mix (E-AC-3, 5.1, reported as quality `LOW`) whatever quality it asks for.
pub fn playback_info(session: &TidalSession, track: u64) -> Request {
    authed(
        format!(
            "{API}/tracks/{track}/playbackinfopostpaywall?countryCode={}&audioquality=LOSSLESS&playbackmode=STREAM&assetpresentation=FULL&immersiveaudio=false",
            percent_encode(&session.country_code)
        ),
        session,
    )
}

/// What to download for `track` (`seconds`: its catalog duration, for DASH
/// manifests without one).
pub fn parse_playback_info(
    status: u16,
    body: &[u8],
    track: u64,
    seconds: f64,
) -> Result<MediaPlan> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Answer {
        track_id: u64,
        asset_presentation: String,
        audio_quality: String,
        #[serde(default)]
        audio_mode: Option<String>,
        manifest_mime_type: String,
        manifest: String,
    }
    ensure!(status == 200, "{}", status_error("stream", status));
    let a: Answer = json("stream", body)?;
    ensure!(a.track_id == track, "stream: another track came back");
    ensure!(a.asset_presentation == "FULL", "stream: only a preview");
    ensure!(
        matches!(
            a.audio_quality.as_str(),
            "LOSSLESS" | "HI_RES_LOSSLESS" | "HI_RES"
        ),
        "stream: not lossless ({})",
        a.audio_quality
    );
    if let Some(mode) = a.audio_mode.as_deref() {
        ensure!(mode == "STEREO", "stream: not stereo ({mode})");
    }
    manifest::plan(&a.manifest_mime_type, &a.manifest, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_info() {
        let r = playlist(&session(), "ab-cd");
        assert_eq!(r.url, format!("{API}/playlists/ab-cd?countryCode=CH"));
        let body = br#"{"uuid":"ab-cd","title":"Wecker","numberOfTracks":4,
            "lastUpdated":"2026-11-13T10:00:00.000+0000","duration":900}"#;
        let info = parse_playlist(200, body).unwrap().unwrap();
        assert_eq!(info.title, "Wecker");
        assert_eq!(info.tracks, 4);
        assert_eq!(info.last_updated, "2026-11-13T10:00:00.000+0000");
        assert_eq!(parse_playlist(404, b"{}").unwrap(), None);
        assert!(parse_playlist(500, b"{}").is_err());
    }

    fn session() -> TidalSession {
        TidalSession {
            access_token: "tok".into(),
            refresh_token: "r".into(),
            expires_at: 0,
            user_id: 42,
            country_code: "CH".into(),
        }
    }

    #[test]
    fn playlists_find_wecker() {
        let r = playlists(&session(), 0);
        assert_eq!(
            r.url,
            "https://api.tidal.com/v1/users/42/playlists?countryCode=CH&limit=50&offset=0"
        );
        assert!(r.headers.contains(&("Authorization", "Bearer tok".into())));
        let body = br#"{"limit":50,"offset":0,"totalNumberOfItems":2,"items":[
            {"uuid":"u1","title":"Chill","numberOfTracks":3},
            {"uuid":"u2","title":" wecker ","numberOfTracks":12}]}"#;
        let (list, next) = parse_playlists(200, body, 0).unwrap();
        assert_eq!(next, None);
        assert_eq!(find_playlist(&list, "Wecker").unwrap().uuid, "u2");
    }

    #[test]
    fn playlist_tracks_and_paging() {
        let body = br#"{"limit":2,"offset":0,"totalNumberOfItems":5,"items":[
            {"type":"track","cut":null,"item":{"id":1,"title":"Song","version":"Live",
             "duration":200,"artist":{"name":"A"},"artists":[{"name":"A"}],
             "album":{"id":9,"title":"X","cover":"ab-cd"},"allowStreaming":true,"streamReady":true}},
            {"type":"video","cut":null,"item":{"id":2}},
            {"type":"track","cut":null,"item":{"id":3,"title":"Gone","duration":100,
             "artists":[{"name":"B"}],"allowStreaming":false,"streamReady":true}}]}"#;
        let (tracks, next) = parse_playlist_items(200, body, 0).unwrap();
        assert_eq!(next, Some(3));
        assert_eq!(
            tracks,
            vec![Track {
                id: 1,
                title: "Song (Live)".into(),
                artist: "A".into(),
                duration_s: 200,
                cover: Some("ab-cd".into()),
            }]
        );
        assert!(
            playlist_items(&session(), "u2", 100)
                .url
                .contains("limit=100&offset=100")
        );
    }

    #[test]
    fn session_check() {
        assert_eq!(
            sessions(&session()).url,
            "https://api.tidal.com/v1/sessions"
        );
        let body = br#"{"sessionId":"s","userId":42,"countryCode":"CH","channelId":1}"#;
        assert_eq!(parse_sessions(200, body).unwrap(), (42, "CH".into()));
        assert!(parse_sessions(401, b"{}").is_err());
    }

    #[test]
    fn playback_info_asks_for_stereo() {
        let r = playback_info(&session(), 7);
        assert!(
            r.url
                .contains("/tracks/7/playbackinfopostpaywall?countryCode=CH")
        );
        assert!(r.url.contains("audioquality=LOSSLESS"));
        assert!(r.url.contains("immersiveaudio=false"));
    }

    #[test]
    fn playback_info_refuses_atmos() {
        let atmos = br#"{"trackId":7,"assetPresentation":"FULL","audioMode":"DOLBY_ATMOS",
            "audioQuality":"LOSSLESS","manifestMimeType":"application/vnd.tidal.bts","manifest":""}"#;
        let e = parse_playback_info(200, atmos, 7, 100.0).unwrap_err();
        assert!(e.to_string().contains("not stereo"), "{e}");
    }

    #[test]
    fn cover_urls() {
        assert_eq!(
            cover_url("aaaa-bb-cc", 160),
            "https://resources.tidal.com/images/aaaa/bb/cc/160x160.jpg"
        );
    }
}
