# Tab5 firmware skeleton

Rust (std) on ESP-IDF 5.5.5 with the Embassy executor, for the M5Stack Tab5
(ESP32-P4 + 5" 720×1280 MIPI-DSI panel). Test applications:

| Binary | What it shows |
| --- | --- |
| `panel_test` | One static picture in both frame buffers, no flips; the backlight steps 100 / 50 / 20 % every 10 s (1–3 white squares show the step); logs the INA226 battery readings. Separates panel or backlight flicker from rendering problems |
| `ppa_probe` | With the display streaming: PPA and 2D-DMA throughput (rotate and copy a 640×720 window), and a sweep of strip heights that reports the first PPA stall (the < 8-line limitation) |
| `display_test` | Panel bring-up, tear-free double buffering, cost of a full-screen CPU fill; a second Embassy task logs a heartbeat |
| `slint_test` | Slint: clock digits, a continuous animation, and a 300-row list you drag and fling by touch, composed by the list layer at 66 fps |
| `audio_test` | USB Audio Class 1.0 speaker (Creative Pebble V3) on USB-A: plays every WAV/AIFF in the microSD root and `sounds/` in a loop (writes a 44.1 kHz test tone first; sample-rate converted with rubato), or a quiet 440 Hz sine without a card. Survives unplugging and re-plugging the speaker; logs packet and underrun counters |
| `buoyant_test` | Buoyant (SwiftUI-like, embedded-graphics): clock digits, animated toggle, sliding bar, changing rows |

Each logs frame statistics once per second, for example:
`slint/ppa: 66.1 fps (panel 66.1 Hz), render avg 6.9 ms max 11.3 ms …`.

## Why ESP-IDF and not esp-hal

The Tab5 on the desk has an **ESP32-P4 rev v1.3** (`espflash board-info`).
`esp-hal` 1.2 supports only P4 rev ≥ v3.0: its linker memory map, ROM symbols
and clock tree are written for v3. ESP-IDF 5.5 builds for rev < v3
(`CONFIG_ESP32P4_SELECTS_REV_LESS_V3`). Embassy runs on top of ESP-IDF as the
async executor (`embassy-executor` std platform, `embassy-time` driver from
`esp-idf-svc`). The project contains no C of its own: the panel init
sequences from Espressif's C BSP are ported to Rust tables, and the M5Stack
BSP component is not used.

## Build and flash

Prerequisites on macOS: `rustup`, `cargo install espflash ldproxy`. The pinned
nightly toolchain installs itself from `rust-toolchain.toml`. The first build
downloads ESP-IDF into `.embuild/` and compiles it (about 10 minutes).

```sh
cd firmware
cargo run --release --bin slint_test      # build, flash, open the monitor
cargo run --release --bin buoyant_test
cargo run --release --bin display_test
```

`cargo run` uses [`scripts/flash.sh`](scripts/flash.sh). It flashes with
ESP-IDF's own bootloader, which is built for P4 rev < v3. espflash's bundled
bootloader might assume rev ≥ v3. It also applies [`partitions.csv`](partitions.csv):
an 8 MB app partition. Set a port with
`ESPFLASH_ARGS="--port /dev/cu.usbmodem2101"`.

## Layout

```
src/
├─ runtime.rs           Embassy executor on the main FreeRTOS task
├─ audio/
│  ├─ uac.rs            USB Audio Class 1.0 output (Pebble) on ESP-IDF's host stack
│  ├─ file.rs           WAV and AIFF/AIFC readers (PCM 8–32 bit, mono/stereo)
│  └─ player.rs         decoder thread → PSRAM ring → USB stream; rubato to 48 kHz
├─ board/               PCB facts and bring-up order (Board::init)
│  ├─ i2c.rs            system I2C (GPIO31/32), bus recovery
│  ├─ ioexp.rs          PI4IOE5V6408 expanders 0x43/0x44: resets, rails
│  ├─ sd.rs             microSD: SD/MMC slot 0, 4-bit, FAT at /sdcard
│  └─ touch.rs          ST7123 touch controller (I2C 0x55)
├─ display/             MIPI-DSI panel, frame buffers, flips
│  ├─ mod.rs            Display: back_buffer().await, present(rows)
│  ├─ ppa.rs            PPA rotator: landscape strips → portrait targets
│  ├─ dma2d.rs          2D-DMA rectangle copies (esp_async_fbcpy)
│  └─ init_tables.rs    ILI9881C / ST7123 / ST7121 init sequences
├─ ui/
│  ├─ slint_platform.rs Slint Platform, offscreen windows, render loop
│  ├─ list_layer.rs     scrolling list composed by DMA from a row ring
│  ├─ touch.rs          touch task: polls the controller, landscape events
│  ├─ canvas.rs         landscape embedded-graphics DrawTarget (Buoyant)
│  └─ stats.rs          frame statistics
└─ bin/                 the test applications
ui/slint_test.slint     Slint test screen; fonts in ui/fonts (Inter, OFL)
```

`Board::init()`: I2C bus recovery → expanders (LCD and touch held in reset,
then released) → backlight PWM (GPIO22) → panel variant detected from the
touch controller (ST712x at 0x55 with firmware 3 → ST7123, 1 → ST7121;
GT911 → ILI9881C) → display. The unit tested here has an **ST7123**.

## Display pipeline

- DPI video mode streams one of **two RGB565 frame buffers in PSRAM**
  (2 × 1.8 MB) to the panel continuously. The CPU never sends pixels to the
  panel.
- Toolkits render **directly into the back buffer**. `Display::present(rows)`
  writes back the CPU cache only for the panel rows that changed, then selects
  that buffer for the next frame. The driver switches buffers without copying.
- `Display::back_buffer().await` returns once the DMA has started the next
  frame from the new buffer. The ISR (`on_frame_buf_complete`) wakes the task
  through esp-idf-hal's `HalIsrNotification`. Result: tear-free output, paced
  to the panel's refresh rate.
- The UI is landscape 1280×720 on a portrait panel. Landscape `(x, y)` maps
  to panel column `y`, row `1279 − x`, so the landscape top edge is the
  panel's left edge. Every path uses this mapping: the PPA (90°
  counter-clockwise), the list layer and the embedded-graphics canvas.
- Slint uses `RepaintBufferType::SwappedBuffers`, so it repaints only what
  changed in the last two frames.
- Slint renders **unrotated landscape lines into 16-line strips in internal
  RAM** (`src/display/ppa.rs`). Each finished strip is queued to the **PPA**
  (the P4's 2D accelerator), which rotates it by 90° into the PSRAM frame
  buffer by DMA while the CPU renders the next strip. The CPU never touches
  the frame buffer in this mode (the PPA's cache invalidation would discard
  its writes). Rendering in place in PSRAM and CPU rotation were measured
  first and dropped (table below).
- **Scrolling lists are composed, not rendered** (`src/ui/list_layer.rs`).
  Each row is rendered once by a Slint component in an offscreen window,
  when it scrolls into view. The PPA rotates it into one of 10 slots of a
  ring buffer in PSRAM, which is stored in the panel's orientation. Every
  frame the 2D-DMA copies the visible window of the ring into the frame
  buffer (one or two copies, depending on the wrap): 7.2 ms for 640×720 px,
  while Slint renders the rest of the screen and the next row is
  prefetched. The main Slint window leaves the list area as a plain
  rectangle that never changes. A `FrameHook` on the render loop hosts this.
- The ring holds 64 rows (7.9 MB). All 300 rows would need 37 MB, more
  than the 32 MB PSRAM. So the first 64 rows are pre-rendered at start
  (206 ms, ~3 ms per row), and the ring is kept filled around the scroll
  position: 40 rows ahead in the scroll direction and 15 behind. This runs
  in the spare time of each frame (up to 9 ms used) and continuously while
  the screen is still.
- Touch: `ui::touch::touch_task` polls the ST7123 every 10 ms. It counts a
  release only after 3 empty readings, because the controller sometimes
  skips a reading or times out on I2C mid-drag. It converts to landscape:
  the touch origin is rotated 180° against the display memory. The render
  loop drains the events each frame and routes them to the `FrameHook`
  (the list takes touches that start in its area, and the rest of a drag)
  or to the main Slint window. The list follows the finger and flings with
  exponential deceleration (τ 0.35 s).
- Overscroll: past either end, dragging meets rubber-band resistance
  (iOS-style, at most 360 px). After release, or when a fling runs past an
  end, a critically damped spring (ω 12/s, ~0.4 s) pulls the list back
  without bouncing. The gap is copied by the 2D-DMA from a blank picture
  in the list's background colour (filled once, written back from the
  cache), so overscroll renders nothing new.
- When Slint repaints over the list area (its full repaints in the first
  frames), the loop composes the list again after Slint in the same frame.
  Otherwise one buffer keeps Slint's background there, and the screen
  flickers between list and background on every flip.
- The PPA's scale-rotate-mirror engine moves only ~20 Mpx/s from PSRAM to
  PSRAM, rotated or not. The 2D-DMA copies at 64 Mpx/s. So the PPA only
  rotates things once (rows, Slint's dirty strips), and the per-frame bulk
  copy goes through the 2D-DMA (`ppa_probe` measures both).
- **PPA limitation on this chip:** with the panel DMA running, a rotated
  block narrower than 8 output pixels (a strip of fewer than 8 lines) can
  stall the PPA for good. `ppa_probe` reproduces it; ESP-IDF's DIG-734
  workaround computes the output width from the unrotated block. Short
  strips are therefore padded to 8 lines with rows read back from the frame
  buffer. Blocks wider than 640 px are split, and only one transaction is in
  flight at a time.
- Slint's renderer is vendored with a glyph fast path, and `i-slint-core`
  with room for 16 instead of 3 dirty rectangles
  ([vendor/README.md](vendor/README.md)).
- Keep things that change on their own small and apart. The clock is one
  fixed-width text item per field, so a tick dirties only the seconds. With
  one text for the whole time, plus 3 dirty rectangles, every tick
  repainted the full screen twice (once per buffer). That was a visible
  stutter in the scrolling list. `DEBUG_SLOW_FRAMES` in
  `src/ui/slint_platform.rs` logs the dirty rectangles of frames over
  35 ms.

## Measured on the device (ST7123, CPU 360 MHz, PSRAM 200 MHz)

The panel timings now follow M5Stack's M5GFX: 80 MHz pixel clock, 66.1 Hz,
1040 Mbps lanes, backlight PWM at 44.1 kHz. Espressif's BSP runs the panel at
70 MHz (57.8 Hz), and two init commands in its table are a byte too long.
That setup flickered visibly in black areas (about 3 Hz, stronger at higher
brightness, also on a static picture). With the M5GFX values the flicker is
gone, confirmed on the device in `panel_test` and `slint_test`. The table
below was measured at 57.8 Hz; at 66.1 Hz the Slint stress test reaches
20.7 fps with no underruns.

With double buffering locked to vsync, a frame takes a whole number of panel
periods (17.3 ms at 57.8 Hz): ≤ 17.3 ms → 58 fps, ≤ 34.6 ms → 29 fps,
≤ 52 ms → 19 fps.

| Test | Pixels per frame | CPU per frame | Result |
| --- | --- | --- | --- |
| `display_test`: full-screen fill + bar | all 921,600 | 18.2 ms | 28.9 fps |
| `slint_test`: half the screen scrolls + animation | 830–1190 panel rows | 40 ms (in place in PSRAM: 44 ms) | 18.6 fps |
| `slint_test` with PPA rotation + glyph fast path, at 66.1 Hz | 830–1190 panel rows | 25.4 ms | 31.1 fps |
| `slint_test` with the list layer, at 66.1 Hz | list: DMA copy of 640×720; Slint: ~75 rows | **6.9 ms avg, 11–12 ms max** | **66 fps** (panel rate) |
| `buoyant_test`: full redraw on every change | all | 48 ms | 19.3 fps |

Breakdown of the Slint frame: Slint itself 25 ms, software rotation +6 ms,
copying rows to PSRAM +9 ms. A full-screen CPU fill moves only about
100 MB/s to PSRAM, while the panel DMA reads about 107 MB/s at the same time.
The memory bus is shared.

Takeaways so far:

- Slint redraws only what changed. A static screen with a ticking clock costs
  a few rows per second. Buoyant always redraws the whole frame
  (≥ 18 ms just to clear it). For a 1280×720 panel this backs the earlier
  choice of Slint.
- With Slint rendering the whole list, the scroll stress test needed
  25.4 ms per frame (two vsyncs, 31 fps); 60 fps needs under 15.1 ms (one
  vsync at 66 Hz). Text was half the cost (see below), and Slint redraws
  the whole scrolling area every frame. The list layer removes that work:
  the list costs one DMA copy per frame plus one row render every ~17
  frames.

What the stress frame cost before the list layer (`PROFILE_STEPS`, since
removed from `slint_test`, measured before the glyph fast path):

| Scene | Render | fps |
| --- | --- | --- |
| full | 30.6 ms | 30 |
| without the bouncing dot | 29.2 ms | 31 |
| + solid tiles instead of gradients | 27.7 ms | 31 |
| + no list text | 13.6 ms | 55 |
| + plain rows only | 12.4 ms | 57 |

Sampling profile of that scene (`PROFILE` in `slint_test`, resolve with
`scripts/profile.sh`): 19 % idle, 11 % background fills, 10 % cache
maintenance inside the ESP-IDF PPA driver (it invalidates every panel row
a rotated strip touches, ~0.9 MB per strip), ~20 % glyphs and blending,
~6 % Slint per-line bookkeeping.

Open ends:

- The list layer handles fixed-height rows. Variable row heights and taps that do something (a tap only logs the row)
  are not done yet. A jump outside the ring renders all visible rows in one
  frame (~9 rows, one slow frame).
- The touch controller is polled; interrupt-driven waiting (GPIO23) comes
  with the sleep work. Only the ST7123/ST7121 variants are supported, not
  the ILI9881C variant's GT911.
- Bypassing the ESP-IDF PPA driver's per-strip cache invalidation (patch or
  own driver on top of the 2D-DMA) would cut ~10 % from Slint frames with
  large dirty areas.
- PPA fills for Buoyant's full-screen clears (18 ms of its 48 ms frame).

## USB audio (Creative Pebble V3 on USB-A)

`src/audio/uac.rs` is a UAC 1.0 speaker driver in Rust on top of ESP-IDF's
USB host library (no C class driver). `UsbAudio::start(vbus, source)`
installs the host and runs the client thread, which owns the whole
lifecycle: VBUS and root-port power (with retries), waiting for a device,
parsing its configuration descriptor, selecting the 48 kHz / 16 bit /
stereo alternate setting, setting the sample rate, unmuting and setting the
volume (feature unit), and tearing everything down when the device goes
away (transfers drained and freed, interface released, device closed) to
wait for the next one. The handle is `Clone`: `set_streaming(on)` switches
the streaming interface between alt 0 and the playing alternate setting
and keeps 3 isochronous transfers of 8 × 1 ms packets in flight, each
refilled from the sample source in its completion callback; a re-plugged
device resumes a wanted stream. `set_volume_db` / `set_mute` go to the
feature unit. Measured: 1000 packets/s, no failures.

What it took on the Tab5 (each was needed):

- **Full-speed-only host:** `HCFG.FSLSSupp` is set on the high-speed
  controller after `usb_host_install`, before the root port is powered.
  Without it the full-speed Pebble never enumerated on the USB-A port
  ("Root port reset failed"; same symptom as m5stack/M5Stack#344). The
  first two resets still fail; the host stack's root-port recovery then
  enumerates the device about 1 s after the port is powered. Newer
  `espressif/usb` releases call this `fsls_only`, but it needs a newer
  ESP-IDF HAL than 5.5.5.
- **Order:** install the host with the root port unpowered, switch on
  USB5V_EN, wait 1 s, then power the root port (retries by power cycling).
- **`SET_INTERFACE`:** ESP-IDF's `usb_host_interface_claim` does not send
  it. Without it the Pebble acknowledges every packet but stays silent.
- The Pebble's real descriptors: VID 041e / PID 3272, control interface 1,
  feature unit 9 (volume −60…0 dB in 0.6 dB steps, persists across
  power cycles), streaming interface 2 alt 1, isochronous OUT endpoint
  0x02, 192 bytes per 1 ms, 48 kHz only, sample-rate control on the
  endpoint. Its LED: solid green = USB audio (low gain), flashing green =
  Bluetooth pairing (USB set-up failed).

**Player** (`audio::player`): a decoder thread reads WAV/AIFF from the SD
card (`audio::file`), converts other sample rates to 48 kHz with rubato's
FFT resampler (44.1 kHz: blocks of 1029 frames) and fills a one-second
ring in PSRAM; the USB stream takes from the ring and plays silence on
underrun. Measured: 48 kHz AIFF and a 44.1 kHz WAV play with 0 underruns.

**microSD** (`board::sd`): SD/MMC slot 0 (IO_MUX pins 43/44/39–42), 4 bit,
FAT with long file names (`CONFIG_FATFS_LFN_HEAP`). On-chip LDO channel 4
is switched on for the SD pins as the BSP does. If the card never answers
(`send_op_cond` timeout), check that it is fully inserted: that was the
cause here. macOS `._*` metadata files are skipped.

Open: power budget measurement at higher volumes, FLAC (`claxon`).

## Not done yet

Touch is wired to Slint and the list layer, not to Buoyant. Also missing:
RTC, Wi-Fi, sleep.
