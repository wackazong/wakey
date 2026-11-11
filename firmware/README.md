# Firmware

Rust (std) on ESP-IDF 5.5.5 with the Embassy executor, for the M5Stack Tab5. Design: [docs/architecture.md](../docs/architecture.md). Traps: [docs/pitfalls.md](../docs/pitfalls.md).

## Programs

| Binary | What it is |
| --- | --- |
| `wakey` | The alarm clock: clock page (RTC time, Europe/Zurich, date, next alarm, symbols for a missing speaker or network), alarm list, alarm, sound and name pages (tap the alarm line), the alarms ringing on the USB speaker (snooze, stop), wheel clicks on the built-in speaker, Wi-Fi + NTP, touch |
| `ui_bench` | Slint and list-layer bench: clock digits, an animation and a 300-row list to drag and fling; frame statistics on screen and in the log (target 66 fps) |
| `audio_test` | USB speaker test: plays every WAV/AIFF in the SD card root and `sounds/` in a loop (writes a 44.1 kHz test tone first), or a 440 Hz sine without a card; logs packets and underruns; survives re-plugging |

## Build and flash

Prerequisites (macOS): `rustup`, `cargo install espflash ldproxy`. The nightly toolchain installs itself from `rust-toolchain.toml`. The first build downloads and compiles ESP-IDF into `.embuild/` (~10 min).

```sh
cd firmware
cargo run --release --bin wakey          # build, flash, open the monitor
ESPFLASH_ARGS="--port /dev/cu.usbmodem2101" cargo run --release --bin ui_bench
cargo build --release --bins             # build only
```

`cargo run` uses [`scripts/flash.sh`](scripts/flash.sh): ESP-IDF's bootloader (built for P4 rev < v3) and [`partitions.csv`](partitions.csv) (12 MB app, NVS). Extra espflash options go into `ESPFLASH_ARGS`.

Host tests of the pure logic: `cd ../wakey-core && cargo test`.

Before committing: `cargo fmt`, `cargo clippy --release --bins` (here and in `wakey-core`), and the programs you touched run on the device.

## SD card

FAT, in the root:

- `config.json`: `wifi` (`ssid`, `password`), `alarms` (a list the settings pages edit; each with `id`, optional `name`, `enabled`, `hour`, `minute`, `weekdays` Monday to Sunday as 0/1, `sound`, `volume` 1–100, `fade_in_s`) and `ringing` (`snooze_min`, `max_snoozes`, `ring_timeout_min`, shared by all alarms). `sound` is `"beep"` (built-in tone), `"random"` (a different file from `sounds/` each day), or a path such as `"sounds/morgen.wav"`. A file with the old single `alarm` section is migrated on boot. The firmware adds missing settings at their defaults (3 snoozes of 9 min, 10 min ring timeout), so after the first boot the file lists everything. More fields are planned, see [docs/roadmap.md](../docs/roadmap.md).
- `sounds/`: WAV and AIFF files for the alarms, listed by file name (`_` shown as a space).
- For development the build environment can provide Wi-Fi credentials (`WAKEY_WIFI_SSID=… WAKEY_WIFI_PASSWORD=…`) and a test alarm (`WAKEY_ALARM=07:38`: one alarm "Test", every day, replacing the file's alarms while that build runs) to `cargo run --release --bin wakey`. Flash a build without `WAKEY_ALARM` afterwards, or it rings every day at that time (and saving on the alarm pages writes the test alarm into the file).
- Sound files (WAV or AIFF/AIFC, PCM 8–32 bit, mono or stereo, any sample rate) in the root or `sounds/`.

## Layout

```
src/
├─ lib.rs              layer overview
├─ app/                the alarm clock
│  ├─ mod.rs           start-up wiring (app::main)
│  ├─ alarm.rs         the alarms: state machine, NVS, speaker, saving, preview
│  ├─ alarm_list.rs    alarm list page view model, its rows for the list layer
│  ├─ alarm_page.rs    alarm page view model (the draft being edited)
│  ├─ sound_page.rs    sound page view model (sound, volume, fade-in, preview)
│  ├─ name_page.rs     name keyboard view model
│  ├─ clock_screen.rs  clock page view model (time, date, alarm, missing speaker/network)
│  └─ bench.rs         scripted gestures for measuring (feature ui-bench)
├─ ui/
│  ├─ platform.rs      Slint platform, windows, rendering through the PPA
│  ├─ render_loop.rs   frame loop, FrameHook, touch routing
│  ├─ list_layer.rs    list scrolled by 2D-DMA from a row ring, Slint when still
│  ├─ input.rs         touch task → landscape pointer events
│  ├─ stats.rs         frame statistics, free heap
│  └─ trace.rs         per-frame log (feature ui-bench)
├─ audio/
│  ├─ uac/             USB Audio Class 1.0 speaker: handle, host, device
│  ├─ player.rs        file / tone playback: decoder thread → ring → stream
│  ├─ builtin.rs       built-in speaker: mixer thread (sound, clicks), amplifier
│  └─ output.rs        player → USB speaker, or the built-in one without it
├─ net.rs              Wi-Fi via the C6, reconnect, NTP
├─ clock.rs            system time from the RTC, local time, RTC sync
├─ config.rs           config.json from the SD card
├─ sounds.rs           sounds/ on the SD card, choosing today's random file
├─ board/              Tab5 hardware, Board::init
│  ├─ i2c.rs           shared system I2C bus, bus recovery
│  ├─ ioexp.rs         two PI4IOE5V6408 expanders: resets, power switches
│  ├─ rtc.rs           RX8130CE
│  ├─ touch.rs         ST7123 touch controller
│  ├─ codec.rs         ES8388 codec (built-in speaker), its I2S pins
│  ├─ sd.rs            microSD (SDMMC slot 0, LDO 4)
│  └─ backlight.rs     LEDC PWM on GPIO22
├─ display/            DSI panel, frame buffers, flips
│  ├─ mod.rs           Display: back_buffer().await, present()
│  ├─ init_tables.rs   ILI9881C / ST7123 / ST7121 init sequences
│  ├─ ppa.rs           PPA rotator: landscape strips → portrait pictures
│  ├─ dma2d.rs         2D-DMA rectangle copies
│  └─ cache.rs         cache write-back / invalidate for DMA buffers
├─ runtime.rs          Embassy executor on its own 64 KiB task
├─ profiler.rs         sampling profiler (core 0)
└─ bin/                wakey, ui_bench, audio_test
ui/                    wakey.slint (window) + wakey/ (pages, theme, wheel), ui_bench.slint, fonts (Inter, OFL)
vendor/                patched Slint and esp-idf-svc (see vendor/README.md)
scripts/               flash.sh (cargo runner), profile.sh
sdkconfig.defaults     ESP-IDF configuration (commented)
```

## How to …

**Add a page.** A component in `ui/wakey/` with a global for its data and callbacks, using the controls in `theme.slint`; a `page` value and an `if` in `ui/wakey.slint` (export the global there); a view model in `app/` that fills the global and handles its callbacks (`Ctx::with` for callbacks that need the window and the shared state), set up in `app/mod.rs`. Add a scene to `tools/ui-preview` and look at the PNG before flashing. Set only properties that changed, use fixed-width `Text` items for values that change on their own, and keep an edit's repaint small (see the dirty-rectangle entry in `docs/pitfalls.md`). Reuse the font sizes in use; characters that only come from Rust strings need a literal in a `.slint` file. Check the image size: `cargo run` prints `App/part. size`.

**Look at the pages without the device.** `cd tools/ui-preview && cargo run --release -- [--by-line] [dir] [name filter]` renders every scene (sample data, some with simulated drags) to `dir` (default `target/ui-preview`) as PNGs and prints the render time per scene. Same `.slint` files, same Slint and renderer; `--by-line` renders line by line as the firmware does. It also checks that the alarm list's rows look exactly like the list layer's rendering of them (panics otherwise). Timing on the device is a different matter (~50 times slower, see below).

**Watch the sky through a day.** `cargo run --release --bin wakey --features sky-demo`: the clock's sky (`wakey_core::sky`) runs through today in one minute, over and over, stepping every 200 ms; the bottom line shows that time of day and the sun's elevation instead of the next alarm. Flash a build without the feature afterwards.

**Measure the UI on the device.** `cargo run --release --bin wakey --features ui-bench`: ten seconds after boot a scripted run (`app/bench.rs`) opens the alarm list, holds a row (the first press), flings the list, stops a fling with a tap (must not open the alarm), presses, swipes a row (short of deleting: it springs back), toggles a switch twice (saved, ends as it began) and goes back, and every frame is logged (`ui/trace.rs`): `frame … ms, gap … ms, dirty (x, y, w, h), … Mcycles, … Minstr`, each step announced as `bench: <step>`. Cycles per instruction near 1.3 mean the work itself is the cost; near 5, stalls (see the internal RAM entry in `docs/pitfalls.md`). The per-second stats line shows free internal and DMA memory. `scripts/profile.sh` resolves the sampling profiler's output (`profiler::start`/`report`). Flash a build without the feature afterwards.

**Test the alarm.** `WAKEY_ALARM=HH:MM cargo run --release --bin wakey` a few minutes ahead; the log shows `alarm: Ringing …`, `uac: stream on`, and `alarm: Idle` after the ring timeout. Unit tests for the rules are in `wakey-core/src/alarm.rs`.

**Add a service.** A module (or directory) below `src/`, owning its thread or Embassy task, taking `board` handles or `Switch`es as arguments and returning a cheap `Clone` handle. Put anything computable without hardware into `wakey-core` with tests.

**Add a chip on the I2C bus.** A driver in `board/` that holds a `SharedI2c` clone and locks it per transaction; create it in `Board::init` (panic with the chip name if it is required).

**Measure performance.** Frame statistics are logged once per second (`slint/ppa: 66.1 fps (panel 66.1 Hz), render avg …`). `DEBUG_SLOW_FRAMES` in `ui/render_loop.rs` logs the dirty rectangles of frames over 35 ms. `PROFILE` in `bin/ui_bench.rs` runs the sampling profiler for 8 s; resolve the log with `scripts/profile.sh <elf> <log>`.

**Debug USB or SD.** `esp_log_level_set` per tag at run time (`CONFIG_LOG_MAXIMUM_LEVEL_DEBUG` allows it); `DEBUG_LOGS` in `board/sd.rs`.
