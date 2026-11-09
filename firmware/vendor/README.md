# Vendored crates

## i-slint-renderer-software 1.18.1

Slint's software renderer, unchanged except for one addition in
`draw_functions.rs` (`draw_texture_line`, marked "wakey patch"): a fast path
for glyphs, meaning alpha maps drawn 1:1 with no rotation and an integer
offset. It skips transparent pixels, writes opaque ones directly and blends
only the edges. The generic path computes several divisions per glyph and
line, plus a division and premultiply per pixel. On the Tab5 the patch cut
text rendering in the scroll test from about 14 ms to 9 ms per frame.

Used through `[patch.crates-io]` in `../Cargo.toml`. Licence unchanged
(GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR
LicenseRef-Slint-Software-3.0, see `LICENSES/`). Worth offering upstream;
drop the vendored copy when a Slint release has an equivalent.

## i-slint-core 1.18.1

Unchanged except `DirtyRegion::MAX_COUNT` in `partial_renderer.rs`, raised
from 3 to 16 (marked "wakey patch"). Slint tracks at most this many dirty
rectangles and merges the rest by bounding boxes. With a scrolling list
(one rectangle per row), the clock tick and an animation, 3 slots merged
the left and right halves into full-screen repaints: two 50 ms frames
(both buffers) every second, a visible stutter. The renderer's
`PhysicalRegion` follows the constant (its `== 3` assertion, only needed
for the C++ API, is removed in the vendored renderer).
