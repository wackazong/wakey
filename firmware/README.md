# Firmware

Rust (std) on ESP-IDF 5.5.5 with the Embassy executor, for the M5Stack Tab5. Design: [docs/architecture.md](../docs/architecture.md). Traps and their fixes: [docs/pitfalls.md](../docs/pitfalls.md).

## Programs

| Binary | What it is |
| --- | --- |
| `wakey` | The alarm clock: clock page (RTC time, Europe/Zurich, date, sky, next alarm, symbols for a missing speaker or network), alarm list, alarm, sound and name pages (tap the alarm line), the alarms ringing on the USB or the built-in speaker (snooze, stop), wheel clicks, TIDAL playlist and downloads, AirDrop, Wi-Fi + NTP, backlight from the camera, display sleep, touch |
| `ui_bench` | Slint and list-layer bench: clock digits, an animation and a 300-row list to drag and fling; frame statistics on screen and in the log (target 66 fps) |

## Build and flash

Prerequisites (macOS): `rustup`, `cargo install espflash ldproxy`. The nightly toolchain installs itself from `rust-toolchain.toml`. The first build downloads and compiles ESP-IDF into `.embuild/` (~10 min).

```sh
cd firmware
cargo run --release --bin wakey          # build, flash, open the monitor
ESPFLASH_ARGS="--port /dev/cu.usbmodem2101" cargo run --release --bin ui_bench
cargo build --release --bins             # build only
```

`cargo run` uses [`scripts/flash.sh`](scripts/flash.sh): ESP-IDF's bootloader (built for P4 rev < v3) and [`partitions.csv`](partitions.csv) (12 MB app, NVS, and `c6`: the C6's images for AirDrop, packed by [`c6/pack.py`](c6/pack.py), ~3.1 MB). The `c6` partition is written only when its contents differ from the packed images (compared by MD5 on the device). espflash prints the app's size against its partition (`App/part. size`). Extra espflash options go into `ESPFLASH_ARGS`.

Host tests of the pure logic: `cd ../wakey-core && cargo test`.

Before committing: `cargo fmt`, `cargo clippy --release --bins` (here and in `wakey-core`), and the programs you touched run on the device. See [CONTRIBUTING.md](../CONTRIBUTING.md).

## SD card

FAT, in the root:

- `config.json` and `sounds/`: see [Configuration](../README.md#configuration). The clock keeps TIDAL's files in `tidal/`.
- For development the build environment can provide Wi-Fi credentials (`WAKEY_WIFI_SSID=… WAKEY_WIFI_PASSWORD=…`, used when the file has none) and a test alarm (`WAKEY_ALARM=07:38`: one alarm "Test", every day, replacing the file's alarms while that build runs) to `cargo run --release --bin wakey`. Flash a build without `WAKEY_ALARM` afterwards, or it rings every day at that time (and saving on the alarm pages writes the test alarm into the file).

## Layout

```
src/
├─ lib.rs              layer overview
├─ app/                the alarm clock
│  ├─ mod.rs           start-up wiring (app::main), page changes, idle timeout
│  ├─ alarm.rs         the alarms: state machine, NVS, speaker, saving, preview
│  ├─ alarm_list.rs    alarm list page view model, its rows for the list layer
│  ├─ alarm_page.rs    alarm page view model (the draft being edited)
│  ├─ sound_page/      sound page view model: mod.rs (sound, volume, fade-in, preview),
│  │                   tidal.rs (TIDAL tab), airdrop.rs (Files tab's AirDrop rows),
│  │                   rows.rs (rows for the list layer)
│  ├─ name_page.rs     name keyboard view model
│  ├─ clock_screen.rs  clock page view model (time, date, sky, alarm, missing speaker/network)
│  ├─ power.rs         when the display sleeps and wakes, USB-A power
│  ├─ texts.rs         German texts for what the services report
│  └─ bench.rs         scripted gestures for measuring (feature ui-bench)
├─ ui/
│  ├─ platform.rs      Slint platform, windows, rendering through the PPA
│  ├─ render_loop.rs   frame loop, FrameHook, touch routing
│  ├─ list_layer.rs    list scrolled by 2D-DMA from a row ring, Slint when still
│  ├─ input.rs         touch thread → landscape pointer events
│  ├─ transition.rs    page slides by 2D-DMA
│  ├─ power.rs         display sleep request to the render loop
│  ├─ dim.rs           night dimming of the pixels
│  ├─ stats.rs         frame statistics, free memory
│  └─ trace.rs         per-frame log (feature ui-bench)
├─ audio/
│  ├─ uac/             USB Audio Class 1.0 speaker: handle, host, device
│  ├─ player.rs        file / tone playback: decoder thread → ring → stream
│  ├─ builtin.rs       built-in speaker: mixer thread (sound, clicks), amplifier
│  └─ output.rs        player → USB speaker, or the built-in one without it
├─ net.rs              Wi-Fi sessions via the C6, NTP, jobs, handing the C6 to AirDrop
├─ http.rs             HTTPS client with a connection pool
├─ tidal/              TIDAL: mod.rs (state, jobs, selection), session.rs (API, login),
│                      playlist.rs (sync, covers), download.rs, card_writer.rs,
│                      growing.rs (reading a track while it downloads), store.rs (files on the card)
├─ airdrop.rs          AirDrop: switching the C6 to the receiver, receiving files
├─ c6/                 the C6: mod.rs (power, esp_hosted transport, set-up), peer.rs
│                      (Wakey's peer-data messages), ota.rs (images from the c6 partition, OTA)
├─ ambient.rs          camera as light meter, backlight
├─ clock.rs            system time from the RTC, local time, RTC sync
├─ config.rs           config.json from the SD card
├─ sounds.rs           sounds/ on the SD card, choosing today's random file
├─ board/              Tab5 hardware, Board::init
│  ├─ i2c.rs           shared system I2C bus, bus recovery
│  ├─ ioexp.rs         two PI4IOE5V6408 expanders: resets, power switches
│  ├─ rtc.rs           RX8130CE
│  ├─ touch.rs         ST7123 touch controller
│  ├─ codec.rs         ES8388 codec (built-in speaker), its I2S pins
│  ├─ camera.rs        SC202CS camera sensor (light meter)
│  ├─ ina226.rs        INA226 battery current
│  ├─ sd.rs            microSD (SDMMC slot 0, LDO 4), card paths, safe file writes
│  └─ backlight.rs     LEDC PWM on GPIO22
├─ display/            DSI panel, frame buffers, flips
│  ├─ mod.rs           Display: back_buffer().await, present(), link rebuilds
│  ├─ init_tables.rs   ILI9881C / ST7123 / ST7121 init sequences
│  ├─ ppa.rs           PPA rotator: landscape strips → portrait pictures
│  ├─ dma2d.rs         2D-DMA rectangle copies (pictures, blits)
│  └─ cache.rs         cache write-back / invalidate for DMA buffers
├─ runtime.rs          Thread helper (core, priority, PSRAM stack), Embassy executor on its own task
├─ psram.rs            cache-line aligned PSRAM buffers for DMA
├─ taskstats.rs        CPU time per task (feature task-stats)
└─ bin/                wakey, ui_bench
ui/                    wakey.slint (window) + wakey/ (pages, theme, wheel), ui_bench.slint, fonts (Inter, OFL)
vendor/                patched Slint, esp-idf-svc and ESP-IDF's esp_lcd (see vendor/README.md)
c6/                    the C6's images and their sources (see c6/README.md)
scripts/               flash.sh (cargo runner)
sdkconfig.defaults     ESP-IDF configuration (commented)
partitions.csv         flash layout
```

## Features

| Feature | Effect |
| --- | --- |
| `ui-bench` | Scripted touch gestures after boot and a per-frame trace in the log (below) |
| `sky-demo` | The clock's sky runs through today in one minute, over and over |
| `debug-overlay` | A box at the top left of every page: pixel dimming, backlight level, light reading (day/night), and when the display sleeps or why not |
| `sleep-cycle-test` | The display sleeps and wakes every 8 s regardless of the conditions; the free memory is logged around each link rebuild |
| `task-stats` | CPU time per task every 5 s |

Flash a build without the feature afterwards.

## How to …

**Add a page.** A component in `ui/wakey/` with a global for its data and callbacks, using the controls in `theme.slint`; a variant of the `Page` enum and an `if root.page == Page.…` in `ui/wakey.slint` (export the global there); a view model in `app/` that fills the global and handles its callbacks (`Ctx::with` for callbacks that need the window and the shared state), set up in `app/mod.rs`, and the page's depth for the slide direction (`depth` in `app/mod.rs`). Add a scene to `tools/ui-preview` and look at the PNG before flashing. Set only properties that changed, use fixed-width `Text` items for values that change on their own, and keep an edit's repaint small (see the Slint entries in `docs/pitfalls.md`). Reuse the font sizes in use (listed in `ui/wakey.slint`); characters that only come from Rust strings need a literal in a `.slint` file. Check the image size: `cargo run` prints `App/part. size`.

**Look at the pages without the device.** `cd tools/ui-preview && cargo run --release -- [--by-line] [dir] [scene …]` renders every scene (sample data, some with simulated drags), or the named ones (a trailing `*` matches a prefix), to `dir` (default `target/ui-preview`) as PNGs and prints the render time per scene. Same `.slint` files, same Slint and renderer; `--by-line` renders line by line as the firmware does. It also checks that the alarm list's rows look exactly like the list layer's rendering of them (panics otherwise). Timing on the device is a different matter (~50 times slower).

**Watch the sky through a day.** `cargo run --release --bin wakey --features sky-demo`: the clock's sky (`wakey_core::sky`) runs through today in one minute, stepping every 200 ms; the bottom line shows that time of day and the sun's elevation instead of the next alarm.

**Measure the UI on the device.** `cargo run --release --bin wakey --features ui-bench`: ten seconds after boot a scripted run (`app/bench.rs`) opens the alarm list, holds a row (the first press), flings the list, stops a fling with a tap (must not open the alarm), presses, swipes a row (short of deleting: it springs back), toggles a switch twice (saved, ends as it began), turns the minute wheel of an alarm and goes back, and every frame is logged (`ui/trace.rs`): `frame … ms, gap … ms, dirty (x, y, w, h), … Mcycles, … Minstr`, each step announced as `bench: <step>`. The list needs more alarms than fit the screen for the scroll steps. Cycles per instruction near 1.3 mean the work itself is the cost; near 5, memory stalls (see the internal RAM entry in `docs/pitfalls.md`).

**Measure performance.** Frame statistics are logged once per second (`slint/ppa: 66.1 fps (panel 66.1 Hz), render avg …`), with free internal, DMA and PSRAM memory and the largest DMA and PSRAM blocks. A Slint render over 35 ms logs its dirty rectangles (`slint: slow frame …`). `--features task-stats` logs CPU time per task.

**Test the alarm.** `WAKEY_ALARM=HH:MM cargo run --release --bin wakey` a few minutes ahead; the log shows `alarm: Ringing …`, `uac: stream on`, and `alarm: Idle` after the ring timeout. Unit tests for the rules are in `wakey-core/src/alarm.rs`.

**Add a service.** A module (or directory) below `src/`, owning its thread (`runtime::Thread`) or Embassy task, taking `board` handles or `Switch`es as arguments and returning a cheap `Clone` handle; texts for the UI stay in `app`. Put anything computable without hardware into `wakey-core` with tests.

**Add a chip on the I2C bus.** A driver in `board/` that holds a `SharedI2c` clone and locks it per transaction; create it in `Board::init` (panic with the chip name if it is required).

**Debug USB, SD or a module.** ESP-IDF's log level can be raised per tag at run time (`esp_log_level_set`; `CONFIG_LOG_MAXIMUM_LEVEL_DEBUG` allows up to debug), e.g. `usb_host` or the SD driver's `sdmmc_*` tags. Rust `log::debug!` lines (e.g. the list layer's row timing) show when their target's level is raised the same way (`EspLogger::set_target_level`).

## Licensing

Wakey's own code is MIT OR Apache-2.0. The firmware links Slint under GPL-3.0, so a distributed firmware binary is GPL-3.0 as a whole; the Inter font is OFL-1.1 ([`ui/fonts/OFL.txt`](ui/fonts/OFL.txt)), the C6 images' components are listed in [`c6/THIRD_PARTY_NOTICES.md`](c6/THIRD_PARTY_NOTICES.md). Details: [License](../README.md#license).
