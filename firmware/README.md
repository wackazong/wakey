# Firmware

Rust (std) on ESP-IDF 5.5.5 with the Embassy executor, for the M5Stack Tab5. Design: [docs/architecture.md](../docs/architecture.md). Traps: [docs/pitfalls.md](../docs/pitfalls.md).

## Programs

| Binary | What it is |
| --- | --- |
| `wakey` | The alarm clock: clock screen (RTC time, Europe/Zurich, date, next alarm, Wi-Fi and speaker status), the alarm ringing on the USB speaker (snooze, stop), Wi-Fi + NTP, touch |
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

- `config.json`: `wifi` (`ssid`, `password`) and `alarm` (`enabled`, `hour`, `minute`, `weekdays` Monday to Sunday as 0/1, optional `sound` path such as `"sounds/morgen.wav"`, `volume` 0–100, `fade_in_s`, `snooze_min`, `max_snoozes`, `ring_timeout_min`). The firmware adds missing settings at their defaults (alarm off, 06:30 Monday to Friday, 3 snoozes of 9 min, 10 min ring timeout), so after the first boot the file lists everything. More fields are planned, see [docs/roadmap.md](../docs/roadmap.md).
- For development the build environment can provide Wi-Fi credentials (`WAKEY_WIFI_SSID=… WAKEY_WIFI_PASSWORD=…`) and a test alarm (`WAKEY_ALARM=07:38`: enabled, every day, overrides the file) to `cargo run --release --bin wakey`. Flash a build without `WAKEY_ALARM` afterwards, or it rings every day at that time.
- Sound files (WAV or AIFF/AIFC, PCM 8–32 bit, mono or stereo, any sample rate) in the root or `sounds/`.

## Layout

```
src/
├─ lib.rs              layer overview
├─ app/                the alarm clock
│  ├─ mod.rs           start-up wiring (app::main)
│  ├─ alarm.rs         the alarm: state machine, NVS, speaker
│  └─ clock_screen.rs  clock screen view model (time, date, alarm, Wi-Fi, speaker)
├─ ui/
│  ├─ platform.rs      Slint platform, windows, rendering through the PPA
│  ├─ render_loop.rs   frame loop, FrameHook
│  ├─ list_layer.rs    scrolling list composed by 2D-DMA from a row ring
│  ├─ input.rs         touch task → landscape pointer events
│  └─ stats.rs         frame statistics
├─ audio/
│  ├─ uac/             USB Audio Class 1.0 speaker: handle, host, device
│  └─ player.rs        file / tone playback: decoder thread → ring → stream
├─ net.rs              Wi-Fi via the C6, reconnect, NTP
├─ clock.rs            system time from the RTC, local time, RTC sync
├─ config.rs           config.json from the SD card
├─ board/              Tab5 hardware, Board::init
│  ├─ i2c.rs           shared system I2C bus, bus recovery
│  ├─ ioexp.rs         two PI4IOE5V6408 expanders: resets, power switches
│  ├─ rtc.rs           RX8130CE
│  ├─ touch.rs         ST7123 touch controller
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
ui/                    .slint files (one per program), fonts (Inter, OFL)
vendor/                patched Slint and esp-idf-svc (see vendor/README.md)
scripts/               flash.sh (cargo runner), profile.sh
sdkconfig.defaults     ESP-IDF configuration (commented)
```

## How to …

**Add a screen.** Add the component to `ui/wakey.slint` (one font face, fixed-width `Text` items for values that change on their own), create it in `app/mod.rs`, and put its view model next to `app/clock_screen.rs`: set only properties that changed. Characters that only come from Rust strings need a literal in the `.slint` file, or they are not rendered. Check the image size: `cargo run` prints `App/part. size`.

**Test the alarm.** `WAKEY_ALARM=HH:MM cargo run --release --bin wakey` a few minutes ahead; the log shows `alarm: Ringing …`, `uac: stream on`, and `alarm: Idle` after the ring timeout. Unit tests for the rules are in `wakey-core/src/alarm.rs`.

**Add a service.** A module (or directory) below `src/`, owning its thread or Embassy task, taking `board` handles or `Switch`es as arguments and returning a cheap `Clone` handle. Put anything computable without hardware into `wakey-core` with tests.

**Add a chip on the I2C bus.** A driver in `board/` that holds a `SharedI2c` clone and locks it per transaction; create it in `Board::init` (panic with the chip name if it is required).

**Measure performance.** Frame statistics are logged once per second (`slint/ppa: 66.1 fps (panel 66.1 Hz), render avg …`). `DEBUG_SLOW_FRAMES` in `ui/render_loop.rs` logs the dirty rectangles of frames over 35 ms. `PROFILE` in `bin/ui_bench.rs` runs the sampling profiler for 8 s; resolve the log with `scripts/profile.sh <elf> <log>`.

**Debug USB or SD.** `esp_log_level_set` per tag at run time (`CONFIG_LOG_MAXIMUM_LEVEL_DEBUG` allows it); `DEBUG_LOGS` in `board/sd.rs`.
