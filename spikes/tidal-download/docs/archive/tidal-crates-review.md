> Historical note: superseded by the TIDAL downloader rewrite. See [the current README](../../README.md) for setup and supported behavior.

# TIDAL crates review — 2026-09-26

Queried crates.io with `cargo search tidal --limit 30` and inspected the published packages with `cargo info` and their downloaded source. This is a source review, not a live playback test.

| Crate | Published version inspected | Relevance to this project |
| --- | --- | --- |
| [tidalrs](https://crates.io/crates/tidalrs/0.5.0) | 0.5.0 | Legacy v1 client with device authentication, token refresh, track metadata, playback information and a stream reader. Matches the architecture's intended starting point. |
| [tidlers](https://crates.io/crates/tidlers/0.5.0) | 0.5.0 | Alternative with device-code and PKCE login, session persistence, playback URLs and DASH manifest parsing. Its README identifies `yadal`, a downloader, as a consumer. Strong candidate if DASH handling is needed. |
| [tidalv2](https://crates.io/crates/tidalv2/0.3.0) | 0.3.0 | Generated v2 API models/endpoints, including track manifests, with authentication helpers and optional tidalrs interoperability. Useful for current API and authentication investigation; not a complete verified file-download transaction. |
| [prawn](https://crates.io/crates/prawn/0.1.0) | 0.1.0 | Generated v2 wrapper with authorization-code/PKCE and client-credentials support. Another public API integration option. |
| [moosicbox_tidal](https://crates.io/crates/moosicbox_tidal/0.4.0) | 0.4.0 | TIDAL integration in the larger MoosicBox ecosystem. Default features include API, database, OpenAPI and scanning; more integration surface than this host probe needs. |
| [rstidal](https://crates.io/crates/rstidal/0.1.2) | 0.1.2 | Its documented login uses username/password and a desktop application token. Poor fit for this project's own-client OAuth approach. |
| [tidal](https://crates.io/crates/tidal/0.1.0), [tidalcache](https://crates.io/crates/tidalcache/0.1.0) | 0.1.0 each | The cache implementation stores JSON catalog records for artists, albums and tracks. It is not a cache of downloaded song audio. |

## Recommendation

Use `tidalrs = "0.5.0"` for the architecture-aligned host experiment's authentication, metadata and playback-information requests. Retain our small file-download and validation layer: complete the transfer, decode/check duration, publish the local file, and verify offline. Investigate `tidlers` before implementing our own DASH parser if actual account responses require it. Use `tidalv2` if the client's supported authentication/API route requires v2.

The initial hand-written API adapter duplicated existing crate functionality. Avoiding an audio player dependency was not a sufficient reason: `rodio` is only a **dev-dependency** of tidalrs. Its normal dependencies do include Tokio, reqwest and stream-download, which are acceptable for the Mac experiment; embedded portability can be addressed after feasibility is proven.

Do not equate a crate method named `stream` with mandatory wakeup-time streaming. A reader can be consumed completely into a file at song-selection time. Specifically, tidalrs 0.5.0's `TrackStream::stream()` uses `StreamDownload` with `MemoryStorageProvider`; for our bounded-memory disk download, use playback information and our file writer instead. The method itself is not a DASH-to-native-FLAC file converter.

The crates do not establish that this account/client receives complete downloadable media. Live authentication, format inspection and offline verification remain necessary. Preserve the distinction between unsupported format, missing entitlement and successful download.

This review changes the recommended dependency approach. The existing executable has not yet been migrated; it still uses the direct reqwest API adapter. No dependency or behavior changes were made as part of this review.
