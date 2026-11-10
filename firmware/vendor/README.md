# Vendored crates

Patched copies of upstream crates, used through `[patch.crates-io]` in `../Cargo.toml`. Every change is marked "wakey" in the source. Each section says when the copy can go.

## i-slint-renderer-software 1.18.1

Slint's software renderer, unchanged except for one addition in `draw_functions.rs` (`draw_texture_line`, marked "wakey patch"): a fast path for glyphs, meaning alpha maps drawn 1:1 with no rotation and an integer offset. It skips transparent pixels, writes opaque ones directly and blends only the edges. The generic path computes several divisions per glyph and line, plus a division and premultiply per pixel. On the Tab5 the patch cut text rendering in the list bench from about 14 ms to 9 ms per frame.

Licence unchanged (GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0, see `LICENSES/`). Worth offering upstream; drop the vendored copy when a Slint release has an equivalent.

## i-slint-core 1.18.1

Three changes. `DirtyRegion::MAX_COUNT` in `partial_renderer.rs` is raised from 3 to 16 (marked "wakey patch"). Slint tracks at most this many dirty rectangles and merges the rest by bounding boxes. With a scrolling list (one rectangle per row), the clock tick and an animation, 3 slots merged the left and right halves into full-screen repaints: two 50 ms frames (both buffers) every second, a visible stutter. The renderer's `PhysicalRegion` follows the constant (its `== 3` assertion, only needed for the C++ API, is removed in the vendored renderer). `DirtyRegion::add_box` merges a rectangle that shares lines with two or more others into one band (marked "wakey patch"): the renderer hands out one line range per rectangle, the firmware's PPA path keeps three strips open, and a row of side-by-side changes (keyboard keys) cost ~350 ms instead of ~40 ms; stacked rows stay separate. And `lib.rs` allows `unused_imports`/`dead_code`: as a path dependency the crate does not get Cargo's lint capping, and its `libm` build warns.

## esp-idf-svc 0.53.0

Unchanged except one `#[cfg(any())]` in `src/wifi.rs` (marked "wakey"): the `embedded_svc::wifi::asynch::Wifi` impl for `AsyncWifi` is compiled out. With Wi-Fi enabled (esp_wifi_remote on the P4) rustc nightly-2026-10-15 panics type-checking it (`compare_impl_item`, "DefId::expect_local … `scan_n::N` isn't local", an ICE on a const generic in an async trait method from another crate). The firmware uses the blocking `EspWifi`/`BlockingWifi` API. Drop the vendored copy once the toolchain is bumped past the bug (check by building with the registry crate).
