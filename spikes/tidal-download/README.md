# Tab5 alarm audio downloader

Download a complete track from **your TIDAL account** into a verified local FLAC file. Optionally also produce a 48 kHz, 16-bit stereo WAV for the planned Tab5/Creative Pebble audio output. Run everything from the Rust dev container with `cargo run`.

The downloader is a host tool. The [hardware architecture](../../architecture/Tab5%20Wecker%20%E2%80%93%20Entwicklungsgrundlage.md) describes the future clock; firmware, alarm scheduling, and device playback are not implemented yet.

## What changed, and why this has a better chance of working

The old probe depended on a newly registered developer application being allowed to use device login, and accepted only a single native FLAC URL. This replacement uses the Rust [`tidlers` crate, pinned to 0.5.0](https://docs.rs/crate/tidlers/0.5.0), for browser PKCE login and session serialization. It uses the crate’s bundled client profiles instead of requiring developer-app credentials. Its own download layer handles direct BTS media and segmented DASH, retries transfers, and uses FFmpeg to decode audio containers into FLAC.

The default quality setting tries lossless first and can fall back to AAC. The downloaded file must decode completely and match the catalog duration before it is published. This removes several concrete failure points in the old implementation.

**This is an unofficial TIDAL integration, not a guaranteed export API.** The bundled client profile can stop working if TIDAL changes its authentication or playback service. A working account with access to the selected track is required. Automated tests validate the HTTP/download/conversion pipeline with generated audio; they do not establish that your account receives full media. Complete the live account test below before relying on it.

## 1. Check the dev container

Rust and Cargo are installed normally and available on `PATH`. The [Dockerfile](../../.devcontainer/Dockerfile) installs Rust 1.97.0 and FFmpeg. If you opened a container built before this change, choose **Dev Containers: Rebuild Container** in VS Code. Alternatively, update that existing container’s FFmpeg installation with:

```sh
sudo apt-get update
sudo apt-get install -y --no-install-recommends ffmpeg
```

From the repository root in the container’s Bash terminal:

```sh
cd spikes/tidal-download
cargo run
cargo run -- doctor
```

Bare `cargo run` displays help. `doctor` checks FFmpeg and reports whether a session file exists; it does not contact TIDAL. All remaining commands in this README run from `spikes/tidal-download`. This folder is its own Cargo workspace with its own `Cargo.lock`; it is excluded from the repository’s future firmware workspace. Its `rust-toolchain.toml` pins host Rust 1.97.0 independently of any future root Xtensa toolchain. Run Cargo from this folder so rustup selects that toolchain; `--manifest-path` alone does not change rustup’s toolchain selection. Keep future firmware-specific Cargo target settings scoped to the firmware directory, because ancestor `.cargo/config.toml` files can still affect nested workspaces.

Build artifacts go to `/tmp/cargo-target`, as configured by the Dockerfile and `devcontainer.json`. The executable is `/tmp/cargo-target/debug/tidal-download`. These artifacts are rebuilt after recreating the container. Sessions and downloaded audio are stored in the spike’s ignored `.local/` directory (`spikes/tidal-download/.local/` from the repository root) and survive container rebuilds as long as the workspace remains.

## 2. Sign in to TIDAL

```sh
cargo run -- login
```

1. Keep the command running in the container terminal. Copy the login URL it prints and open it in a browser on your host computer.
2. Sign in to your TIDAL account on the TIDAL website and complete any approval or account checks shown there. Your password is entered on TIDAL’s website, not in this tool.
3. After login, the browser redirects to `https://tidal.com/android/login/auth?...`. Copy the **entire address**, including the query string containing `code` and `state`. A blank page or an error at the destination does not necessarily mean login failed; the redirect address is what the CLI needs. If the browser offers to open an installed app, keep the redirect in the browser so you can copy it.
4. Paste that address at the CLI’s prompt and press Enter. Input is hidden. Do not run it as a shell command or paste it into chat, an issue, or a log.
5. Wait for `Login saved`. Then check the session:

```sh
cargo run -- status
```

No port forwarding, browser inside the container, or local callback server is required. The CLI checks that the callback matches this login attempt and exchanges its one-time code using PKCE. If the URL has no `code`, or the state does not match, start `login` again and use the URL from that new attempt.

### Do I need to create a developer app?

**No, not for this implementation.** `tidlers` supplies its own bundled unofficial client configuration; the default PKCE profile uses an Android callback. This is why the setup differs from the previous README. It also means continued access depends on that upstream profile remaining accepted by TIDAL. Creating your own public developer app does not make it equivalent to this client profile.

The previous `TIDAL_CLIENT_ID`, `TIDAL_CLIENT_SECRET`, and `TIDAL_ACCESS_TOKEN` environment variables are no longer read. There is no `.env` setup step.

### Alternative: device-code login

If PKCE cannot be completed in your browser, try the crate’s alternate device profile:

```sh
cargo run -- login --device
```

Open the displayed verification URL in your host browser, authorize the device, and leave the terminal running while it polls. This replaces the saved session only after successful login. It uses the library’s bundled device profile, not a newly registered public application. Device-profile availability and audio quality may differ; neither login method is guaranteed to remain available.

### Session storage and refresh

The default session file is `.local/tidal/session.json`. It contains access and refresh tokens in plaintext, is written atomically with mode `0600` on Linux, and is excluded from version control by `.gitignore`. Treat it as a credential. Commands sharing a session are serialized by a file lock so simultaneous refreshes cannot overwrite each other.

Downloads and `status` reuse the saved session and refresh tokens close to expiry, preserving a replacement refresh token if TIDAL rotates it. If TIDAL revokes the session or refresh fails, run `login` again. The CLI does not print tokens or signed audio URLs. To keep sessions separate, use the global `--session` option consistently:

```sh
cargo run -- --session .local/tidal/second-account.json login
cargo run -- --session .local/tidal/second-account.json download 66035607
```

To remove the locally saved credentials:

```sh
cargo run -- logout
```

This deletes the local session file; it does not revoke access on TIDAL’s servers. Previously downloaded audio remains available.

## 3. Download a track

In TIDAL, open a track and copy its track link. Replace the example below with a track you can play fully in your account:

```sh
cargo run -- download 'https://tidal.com/browse/track/66035607'
```

A numeric ID works too:

```sh
cargo run -- download 66035607
```

The downloader accepts `tidal.com/track/...`, `tidal.com/browse/track/...`, and `listen.tidal.com/track/...` links. For a shortened share link, open it in the browser first and copy the resolved track URL. Albums, playlists, and batches are not implemented.

The account’s country is obtained from TIDAL; there is no manual `--country` flag. The CLI requests a full track, downloads the complete file or all DASH segments, converts to native FLAC, and validates it. A successful run creates:

```text
.local/sounds/66035607/
├── audio.flac
└── report.json
```

The report records the track, artist, expected and decoded durations, requested and returned quality, source byte count, audio format, and SHA-256. It contains no tokens, media manifests, or signed URLs.

### Quality selection

| Option | Behavior |
| --- | --- |
| `--quality auto` (default) | Request `LOSSLESS`; if that attempt fails, try `HIGH` AAC. Record the quality actually returned. |
| `--quality lossless` | Require TIDAL to return `LOSSLESS`; fail instead of falling back. |
| `--quality hires` | Request `HI_RES_LOSSLESS`; require that returned quality. Try this after a normal download works. |
| `--quality high` | Require `HIGH` AAC. Useful when lossless delivery is unavailable. |

```sh
cargo run -- download 66035607 --quality lossless
cargo run -- download 66035607 --quality hires --output .local/hires
```

The final filename is always `audio.flac`. **If the source is AAC, converting it to FLAC does not make the source lossless or restore lost detail.** The CLI announces this, and the report sets `source_lossy: true`. Use `--quality lossless` when lossless source audio is a requirement.

Existing track directories are never replaced. To compare quality settings, choose a different `--output` directory. The CLI refuses to overwrite an existing directory even if it is empty.

### Prepare audio for the planned Tab5 output

Add `--wav` to also produce `alarm.wav`:

```sh
cargo run -- download 66035607 --wav
```

The WAV is 48,000 samples/second, two channels, signed 16-bit PCM. It is decoded and checked separately, and its checksum is included in the report. This matches the output format specified in the hardware architecture and avoids requiring FLAC decoding or resampling on the clock. Copy that file to the eventual SD-card sound directory. This repository does not yet flash firmware or play it through the Tab5 hardware.

## 4. Verify and listen offline

For a complete download directory:

```sh
cargo run -- verify .local/sounds/66035607
```

This decodes `audio.flac`, checks its duration against the report, and compares its SHA-256 with the recorded value. If the report includes `alarm.wav`, that file is checked too. Verification makes no network requests and does not load account credentials.

To test with networking completely disconnected, build first, then run the executable directly inside the container:

```sh
cargo build --locked
# Now disconnect networking.
/tmp/cargo-target/debug/tidal-download verify .local/sounds/66035607
```

Open the saved audio in a player on your host computer and listen through to the end while offline. Container audio passthrough is not required. The JSON report's `offline_listening_test` field is not updated by manual listening. On 2026-09-26, the user confirmed that the saved lossless FLAC passed this offline listening check. This is evidence for that local file only; it does not prove Tab5 decoding or playback. The current report describes a 44.1 kHz FLAC and has no WAV entry, so the optional 48 kHz WAV conversion has not yet been run on this track.

A standalone FLAC, including one saved by the old probe, can also be checked if you supply its expected full duration:

```sh
cargo run -- verify /path/to/song.flac --expected-seconds 240
```

## Failure handling and limits

The implementation handles unencrypted BTS files, BTS mirror URLs, and static single-period DASH audio with segment templates, timelines, or segment lists. DASH templates support number/time substitutions and numbered formatting. The downloader selects the highest-bandwidth audio representation in the returned manifest. It rejects previews, encrypted manifests, unsupported DASH byte ranges, and dynamic/live manifests. Automatic quality fallback may find an unencrypted supported rendition; there is no decryption or preview fallback.

Transfers retry transient HTTP errors and interrupted bodies. Each media-object attempt starts from its original file offset, so retries do not duplicate bytes. Downloaded data is capped at 512 MiB per source; generated FLAC and WAV files are each capped at 512 MiB. Each quality attempt has a 30-minute limit, each conversion a five-minute limit, and DASH is capped at 20,000 URLs. Downloads are sequential and cannot resume across process restarts.

The decoded duration must match the catalog within two seconds, with a tighter tolerance for short clips. This allows catalog rounding and codec padding; it is a practical completeness check, not proof that the result matches a reference master sample-for-sample. FLAC frame decoding, declared sample counts, and recorded hashes provide additional checks. A checksum detects changes relative to this download, not authenticity against TIDAL’s master.

Work takes place in `.download-*` staging directories beneath the selected output directory. The final track directory appears only after verification, through an atomic rename that refuses replacement. Ordinary failures remove staging data. A forced kill can leave a staging directory; remove only the abandoned `.download-*` directory after confirming no download is running. Locks are released by the operating system when a process exits; a remaining `.lock` file does not mean a lock is still held.

| Symptom | What to do |
| --- | --- |
| `FFmpeg missing` | Rebuild the dev container or install FFmpeg using the command above; rerun `doctor`. |
| No saved session | Run `cargo run -- login`. |
| Callback has no code / state mismatch | Keep the original login process open and copy its full final redirect URL. Start a fresh login if needed. |
| Login or refresh rejected | Retry login; optionally try `login --device`. If both fail, the upstream client profile may need an update. |
| HTTP 401 | Run login again and retry. |
| HTTP 403 | Check that the track plays fully in your TIDAL account. Try `--quality auto` or `--quality high`. This can also indicate client-profile restrictions. |
| HTTP 404 | Check the numeric track ID and availability in your account’s country. |
| Encrypted/unsupported media or FFmpeg failure | Try automatic quality fallback. The tool saves nothing as successful unless decoding and duration verification pass. |
| Existing output | Verify the existing directory, or select a different `--output`. |
| Duration/hash mismatch | Do not use that file for the alarm; download again to a different output directory and investigate the source. |

A successful `status` confirms session access, not download entitlement. The meaningful live test is **login → download → offline verification → listening** with your own account and chosen track.

## Development and verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests require FFmpeg, included in the dev container. They generate real audio locally and cover FLAC validation, DASH/AAC transfer and decoding, interrupted-body retries, transient HTTP errors, WAV conversion, manifest checks, PKCE callback validation, private session files, and preservation of existing downloads. No TIDAL credentials are needed; tests do not contact TIDAL.

The Rust code lives in [`src`](src), with tests in [`tests`](tests). Authentication uses `tidlers`; bounded HTTP transfer uses `reqwest`; DASH parsing uses `roxmltree`; FFmpeg handles container decoding; `claxon` and `hound` independently validate the published audio formats. Dependencies are locked in `Cargo.lock`. There is no Python helper or external downloader service.

The earlier feasibility plan and crate comparison are retained under [`docs/archive`](docs/archive) as historical notes. They describe the superseded prototype and are not current setup instructions.
