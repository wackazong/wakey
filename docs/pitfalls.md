# Pitfalls

Things that already went wrong on this board, with the fix that is in place. Read before changing the area; keep entries short.

## Platform and build

- **esp-hal does not run here.** It supports only ESP32-P4 rev ≥ v3.0; this Tab5 is rev v1.3. Hence ESP-IDF (which builds for rev < v3 with `CONFIG_ESP32P4_SELECTS_REV_LESS_V3`). Flash with ESP-IDF's bootloader (`scripts/flash.sh`), not espflash's bundled one.
- **`sdkconfig.defaults` changes rebuild much of ESP-IDF** (~10 min). Changes to `[package.metadata.esp-idf-sys]` in `Cargo.toml` may need `cargo clean --release -p esp-idf-sys`.
- **rustc ICE with Wi-Fi enabled:** nightly-2026-10-15 panics compiling esp-idf-svc's async `Wifi` impl for `AsyncWifi`. Vendored esp-idf-svc with that impl compiled out; use the blocking API.
- **Internal RAM before `main`:** esp_hosted initialises in a C constructor. With its defaults a 64 KiB main task no longer fitted: boot loop with `assert failed: esp_startup_start_app … (res == pdTRUE)`. The executor runs on its own task (`runtime.rs`), the main task is 8 KiB, esp_hosted's pools and stacks are in PSRAM.
- **No 64-bit atomics:** the target (`riscv32imafc`) has no `AtomicU64`; use `AtomicU32` (or a `Mutex`). `wakey-core` builds on the host, where it exists, so the error shows only in the firmware build.
- **Per-sample float math is slow:** the P4's FPU is single precision only; `f64` is soft-float, and `sinf` of a large argument (above ~200 rad) takes newlib's slow range reduction (`__kernel_rem_pio2f`). The built-in tone computed `sin` and `%` per sample and used a whole core; its thread (priority 5) starved the UI task (priority 1) whenever it ran on core 0: the screen froze while the tone played. The tone is now a precomputed table, and the decoder thread is pinned to core 1 (`ThreadSpawnConfiguration`). A task watchdog report naming `pthread` on CPU 0 with ROM math routines in the register dump points here.
- **`NVS` partition can be taken once:** `EspDefaultNvsPartition::take()` happens in `app` and is cloned into `net::start` and the alarm; a second `take()` fails.
- **Small allocations used up internal RAM:** with `CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL=16384` (every `malloc` up to 16 KiB tried internal RAM first), opening the alarm list took free internal RAM from ~87 KiB to 14 KiB and DMA-capable memory to 0 (Slint's items, model rows, strings). The SD driver then found no buffer for one sector (`sdmmc_cmd: allocate_dma_buf: not enough mem`, `writing config.tmp: I/O error (os error 5)`): switching an alarm on or off was not saved. At the same time every frame got about four times slower (5 CPU cycles per instruction instead of 1.4: a list frame took 170 ms instead of 42). Now 0: `malloc` takes PSRAM first, which is as fast as internal RAM while cached; drivers that need internal or DMA memory ask for it. The per-second stats line shows free DMA memory and its largest block.
- **Flashing is interrupted by a short timeout** → "invalid segment length" and no bootable app. The 7 MB image takes ~30 s to write; allow ≥ 90 s when wrapping `cargo run` in a timeout.

## Display

- **DSI PHY clock:** `phy_clk_src` must be 0 (driver default) on rev < v3; the `…_DEFAULT` constant is the rev ≥ v3 choice and aborts in the HAL.
- **Flicker in dark areas** (≈ 3 Hz, stronger when bright) with Espressif's BSP timings (70 MHz, 57.8 Hz). Fixed with M5GFX's: 80 MHz pixel clock, 66.1 Hz, 1040 Mbps lanes, backlight PWM 44.1 kHz. esp-bsp's ST7123 table also declares `0xA3`/`0xE8` one byte too long (39 and 14 parameters are right).
- **Upside down:** landscape `(x, y)` → panel column `y`, row `1279 − x`; touch is rotated 180° against display memory (`ui/input.rs`).
- **PPA stalls for good** (no completion, no error) under DSI load when: a rotated block is narrower than 8 output pixels (strips < 8 lines are padded), a block is wider than 640 px (split), or several transactions are queued (one in flight). The PPA moves only ~20 Mpx/s; bulk copies go through the 2D-DMA (64 Mpx/s).
- **CPU writes into a PPA target are lost:** the driver invalidates the output rows before each transaction. The CPU never writes the frame buffer while the PPA is used.

## Slint

- **Stutter every second:** with 3 dirty rectangles (Slint's default) a ticking clock plus a scrolling list merged into full-screen repaints. Vendored `i-slint-core` allows 16; every independently changing text is its own fixed-width item.
- **Flicker in the list area at start:** Slint's first full repaints land over the list; the render loop composes the list again after Slint when their areas overlap (`FrameHook::overlaps`).
- **Flash size explodes with big fonts:** every imported face × every pixel size × ASCII is pre-rendered. Two faces at 300 px overflowed the app partition. One face per program, one compiled `.slint` file per program (pages are imported). Each new size costs: adding 140 px (alarm page) grew `wakey` by 1.4 MB; reuse the sizes listed in `ui/wakey.slint`.
- **Many dirty rectangles side by side are slow:** when several dirty items share the same lines (weekday chips, keyboard keys), Slint hands out one line range per rectangle, more than the PPA path keeps open strips (3); every strip is flushed after one line and padded from PSRAM. Seven chips took ~160 ms, a row of keys ~350 ms, a full-screen repaint ~40 ms. The vendored `DirtyRegion::add_box` now merges a rectangle that shares lines with two or more others into one band. Still keep an edit's repaint to the items that change: no binding shared by a row of items (the chips own their state), no `for` over a model or array that changes (it re-creates its children; use `for i in N` with the changing value per item, as the keyboard does).
- **Rendering cost grows with items per line:** the line-by-line renderer visits every item on each line. The keyboard page (~30 items per line) took ~160 ms for a full repaint, other pages ~45 ms, measured while internal RAM was used up (see Platform), so today's numbers are lower. The vendored renderer skips what lies behind an opaque rounded rectangle on its straight lines (cards, list rows). A full-width list scrolls at ~24 fps (41 ms per frame), limited by the PPA's rotation rate (~21 Mpx/s); the list layer (DMA) is the way to 60 fps.
- **A binding on shared touch state repaints every row:** `highlighted: i == view.touched` in each row of the alarm list made every row depend on `touched`, so a press, a swipe step or a toggle repainted the whole list (~170 ms then). The rows now carry their touch state in the model (`AlarmRow.dx`, `dragging`, `highlighted`); the page writes only the row that changes (~15 ms). The Rust side likewise updates only rows that differ; `ModelRc` fields compare by identity, so `days` is compared by content.
- **`clip` ignores `border-radius`** in the software renderer: content is clipped to the rectangle and can show in the rounded corners. Round the content itself (the stop button's fill bar).
- **Wrong `default-font-family` embeds a second font:** with `"Inter 24pt"` (the file's family is "Inter 24pt Medium") the compiler found no match, embedded the system's sans-serif (Helvetica on the Mac that builds) at every size, and the UI drew with it. Symptoms: capital umlauts without dots at 28–32 px (the fallback face had no precomposed glyph there), and 3.4 MB of flash. Use the exact family name; `tools/ui-preview` lists the embedded families in its generated code.
- **A `TouchArea` inside a moving element sees moving coordinates:** in a swipe-to-delete row the touch area moved with the row, so `mouse-x` jumped with every offset change. Put the touch area on the fixed parent and the sliding content above it.
- **Scrolling list with swipeable rows:** with a `Flickable` and a touch area per row, a swipe that drifted a little vertically was taken by the list (it intercepts after 8 px in a direction it can scroll) and deleting failed once the list scrolled. The alarm list has one touch area for everything and decides the direction itself.
- **`Flickable` holds a press back briefly** to tell taps from drags: a press, moves and a release within one frame (scripted touches) arrive as a tap. Real swipes are slower; scripted gestures need ~16 ms between events.
- **Missing glyphs** for characters that only come from Rust strings (e.g. "März"): put them into a literal in the `.slint` file.

## Touch and I2C

- **SDA held low after a reset** (a slave mid-byte): `SysI2c::new` clocks the bus free on every boot.
- **Touch lagged, then stopped, while touched quickly:** reads of the ST7123 timed out (`ESP_ERR_TIMEOUT`, then NACKs) and the app looked hung. The legacy I2C driver sets the hardware's clock-stretch timeout to 2^10 clock cycles (~26 µs); the touch controller stretches SCL longer while it processes a touch. `SysI2c::new` sets ~13 ms. As a safety net, the touch task releases a held press when reads fail and pulses `TP_RST` after 5 failed reads within a second.
- **Drags broke off** because the touch controller skips readings or times out mid-drag: a release needs 3 empty polls.

## USB audio

- **"Root port reset failed" forever:** the HS controller must be full/low-speed only (`HCFG.FSLSSupp`, register 0x400 bit 2) after `usb_host_install` and before powering the port. espressif/usb's `fsls_only` needs a newer ESP-IDF HAL than 5.5.5.
- **Order:** host installed with the root port unpowered, `USB5V_EN` on, 1 s for the speaker to boot, then port power. The first two resets still fail ("Root port reset failed" in the log is normal); the host stack recovers by itself, and the firmware power-cycles the port if nothing enumerates within 3 s.
- **Packets acknowledged, no sound:** ESP-IDF's interface claim does not send `SET_INTERFACE`; send it explicitly.
- **Control IN transfers:** the buffer must hold the setup packet plus the data stage rounded up to 64 bytes.

## SD card

- **`send_op_cond` timeout:** the SD pins need on-chip LDO 4 at 3.3 V — and a card that is fully inserted (that was the actual cause once).
- Long file names need `CONFIG_FATFS_LFN_HEAP`; macOS `._*` files are skipped.

## Wi-Fi (ESP32-C6)

- **C6 never answers** (`sdmmc send_op_cond 0x107`): esp_hosted's Tab5 preset assumes an active-low reset and holds the C6 in reset. GPIO15 drives EN directly: `CONFIG_ESP_HOSTED_SDIO_RESET_ACTIVE_HIGH=y`.
- **Host version:** esp_hosted host 2.x works with the factory C6 firmware; 3.x is a different protocol. The factory firmware lacks transport-size reporting (warnings at start) and has a known RX stall under memory pressure (m5stack/M5Tab5-UserDemo#20); updating the C6 to slave ≥ 2.13.1 would fix both.
- The C6 carries a factory test SSID ("M5Stack-Production"); harmless, the firmware sets its own configuration.

## RTC

- A fresh RX8130 can hold **non-BCD junk with the power-loss flag clear**. `get_time` validates BCD and date ranges; `clock::init` also rejects times before the firmware build.

## Removed experiments

Recover from git if needed:

- **Buoyant** (`buoyant_test`): full redraw on every change, 48 ms per frame at 1280×720 vs Slint's partial rendering; Slint was chosen. Removed in 41b908d.
- **`ppa_probe`** (PPA/2D-DMA throughput, PPA stall sweep), **`panel_test`** (static picture, backlight steps, INA226 readings), **`display_test`** (raw CPU fill: 18 ms per full frame): removed in 41b908d.
- **esp-hal smoke spike** (`spikes/tab5-p4-rust-smoke`): builds, cannot run on rev v1.3. Removed in 41b908d.
