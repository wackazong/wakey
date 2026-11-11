# Vendored crates

Patched copies of upstream crates, used through `[patch.crates-io]` in `../Cargo.toml`. Every change is marked "wakey" in the source. Each section says when the copy can go.

## i-slint-renderer-software 1.18.1

Slint's software renderer with four additions, all marked "wakey patch".

`draw_texture_line` in `draw_functions.rs` has a fast path for glyphs, meaning alpha maps drawn 1:1 with no rotation and an integer offset. It skips transparent pixels, writes opaque ones directly and blends only the edges. The generic path computes several divisions per glyph and line, plus a division and premultiply per pixel. On the Tab5 the patch cut text rendering in the list bench from about 14 ms to 9 ms per frame.

`Scene::is_opaque_on_line` in `scene.rs` treats a rounded rectangle with an opaque fill and border as covering its lines outside the top and bottom corners, so the line renderer skips what lies behind it there (the page background behind cards and list rows, a list row's red underlay). Upstream counts only plain rectangles and RGB images as opaque. Output is pixel for pixel the same (checked with `tools/ui-preview`). Measured on the Tab5: scrolling the alarm list 55 → 42 ms per frame, a touched row 24 → 19 ms.

`PrepareScene::process_filled_path` and `process_stroked_path` in `lib.rs` draw paths in line-by-line mode, where upstream draws nothing (every `Path`, so every icon, was invisible on the device). The path is rasterized with zeno (`path::mask`, shared with the full-buffer renderer) into an alpha map once per frame and added to the scene through `process_target_texture`, like a glyph. Icons are small (32–40 px), so this costs little.

`draw_linear_gradient` in `draw_functions.rs` dithers vertical gradients (4×4 Bayer, through `TargetPixel::from_rgb_dithered`, which only `Rgb565Pixel` implements). Upstream fills each line with one colour, which on a 16-bit target shows the clock's sky as visible bands. The pattern repeats every 4 pixels, so it is computed once per line and copied: a full clock repaint takes ~66 ms on the Tab5 (~50 ms with a plain background).

Licence unchanged (GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0, see `LICENSES/`). Worth offering upstream; drop the vendored copy when a Slint release has an equivalent.

## i-slint-core 1.18.1

Three changes. `DirtyRegion::MAX_COUNT` in `partial_renderer.rs` is raised from 3 to 16 (marked "wakey patch"). Slint tracks at most this many dirty rectangles and merges the rest by bounding boxes. With a scrolling list (one rectangle per row), the clock tick and an animation, 3 slots merged the left and right halves into full-screen repaints: two 50 ms frames (both buffers) every second, a visible stutter. The renderer's `PhysicalRegion` follows the constant (its `== 3` assertion, only needed for the C++ API, is removed in the vendored renderer). `DirtyRegion::add_box` merges a rectangle that shares lines with two or more others into one band (marked "wakey patch"): the renderer hands out one line range per rectangle, the firmware's PPA path keeps three strips open, and a row of side-by-side changes (keyboard keys) cost ~350 ms instead of ~40 ms; stacked rows stay separate. And `lib.rs` allows `unused_imports`/`dead_code`: as a path dependency the crate does not get Cargo's lint capping, and its `libm` build warns.

## esp-idf-svc 0.53.0

Unchanged except one `#[cfg(any())]` in `src/wifi.rs` (marked "wakey"): the `embedded_svc::wifi::asynch::Wifi` impl for `AsyncWifi` is compiled out. With Wi-Fi enabled (esp_wifi_remote on the P4) rustc nightly-2026-10-15 panics type-checking it (`compare_impl_item`, "DefId::expect_local … `scan_n::N` isn't local", an ICE on a const generic in an async trait method from another crate). The firmware uses the blocking `EspWifi`/`BlockingWifi` API. Drop the vendored copy once the toolchain is bumped past the bug (check by building with the registry crate).
