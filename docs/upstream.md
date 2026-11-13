# Upstream bug reports and pull requests (candidates)

Found while working on Wakey (M5Stack Tab5, ESP32-P4 + ESP32-C6, ESP-IDF 5.5.5, esp_hosted 2.13.1, esp-idf-svc 0.53, Slint 1.18.1). Every candidate was checked against the upstream tracker and upstream HEAD on 2026-11-13; the ones that were already filed, fixed or not worth a maintainer's time are in the table at the end. The rest is below, with what to cite.

| # | Candidate | Repository | Kind | Ticket |
| --- | --- | --- | --- | --- |
| 1 | `EspHttpConnection` sends stale request headers on a reused connection | esp-rs/esp-idf (`esp-idf-svc/`) | Bug + PR | not filed |
| 2 | `save_client_session` and `close()` for `EspHttpConnection` | esp-rs/esp-idf (`esp-idf-svc/`) | Feature PR | not filed |
| 3 | Software renderer: `DirtyRegion::MAX_COUNT` of 3 collapses to full-screen repaints | slint-ui/slint | Discussion, then PR | not filed |
| 4 | Software renderer draws no `Path` in line-by-line mode | slint-ui/slint | Issue, then PR | not filed |
| 5 | Software renderer: rounded rectangles as opaque line coverage | slint-ui/slint | Performance PR | not filed |
| 6 | Compiler: no glyphs for a `font-size` set through a property; a second `Window` embeds every font again | slint-ui/slint | Bug + diagnostic request | not filed |
| 7 | `esp_lcd_panel_del` on a DPI panel leaves its DW-GDMA channel streaming (fixed on master, no backport) | espressif/esp-idf | Backport request | not filed |
| 8 | ISP driver logs FIFO overflow from its ISR; a burst trips the interrupt watchdog | espressif/esp-idf | Bug | not filed |
| 9 | Tab5 BSP: ST7123 init table declares `0xA3`/`0xE8` one parameter too long | espressif/esp-bsp | Bug + PR | not filed |
| 10 | Tab5 board preset defaults to an active-low C6 reset and holds the C6 in reset | espressif/esp-hosted-mcu (`release/v2.x`) | Bug + PR | not filed |
| 11 | `espflash monitor`: passive use undocumented, `--no-reset` needs `--non-interactive` | esp-rs/espflash | Usability | not filed |

## 1. esp-idf-svc: stale request headers on a reused `EspHttpConnection`

Repository: esp-rs/esp-idf (monorepo; `esp-idf-svc/src/http/client.rs`, tags `esp-idf-svc-vX.Y.Z`). The standalone esp-rs/esp-idf-svc repository is archived since 2026-09-19. Kind: bug, with a PR (the project takes fixes as PRs with the bug in the description; template asks for `cargo fmt`, clippy, a `CHANGELOG.md` entry and a "Testing" section).

`EspHttpConnection::initiate_request` sets each request header with `esp_http_client_set_header`, but ESP-IDF keeps request headers on the client handle for its lifetime (`esp_http_client.c`: `set_header` writes into `client->request->headers`, cleared only by `esp_http_client_delete_header`/`delete_all_headers`; `esp_http_client.h` documents that `set_*` persists between `perform` calls). A second request on the same connection therefore also sends every header of the previous request that it does not set itself, for example a `Range` header from an earlier download or an `Authorization` header.

Checked 2026-11-13: not reported (the only header-adjacent tickets are the archived #263 on connection state and the stranded PR #578 on reconnect); not fixed on master (no `esp_http_client_delete_header` call; one commit since 0.53.0 touched the file, the MTU accessors).

Proposed fix: remember the names of the headers a request set and delete them with `esp_http_client_delete_header` at the start of the next `initiate_request`. `esp_http_client_delete_all_headers` is not an option: ESP-IDF stores its own `User-Agent` and `Host` through the same list. About ten lines; Wakey carries it in its vendored copy (marked "wakey").

## 2. esp-idf-svc: TLS session tickets and closing without cleanup

Repository: esp-rs/esp-idf (monorepo, as above). Kind: feature PR, one PR for both.

Two small additions that Wakey carries in its vendored copy:

- `Configuration::save_client_session: bool`, passed to `esp_http_client_config_t::save_client_session`. The field exists only under `CONFIG_ESP_TLS_CLIENT_SESSION_TICKETS`, so the assignment needs `#[cfg(esp_idf_esp_tls_client_session_tickets)]` (the crate gates other fields the same way, e.g. `esp_idf_esp_tls_use_ds_peripheral`).
- `EspHttpConnection::close()`: calls `esp_http_client_close` (closes the transport, state back to init, keeps the client and its saved ticket) and resets the connection's state.

Benefit measured on the ESP32-P4: a new connection to a host seen before resumes the TLS session instead of a full handshake (about 1.5 s saved per connection). `close()` is also the recovery path ivmarkov suggested in the archived issue #263.

Checked 2026-11-13: neither exists on master. Recent `Configuration` additions were merged with a changelog line (#633 keep-alive, #638, #642, `hw_private_key` in Unreleased), so the shape is accepted. On master `Configuration` is no longer `Copy` and has `hw_private_key`, so the vendored diff needs a small rebase.

## 3. Slint software renderer: three dirty rectangles

Repository: slint-ui/slint. Kind: discussion or issue first (public API), then a PR.

`DirtyRegion::MAX_COUNT` (`internal/core/partial_renderer.rs`) is 3. With a scrolling list (one rectangle per row), a clock tick and an animation, three slots plus the previous frame's rectangles under `SwappedBuffers` merged the left and right halves into full-screen repaints: two 50 ms frames every second on the ESP32-P4 (1280×720 RGB565, line-by-line). Wakey's vendored `i-slint-core` uses 16 and the renderer's `PhysicalRegion` follows it.

Why a discussion: `PhysicalRegion` is `#[repr(C)]` with `rectangles: [Box2D; 3]` and part of the C++ ABI (`api/cpp/include/slint-platform.h`), with a `const` assertion that both constants are 3, and `DirtyRegion` is public API used by the Skia renderer too. The proposal: a larger or configurable count on the renderer side while the C++ `PhysicalRegion` stays at 3 (merge down when crossing the ABI), with the Tab5 numbers. Related: discussion #10380 (ogoffart on configuring the region), issue #7432 (unexpected full-screen dirty regions).

Checked 2026-11-13: unchanged on master. Two other vendored patches of this family are not worth upstreaming: the glyph fast path is superseded by #13873 (1.19), and the `add_box` band merge trades pixels for fewer ranges per line, which upstream deliberately does not do; `SoftwareRenderer::set_dirty_region_alignment` (#12656, in 1.19) with the panel width as horizontal alignment gives a similar effect without a patch.

## 4. Slint software renderer: paths in line-by-line mode

Repository: slint-ui/slint. Kind: issue first (design question upstream left open), then a PR.

`render_by_line` draws nothing for a `Path`: `PrepareScene::process_filled_path`/`process_stroked_path` in `internal/renderers/software/lib.rs` are empty ("Path rendering is not supported in line-by-line mode"), and the `render_by_line` docs do not say so. On a device that renders by line every icon is invisible, while a desktop preview with the full-buffer renderer shows them. Wakey's vendored renderer rasterizes the path with zeno (`path::mask`, shared with the full-buffer renderer) into an alpha map once per frame and adds it to the scene through `process_target_texture`, like a glyph; icons are small (32–40 px), so the cost is negligible. A size cap with a fallback would be needed for large paths.

Checked 2026-11-13: still unsupported on master. Upstream acknowledged the gap: PR #9894 (buffer-mode paths, merged 2025-11) said there was no obvious way to do it per line and asked for ideas; PR #6032 asked whether zeno could rasterize per line. #14644 notes buffer mode re-rasterizes the full mask on every call, so a per-path mask cache would help both modes. PR #14866 (approved, not merged) renames `PrepareScene` → `ScanlineRasterCommands`; base the PR on it once it lands.

## 5. Slint software renderer: rounded rectangles as opaque line coverage

Repository: slint-ui/slint. Kind: performance PR.

`Scene::covers_line` (`scene.rs`; `is_opaque_on_line` in 1.18.1) counts plain rectangles, opaque RGB/RGB565/Gray8 textures and, since #13864, gradients as covering a line; `RoundedRectangle` is `_ => false`, so the line renderer draws everything behind a card or list row with rounded corners. Wakey's vendored renderer treats a rounded rectangle with an opaque fill and border as covering its lines outside the top and bottom corner regions. Output is pixel for pixel the same (checked with `tools/ui-preview`). Measured on the ESP32-P4: scrolling a list of rounded rows 55 → 42 ms per frame, a touched row 24 → 19 ms.

Checked 2026-11-13: not on master. #13864 (same kind of extension, merged in 12 days with benchmark tables) is the model. On master `RoundedRectangle` holds `shape: RoundedShape { radius, left/right/top/bottom_clip }`, anti-aliasing changed in #14560 (re-verify that the straight-edge pixels are fully opaque), and open PRs #14714/#14717 add corner shapes, so coverage must be limited to round corners. Open PR #13027 (occlusion culling at the partial-renderer level) excludes rounded corners and does not overlap.

## 6. Slint compiler: font embedding with `EmbedForSoftwareRenderer`

Repository: slint-ui/slint. Kind: one bug with a one-hunk fix, one diagnostic request; two issues.

- A `font-size` that reaches a `Text` through a component property (`Roll { font-size: 280px; }` into its `Text`) embeds no glyphs for that size: `collect_font_sizes_used` (`internal/compiler/passes/embed_glyphs.rs`) accepts only number literals (or casts of them) directly on `font-size`, does not follow property bindings, and warns about nothing. The text then draws at the nearest embedded size (about a quarter as large). The docs mention `SLINT_FONT_SIZES` for sizes "only known at run time", but this one is a compile-time constant. Ask: follow constant bindings, or warn that the size is not a literal. Related: issue #4956 (open; a comment reports the same symptom with a 100 px `Text`).
- A second `export component … inherits Window` in the same compilation embeds every font again: the `for c in doc.exported_roots()` loop pushes a `(path, query)` entry per exported component with no dedup by path, `embed_font_by_path` runs per entry and the resources are pushed into a plain `TiVec`. Wakey's image grew from 5.65 to 8.64 MB before the second window was folded into the first. Fix: dedup by path in that loop. Related: #14033 fixed only the runtime registration list.

Checked 2026-11-13: both still in master. The third observation (a `default-font-family` matching no imported font silently embeds a system font) is fixed by #13503, in 1.19.

## 7. ESP-IDF: `esp_lcd_panel_del` leaves the DPI panel's DW-GDMA channel running

Repository: espressif/esp-idf. Kind: backport request (the fix exists on master).

With ESP-IDF 5.5.5, deleting a MIPI-DSI DPI panel frees its DW-GDMA channel without disabling it (`dw_gdma.c` `channel_destroy`: unregister, free the interrupt, `free`), then frees the frame buffers; the channel keeps streaming from freed memory. Alone on the DW-GDMA nobody notices (the controller is powered down with its last channel). With another client alive (here the camera's CSI receiver), the next `esp_lcd_new_panel_dpi` gets the still running channel and `dpi_panel_init` dies with a store access fault in `dw_gdma_ll_channel_set_link_list_master_port` (MTVAL 0x50081128).

Fix on master: 5b52071edb "fix(dwgdma): improve release flow" (2026-09-21, MR !53017): `channel_destroy` now disables the channel and its interrupts before freeing. Not in v5.5.6, `release/v5.5`, v6.0.3, `release/v6.0`, v6.1 or `release/v6.1`; no GitHub issue references it.

Workaround in Wakey: `Display::stop_stream_dma` (`firmware/src/display/mod.rs`) finds the channel by its source address in the DW-GDMA registers and disables it before `esp_lcd_panel_del`.

Ask: backport 5b52071edb to `release/v5.5` (and 6.0/6.1), with the crash as the reason.

## 8. ESP-IDF: ISP driver logs FIFO overflow from its ISR

Repository: espressif/esp-idf. Kind: bug.

`s_isp_isr_dispatcher` in `components/esp_driver_isp/src/isp_core.c` calls `ESP_EARLY_LOGE(TAG, "fifo overflow")` for `ISP_LL_EVENT_ASYNC_FIFO_OVF` (and logs the other error events the same way); the error interrupts are enabled unconditionally by `esp_isp_new_processor`, and `esp_isp_evt_cbs_t` has no error or overflow callback on any branch. On the ESP32-P4 with the camera at 30 fps (1280×720 RAW8, one MIPI lane), while TLS, the SD card and the display all used PSRAM, the FIFO overflowed thousands of times a second and the logging from the ISR tripped the interrupt watchdog.

Checked 2026-11-13: unchanged on master except ea417f202e (2026-07), which moved `BUF_FULL` to `ESP_EARLY_LOGD`; the overflow is still `LOGE` from the ISR. Not reported (#19136/#19137 ask for more ISP error reporting, not less).

Workaround in Wakey: the sensor runs at 5 fps, the overflow interrupts are disabled after `esp_isp_enable` (`ISP_INT_ENA` 0x500A106C, bits 1 and 2), and a frame whose raw status shows an overflow is dropped (`firmware/src/ambient.rs`).

Proposed fix: no logging in the ISR; count the overflows and report them through an event callback, or at most one rate-limited log.

## 9. esp-bsp: Tab5 ST7123 init table declares two commands one parameter too long

Repository: espressif/esp-bsp, `bsp/m5stack_tab5/priv_include/disp_init_data_1.h`. Kind: bug, with a two-number PR.

Lines 16 and 23: `{0xA3, (uint8_t[]){…39 bytes…}, 40, 0}` and `{0xE8, (uint8_t[]){…14 bytes…}, 15, 0}`. The driver (`esp_lcd_st7123.c`, `esp_lcd_panel_io_tx_param(io, cmd, data, data_bytes)`) reads one byte past each array and sends it as the last parameter. The BSP's own ST7121 table (`disp_init_data_st7121.h`) declares the same two commands with 39 and 14; M5GFX's `Panel_ST7123.hpp` lists them as `40, 0xA3, …` and `15, 0xE8, …` in its command-inclusive format, i.e. 39 and 14 parameters; the lengths were copied including the command byte. Unchanged since the table was added in #765 (2026-04); #807 touched only delays.

Wakey's copy of the table with the right lengths: `firmware/src/display/init_tables.rs`.

## 10. esp_hosted: the Tab5 board preset holds the C6 in reset

Repository: espressif/esp-hosted-mcu, branch `release/v2.x` (`main` is the 3.x rewrite, where the polarity option no longer exists and the EN line is driven high/low-pulse/high by design). Kind: bug, with a one-line Kconfig PR.

`host/Kconfig` under `ESP32P4_TAB5_C6_BOARD`: `default ESP_HOSTED_SDIO_RESET_ACTIVE_LOW`, with the comment "M5Stack Tab5's C6 reset circuit is active-low". In esp_hosted's terms "active" is the level the pin is parked at after `transport_gpio_reset` (active, 10 ms inactive, active); the header says "If Reset pin is Enable, it is Active High. If it is RST, active low". On the Tab5, GPIO15 drives the C6's EN through 1 kΩ (schematic p. 2; 10 kΩ pull-up): with the preset the pin parks low and the C6 never leaves reset (`sdmmc send_op_cond 0x107`). `CONFIG_ESP_HOSTED_SDIO_RESET_ACTIVE_HIGH=y` fixes it; ESPHome's `esp32_hosted` maps `active_high: true` to the same option and M5Stack's own Tab5 ESPHome YAML uses `active_high: true`. Present in every 2.x tag from v2.12.12 (f8961155, 2026-07) to v2.13.1.

Related: issue #127 (open, "M5Stack Tab5: P4 host fails to establish SDIO communication"), where ciniml found the same inversion on 2026-05-11 with no maintainer reply. The branch is maintained (a fix merged 2026-10-28).

Proposed fix: `default ESP_HOSTED_SDIO_RESET_ACTIVE_HIGH if ESP32P4_TAB5_C6_BOARD` and the corrected comment.

## 11. espflash: passive monitoring

Repository: esp-rs/espflash. Kind: usability issue (low priority).

`espflash monitor` goes through `connect()` (the `--before` reset sequence, sync and chip detection), so it resets the chip into the bootloader before showing anything; `--no-reset` only skips the reset into the app afterwards and is ignored without `--non-interactive`. Listening without touching the chip needs `--non-interactive --no-reset --before no-reset-no-sync`, which nothing documents (README: "Open the serial monitor without flashing"). After such passive sessions flashing sometimes failed with "Timeout while running ReadReg" until the board was reset.

Checked 2026-11-13 (espflash 4.7.0, `main` at 95d8380): the connect step is intentional (#346, closed via #561: chip detection sets the monitor baud rate for 26 MHz crystals), so a passive default will not be accepted. Not covered: documenting the passive combination, letting `--no-reset` work without `--non-interactive` (or a `--passive` shorthand), and the ReadReg timeout after passive sessions (#646, open, shows the same error with `-b no-reset-no-sync`). Open PR #1129 changes the reset handling around `--before no-reset`; wait for it before claiming the timeout still happens.

## Checked and not filed

Candidates ruled out on 2026-11-13. "Drop" says what Wakey can remove once it updates.

| Candidate | Why not | Drop |
| --- | --- | --- |
| esp_hosted leaks the static task buffers per init/deinit | Fixed on `release/v2.x` by 03d438a4 (2026-10-26), not yet in a tag (v2.13.1 is from 2026-10-22). 3.x never had it | `net::hosted_task_buffers`/`free_task_buffers` with the next 2.x host component |
| USB host abort in `hub.c` after root port power-down | Duplicate of espressif/esp-idf#18366 (same abort line, other trigger); fixed in the esp-usb `usb_host` component 1.4.1 (e9b2c88 treats `ESP_ERR_NOT_FOUND` as benign). The in-tree `components/usb` of `release/v5.5` is unchanged | Nothing: keep powering the root port once; the fix reaches 5.5 only through the managed component |
| Slint `animation-tick()` not keeping `has_active_animations()` true | Not reproducible: a minimal `MinimalSoftwareWindow` loop on 1.18.1 draws every frame in all buffer modes; `animation_tick()` sets the flag on every evaluation and upstream tests exactly this binding. Wakey's own loop is fine too: `ui_bench`'s ball (`x` bound to `animation-tick()`) renders at 66 fps, 60–96 rows per frame, and the sound page's scrolling texts run smoothly (both checked on the device 2026-11-13). The original observation is not reproducible; the comment in `sound_page.slint` that the loop draws frames "for timers, not for `animation-tick()` alone" is wrong, the `Timer` there is just one working way to drive the marquee | – |
| rustc ICE in `compare_impl_item` | Filed five times (rust-lang/rust#164180 and others), fixed by #164318 in nightly-2026-10-17 | Done 2026-11-13: pin moved to nightly-2026-10-17, the `#[cfg(any())]` in the vendored esp-idf-svc removed. Later nightlies are blocked by rust-lang/rust#164555 (libc 0.2.191 vs std for espidf) until rust-lang/libc#5662 lands |
| Clippy `missing_const_for_thread_local` false positive | Known: rust-lang/rust-clippy#17566 and #13422 (targets without `#[thread_local]`, which includes `riscv32imafc-esp-espidf`), fix PR #17905 pending. The `#[allow]` placement is general rustc behaviour (attributes on a macro invocation are not inherited) | The two `#[allow]`s when #17905 ships |
| PPA stalls on rotated blocks narrower than 8 output pixels | Duplicate of espressif/esp-idf#19023 (fixed by 469aa16c35; the check used the unrotated dimensions, as suspected), backported to `release/v5.5` after v5.5.6 (e106b16b9a). The fixed check bypasses only blocks ≤ 5 lines; whether 6 and 7 still stall is untested | `MIN_LINES` padding in `display/ppa.rs` with the next 5.5.x, after a re-test of 1280×6 and ×7 |
| `MIPI_DSI_PHY_CLK_SRC_DEFAULT` on rev < v3 | By design: the enum has a `_DEFAULT_LEGACY` (PLL_F20M, rev < 3) and a `_DEFAULT` (XTAL, rev ≥ 3); Espressif's answer in #18521 is to pass 0 and let the driver choose, which Wakey does | – |
| `esp_hosted_deinit` asserts without a Wi-Fi driver | No such assert exists in the 2.13.1 deinit path (every step is null-guarded or always `ESP_OK`), nothing was recorded, and the leak test runs init/deinit around a driver that never connects. If seen again, check for a deinit without a matching init | – |
| Slint dithered gradients on RGB565 | Fixed by slint-ui/slint#13696 (interleaved gradient noise, `blend_dithered`), in 1.19 | The gradient hunk of the vendored renderer with 1.19 |
| Slint glyph fast path | Superseded by #13873 (skip zero and full coverage), in 1.19; only the per-pixel fixed-point addressing remains | The glyph hunk with 1.19, after re-measuring |
| Slint `add_box` band merge | Upstream renders several x-ranges per line on purpose; `SoftwareRenderer::set_dirty_region_alignment` (#12656, in 1.19, not in any 1.18 tag) with the panel width as horizontal alignment gives one range per line, at the price of widening every rectangle, also a lone clock tick | With 1.19: try the alignment setting against the vendored `add_box` (keyboard row via `DEBUG_SLOW_FRAMES`, clock tick via the stats line) |
| Slint warning for an unmatched `default-font-family` | Fixed by #13503, in 1.19 | – |
| AES-GCM with mbedTLS buffers in PSRAM | Root cause (any PSRAM buffer disqualified the hardware GCM path, so GHASH ran in software with per-chunk bounce buffers) fixed on master by f53e7995a5 (2026-10-12), not in any release; too large to backport | Re-measure on a release that has it; until then ChaCha20 first |
