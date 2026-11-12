# TIDAL downloader (host tool)

Downloads one complete track from **your own TIDAL account** into a verified local FLAC, optionally plus a 48 kHz / 16 bit stereo WAV for the alarm clock's SD card. It is the basis for the planned TIDAL integration (see [`docs/roadmap.md`](../../docs/roadmap.md)); the firmware does not use it.

**Unofficial integration.** Login uses the [`tidlers`](https://docs.rs/crate/tidlers/0.5.0) crate (pinned 0.5.0) with its bundled client profiles. It can stop working whenever TIDAL changes authentication or playback.

## Requirements

- Rust: `rust-toolchain.toml` pins 1.97.0 (rustup installs it). Run Cargo from this directory: it is its own workspace with its own `Cargo.lock`.
- FFmpeg on `PATH` (decoding containers to FLAC, tests).

```sh
cargo run -- doctor     # checks FFmpeg and the saved session, no network
```

## Sign in

```sh
cargo run -- login
```

Open the printed URL in a browser and sign in on TIDAL's website. After login the browser lands on `https://tidal.com/android/login/auth?...`; copy that **entire** address (it contains `code` and `state`) into the hidden prompt. A blank or error page there is normal. If PKCE login does not work, `cargo run -- login --device` uses the device-code profile instead.

`cargo run -- status` checks the session. It is stored in plain text in `.local/tidal/session.json` (mode 0600, ignored by git): treat it as a credential. `cargo run -- logout` deletes it locally (no server-side revocation). `--session <file>` selects another session file.

## Download

```sh
cargo run -- download 'https://tidal.com/browse/track/66035607'   # or the numeric ID
cargo run -- download 66035607 --wav                              # also alarm.wav
```

Result in `.local/sounds/<id>/`: `audio.flac`, `report.json` (track, durations, qualities, SHA-256; no tokens or URLs) and, with `--wav`, `alarm.wav`. Copy `alarm.wav` to the SD card's `sounds/` directory for the clock (the firmware plays WAV and AIFF; it does not decode FLAC yet).

| `--quality` | Behaviour |
| --- | --- |
| `auto` (default) | `LOSSLESS`, falling back to `HIGH` (AAC) |
| `lossless` | require `LOSSLESS` |
| `hires` | require `HI_RES_LOSSLESS` |
| `high` | require `HIGH` (AAC) |

AAC converted to FLAC stays lossy (`source_lossy: true` in the report). Existing output directories are never overwritten; use `--output` to choose another one.

## Credentials for the clock

```sh
cargo run -- clock-config
```

Prints the `tidal` section for the clock's `config.json`: the device-code app credentials bundled with `tidlers` (no network). Paste it into `config.json` on the SD card; the clock logs in with a device code and writes its session there. Treat the output like a password and never commit it.

## Verify offline

```sh
cargo run -- verify .local/sounds/66035607
cargo run -- verify /path/to/song.flac --expected-seconds 240
```

Decodes the FLAC (and the WAV, if recorded), checks the duration against the report and the SHA-256. No network, no credentials.

## Limits

Single tracks only (no albums, playlists, batches). Supports unencrypted BTS files and static single-period DASH; rejects previews, encrypted and live manifests. Transfers retry transient HTTP errors; downloads are capped at 512 MiB per source and 30 minutes per quality attempt, and cannot resume. The decoded duration must match the catalog within 2 s. Work happens in `.download-*` staging directories, published by an atomic rename only after verification; a killed process can leave one behind.

| Symptom | What to do |
| --- | --- |
| FFmpeg missing | Install FFmpeg, rerun `doctor` |
| Callback has no code / state mismatch | Copy the full final redirect URL of the same login attempt |
| HTTP 401, login or refresh rejected | `login` again; try `login --device` |
| HTTP 403 | Check the track plays fully in your account; try `--quality high` |
| HTTP 404 | Check the track ID and availability in your country |

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked      # needs FFmpeg, does not contact TIDAL
```

Code in [`src`](src), tests in [`tests`](tests): `tidlers` (auth), `reqwest` (transfer), `roxmltree` (DASH), FFmpeg (decoding), `claxon` and `hound` (independent validation of the results).
