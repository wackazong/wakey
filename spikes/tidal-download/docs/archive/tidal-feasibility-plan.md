> Historical note: superseded by the TIDAL downloader rewrite. See [the current README](../../README.md) for setup and supported behavior.

# Step 1: prove full-song download before the embedded port

## Objective and pass condition

Move architecture step 10 ahead of firmware work. On the development Mac, select one TIDAL track and save the complete song now. After networking is disabled and the process restarted, the local file must decode and play to the end. A preview, a signed URL, successful catalog lookup, or streaming playback is not a pass.

The future selection transaction is: select track → download to temporary storage → validate → publish local file → update the alarm's selected path. Until the transaction completes, retain the previous waking song. At alarm time, read the local file with an embedded fallback tone; never authenticate or download at wakeup.

## Findings and sources (2026-09-26)

Follow-up: the [crates.io review](tidal-crates-review.md) recommends replacing the initial hand-written API adapter with `tidalrs` and evaluating `tidlers` for DASH. The implementation status below describes the current direct adapter, before that migration.

- The architecture references [`phayes/tidalrs`](https://github.com/phayes/tidalrs), an unofficial client targeting legacy v1. Its [track implementation](https://github.com/phayes/tidalrs/blob/master/src/track.rs) calls `playbackinfopostpaywall` with full asset presentation. Its [authentication implementation](https://github.com/phayes/tidalrs/blob/master/src/lib.rs) uses device authorization. The small host adapter here follows that request shape without pulling in its playback stack.
- TIDAL's [developer terms](https://developer.tidal.com/documentation/guidelines/guidelines-developer-terms) and [developer guidelines](https://developer.tidal.com/documentation/guidelines/guidelines-developer-guidelines) limit available playback uses, including downloads from embeds. A subscription or successful stream does not by itself establish offline export permission.
- TIDAL's [device support page](https://support.tidal.com/hc/en-us/articles/115005871525-Authorize-a-Device) describes offline use in its mobile app. That is not evidence of an exportable FLAC API for an embedded device.

Conclusion: **technical feasibility with this user's client/account is not established**. The architecture's “download FLAC to SD” is a hypothesis. Test actual access, and establish an appropriate supported integration before treating a technical success as a production design decision.

## Implementation sequence

1. **Host workspace** — stable Rust, native target, isolated `tools/tidal-probe` package. Keep hardware setup out of the host build. Implemented.
2. **Authenticate using your application** — accept an existing user token or run device authorization with bounded polling, pending/slow-down handling, and expiry. Keep tokens in memory. Implemented; actual client support must be tested.
3. **Select a track** — use a numeric track ID and country, obtain catalog duration, request `FULL` + `LOSSLESS`. Implemented. Start with a familiar song longer than a preview, then repeat with a second track from another album.
4. **Classify the response** — require the requested track, full asset presentation, and lossless quality. Accept only clear BTS/FLAC; distinguish encrypted and unsupported manifests. Implemented. Authentication/HTTP failures are inconclusive about media availability.
5. **Download at selection time** — read the full body into a temporary file with a size cap and timeouts; do not forward the API bearer token to the CDN. Decode all FLAC frames, verify declared sample count and catalog duration, hash the file, then publish without overwriting. Implemented. Report persistence is a separate operation; a report failure can leave a verified FLAC and is explicitly reported.
6. **Offline proof** — stop the process, disconnect the network, run `verify`, compare its hash with the saved report, and play the file to its end. Implemented verification command; manual account and listening checks remain pending.
7. **Decision** — record the matrix below. If clear FLAC succeeds, proceed to a provider/storage interface and the SD selection transaction. If only unsupported DASH/HLS is returned, inspect its structure and add a tested unencrypted segment/container adapter before deciding. If the client lacks entitlement, resolve client access first. If only encrypted content is available, the present native-FLAC design has no path implemented; use local user-provided audio as the V1 path.

`playbackmode=STREAM` is a legacy request parameter used by `tidalrs`; it does not cause wakeup streaming in this tool, and it does not represent an OFFLINE entitlement. All bytes are consumed during `download`. Whether durable storage is supported for a production integration is a separate decision.

## Evidence to record locally

| Check | Current state | Passing evidence |
| --- | --- | --- |
| Rust build and offline automated tests | Passed locally | Build/test output |
| Your client supports login | Not run; credentials needed | Successful authorization |
| Your client/account returns full media | Not run | FULL response for selected track |
| Download is native unencrypted FLAC | Not run | Verified file and JSON report |
| Full duration and integrity | Not run | Decoded duration, frame checks, SHA-256 |
| Restart + network disconnected | Not run | Same hash and successful local verification |
| Audible playback through end | Not run | Manual listening observation |
| Other album and expired session | Not run | Repeated success or classified failure |
| Supported durable storage integration | Unresolved | TIDAL-supported access/terms for the intended use |

Do not share tokens, manifests, or signed CDN URLs in logs. Failure diagnostics intentionally omit raw response bodies. For unsupported responses, record only their MIME type and the error category to decide the next implementation step.

Validation on the development Mac: stable Rust 1.98.1; `cargo test --workspace --locked --offline` passed all four tests; `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` passed; code formatted with `cargo fmt --all`; CLI help commands ran successfully. Tests generate real FLAC audio, decode it in a fresh CLI process without credentials, and reject truncation, CRC corruption, duration mismatch, encryption, and insecure media URLs. These are local tests, not a live TIDAL or physical network-disconnection test. `Cargo.lock` records the resolved dependency versions.

## Later Tab5 work

Only after the decision: introduce provider-independent `Track`, `DownloadResult`, storage and HTTP interfaces; map HTTP onto `esp_idf_svc::http::client`; atomically replace the selected alarm configuration after durable SD publication; preserve a local fallback. Convert to 48 kHz / 16-bit stereo WAV at selection time or use the architecture's FLAC decoder/resampler during local playback. Store no developer client secret in distributable firmware. Firmware power management, USB audio, and Slint remain separate work.
