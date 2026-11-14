# Upgrades

What can go, or must be re-checked, when a dependency is updated: workarounds and vendored patches that exist because of an upstream bug or a missing feature, with the upstream ticket or fix. Check this list before and after bumping a version; remove an entry once its change is done. The details of each workaround are in [pitfalls.md](pitfalls.md) and [vendor/README.md](../firmware/vendor/README.md).

## Rust toolchain

- **The nightly pin (nightly-2026-10-17).** Later nightlies cannot build `std` for `riscv32imafc-esp-espidf` (libc 0.2.191 renamed newlib's `stat` fields; rust-lang/rust#164555, fix pending in rust-lang/libc#5662). Once a nightly has the fix: move the pin, `cargo build --release --bins`, run on the device.
- **`#[allow(clippy::missing_const_for_thread_local)]`** in `firmware/src/http.rs` and `firmware/src/ui/platform.rs`: a known false positive on targets without `#[thread_local]` (rust-lang/rust-clippy#17566, fix PR #17905). Remove both once a nightly's clippy has the fix.

## ESP-IDF (5.5.5)

- **`Display::stop_stream_dma`** (`firmware/src/display/mod.rs`): `esp_lcd_panel_del` leaves the DPI panel's DW-GDMA channel running. Fixed in v6.1.1 and on `release/v6.0`; backport to 5.5 requested (espressif/esp-idf#19437). Drop with either.
- **PPA strip padding** (`MIN_LINES` in `firmware/src/display/ppa.rs`): the stall on rotated blocks narrower than 8 pixels was an ESP-IDF bug (espressif/esp-idf#19023), fixed on `release/v5.5` after v5.5.6. The fix covers blocks of ≤ 5 lines: with that release, test 1280×6 and 1280×7 strips under DSI load before dropping the padding. The 640 px width split (`MAX_BLOCK_W`) is probably the same bug; test it too.
- **ChaCha20-Poly1305 first** (`firmware/sdkconfig.defaults`): AES-GCM with mbedTLS buffers in PSRAM took ~60 % of a core because any PSRAM buffer disqualified the hardware GCM path. Fixed on master (f53e7995a5, 2026-10-12), in no release yet. With a release that has it: re-measure a TIDAL download with AES-GCM forced; keep ChaCha20 first if it is still cheaper.
- **USB host: root port powered once, never down** (`firmware/src/audio/uac/host.rs`, `device.rs`): powering the root port down during enumeration aborts in `hub.c` (espressif/esp-idf#18366). Fixed only in the esp-usb `usb_host` managed component (1.4.1), not in the in-tree `components/usb` of 5.5. Keep the workaround unless Wakey moves to the managed component.
- **ISP overflow interrupts masked, sensor at 5 fps** (`firmware/src/ambient.rs`): ESP-IDF's ISP driver logs FIFO overflow from its ISR. Not reported upstream: a re-test at 30 fps (2026-11-14, without an SD card) showed no overflow at all; the original storm happened with SD writes on top of a download. Keep as is.

## esp_hosted (2.13.1)

- **`net::hosted_task_buffers` / `free_task_buffers`** (`firmware/src/net.rs`): the static task buffers leaked per init/deinit. Fixed on `release/v2.x` (03d438a4, after 2.13.1). Drop with the next 2.x host component; check with the `wifi-leak-test` feature (0 B per session).
- **`CONFIG_ESP_HOSTED_SDIO_RESET_ACTIVE_HIGH=y`** (`firmware/sdkconfig.defaults`): the Tab5 board preset defaults to the wrong reset polarity (espressif/esp-hosted-mcu#258, against `release/v2.x`). Once a release has the fix the line is redundant; keeping it does no harm.

## esp-idf-svc (0.53.0, vendored)

- **The vendored copy** (`firmware/vendor/esp-idf-svc`): deleting the previous request's headers (esp-rs/esp-idf#632), `Configuration::save_client_session` and `EspHttpConnection::close` (esp-rs/esp-idf#633). esp-idf-svc now lives in the esp-rs/esp-idf monorepo. Drop the vendored copy and the `[patch.crates-io]` entry once a release has both; upstream's `close()` returns `Result`, so `http::close_all` then needs `let _ =` or a log.

## Slint (1.18.1, vendored renderer and core)

- **With 1.19:** the gradient dithering hunk (upstream slint-ui/slint#13696) and the glyph fast path (upstream #13873; re-measure the list bench first) of the vendored renderer can go. Try `SoftwareRenderer::set_dirty_region_alignment` (#12656) with the panel width as horizontal alignment instead of the vendored `DirtyRegion::add_box` band merge: compare the keyboard row (`DEBUG_SLOW_FRAMES`) and the clock tick (stats line).
- **Later releases:** the rounded-rectangle cover (`Scene::is_opaque_on_line`, slint-ui/slint#14956), paths in line-by-line mode (slint-ui/slint#14955) and the dirty-rectangle count of 16 (slint-ui/slint#14954) are upstream as a PR or an issue. Drop each hunk when a release has an equivalent; drop the vendored crates when all are covered.
- **Hidden literal `Text` items for font sizes** (`ui/wakey/clock_page.slint`, the 280 px digits): a `font-size` set through a component property embeds no glyphs (slint-ui/slint#14957). If upstream follows constant bindings, the hidden `Text` can go.
- **One window component per program**: a second exported `Window` embedded every font again (slint-ui/slint#14958). Once fixed, the alarm list's offscreen row could be its own component again instead of a page of `AppWindow`; no need to change it.

## espflash (4.7.0)

- **Passive monitoring** (`docs/pitfalls.md`, "Tools"): `espflash monitor -c esp32p4 --non-interactive --no-reset --before no-reset-no-sync`, with a 9 s pause and a false "Secure Download Mode" warning (esp-rs/espflash#1130). Update the pitfalls entry when espflash changes this.
