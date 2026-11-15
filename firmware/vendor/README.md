# Vendored crates

Patched copies of upstream crates, used through `[patch.crates-io]` in `../Cargo.toml`. Every change is marked "wakey" in the source. Each section says when the copy can go.

## i-slint-renderer-software 1.18.1

Slint's software renderer with four additions, all marked "wakey patch".

`draw_texture_line` in `draw_functions.rs` has a fast path for glyphs, meaning alpha maps drawn 1:1 with no rotation and an integer offset. It skips transparent pixels, writes opaque ones directly and blends only the edges. The generic path computes several divisions per glyph and line, plus a division and premultiply per pixel. On the Tab5 the patch cut text rendering in the list bench from about 14 ms to 9 ms per frame.

`Scene::is_opaque_on_line` in `scene.rs` treats a rounded rectangle with an opaque fill and border as covering its lines outside the top and bottom corners, so the line renderer skips what lies behind it there (the page background behind cards and list rows, a list row's red underlay). Upstream counts only plain rectangles and RGB images as opaque. Output is pixel for pixel the same (checked with `tools/ui-preview`). Measured on the Tab5: scrolling the alarm list 55 → 42 ms per frame, a touched row 24 → 19 ms.

`PrepareScene::process_filled_path` and `process_stroked_path` in `lib.rs` draw paths in line-by-line mode, where upstream draws nothing (every `Path`, so every icon, was invisible on the device). The path is rasterized with zeno (`path::mask`, shared with the full-buffer renderer) into an alpha map once per frame and added to the scene through `process_target_texture`, like a glyph. Icons are small (32–40 px), so this costs little.

`draw_linear_gradient` in `draw_functions.rs` dithers vertical gradients (4×4 Bayer, through `TargetPixel::from_rgb_dithered`, which only `Rgb565Pixel` implements). Upstream fills each line with one colour, which on a 16-bit target shows the clock's sky as visible bands. The pattern repeats every 4 pixels, so it is computed once per line and copied: a full clock repaint takes ~66 ms on the Tab5 (~50 ms with a plain background).

Licence unchanged (GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0, see `LICENSES/`). Upstream state (2026-11-14): Slint 1.19 brings its own dithering (slint-ui/slint#13696) and a glyph fast path (#13873), so those two hunks go with 1.19 (re-measure the glyph path first); rounded rectangles as cover is slint-ui/slint#14956, paths in line-by-line mode is #14955 (a PR follows). Drop the vendored copy when a Slint release covers all four.

## i-slint-core 1.18.1

Three changes. `DirtyRegion::MAX_COUNT` in `partial_renderer.rs` is raised from 3 to 16 (marked "wakey patch"). Slint tracks at most this many dirty rectangles and merges the rest by bounding boxes. With a scrolling list (one rectangle per row), the clock tick and an animation, 3 slots merged the left and right halves into full-screen repaints: two 50 ms frames (both buffers) every second, a visible stutter. The renderer's `PhysicalRegion` follows the constant (its `== 3` assertion, only needed for the C++ API, is removed in the vendored renderer). `DirtyRegion::add_box` merges a rectangle that shares lines with two or more others into one band (marked "wakey patch"): the renderer hands out one line range per rectangle, the firmware's PPA path keeps three strips open, and a row of side-by-side changes (keyboard keys) cost ~350 ms instead of ~40 ms; stacked rows stay separate. Upstream state (2026-11-14): the count is slint-ui/slint#14954; the band merge will not go upstream, but Slint 1.19's `SoftwareRenderer::set_dirty_region_alignment` (#12656) with the panel width as horizontal alignment may replace it (compare the keyboard row and the clock tick). And `lib.rs` allows `unused_imports`/`dead_code`: as a path dependency the crate does not get Cargo's lint capping, and its `libm` build warns.

## esp-idf-svc 0.53.0

Changes marked "wakey" in `src/http/client.rs`: a request first deletes the headers the previous request on the same connection set (ESP-IDF keeps them on the client, so a reused connection, `firmware/src/http.rs`, sent a stale `Range` or token again); `Configuration::save_client_session` and `EspHttpConnection::close` (keep a TLS session ticket across connections). Both are pull requests upstream (esp-rs/esp-idf#632 and #633; esp-idf-svc now lives in the esp-rs/esp-idf monorepo); drop the vendored copy once a release has them. Upstream's `close()` returns `Result`: `http::close_all` then needs `let _ =` or a log.

Also marked "wakey" in `src/timer.rs`, the embassy time driver: with an empty timer queue it armed the ESP-IDF timer for `u64::MAX - now`, which passed its "safe" bound once the uptime exceeded one day; `esp_timer_start_once` adds the timeout to its own now as an `int64_t`, so the timer fired at once and re-armed itself forever. The `esp_timer` task held CPU 0 and the clock froze 24 h after boot (task watchdog: `IDLE0`, `CPU 0: esp_timer`). The driver now arms nothing for an empty queue and at most one hour ahead. Upstream reported this as esp-rs/esp-idf#616 and fixed it the same way in #618 (c0c177b, 2026-10-20, at most one day ahead), not in a release yet; this hunk goes with the vendored copy.

## esp_lcd (ESP-IDF 5.5.5), in `esp-idf-components/`

ESP-IDF's `esp_lcd` component without its `test_apps`, added through `[[package.metadata.esp-idf-sys.extra_components]] component_dirs`, where it takes the place of ESP-IDF's own. One change, marked "wakey" in `dsi/esp_lcd_panel_dpi.c`: the DPI panel allocates and frees its frame buffers through `esp_lcd_dpi_panel_alloc_fb` and `esp_lcd_dpi_panel_free_fb`, weak functions whose defaults are the driver's own code (`heap_caps_calloc` in PSRAM with DMA, `free`). The firmware replaces them (`src/display/mod.rs`): the first panel's two buffers are kept and handed to every later one. Display sleep rebuilds the display link for 33 Hz (21 mA less), and each rebuild freed and allocated the buffers anew; with ~2 MB of PSRAM free in the app, the freed 3.7 MB came back too fragmented for new ones (the largest block 8 KB short) and the clock crashed on waking.

Upstream state (2026-11-14): ESP-IDF's DPI driver has no way to pass frame buffers in. Drop the copy when it gets one (then use it), or when the link no longer needs rebuilding for a frame rate change. After an ESP-IDF update, copy its `esp_lcd` again and re-apply the change. A change here is picked up only after `cargo clean -p esp-idf-sys --release` (esp-idf-sys keeps its ESP-IDF build).
