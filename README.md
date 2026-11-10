# Wakey

A bedside alarm clock on the M5Stack Tab5 (ESP32-P4, 5" 1280×720 touch panel) that plays sound files from its microSD card on a Creative Pebble V3 USB speaker. Written in Rust (std) on ESP-IDF with the Embassy executor. **Rust only:** no project-authored C/C++ and no C board support packages.

Working today: clock screen with the RTC time (Europe/Zurich), one alarm (weekdays, snooze, fade-in, set in `config.json`) ringing on the speaker, Wi-Fi and NTP through the on-board ESP32-C6, touch, and a list layer that scrolls at the panel's 66 fps. Next: the alarm settings screen ([roadmap](docs/roadmap.md)).

## Repository

| Path | Content |
| --- | --- |
| [`firmware/`](firmware/README.md) | the firmware (own Cargo workspace, ESP32-P4 target): build, flash, layout |
| [`wakey-core/`](wakey-core/src/lib.rs) | hardware-independent logic, tested on the host |
| [`docs/architecture.md`](docs/architecture.md) | how the software works: layers, threads, display, audio, time, network |
| [`docs/hardware.md`](docs/hardware.md) | Tab5 pins, I2C devices, expanders, power, the speaker |
| [`docs/pitfalls.md`](docs/pitfalls.md) | traps already hit and their fixes — read before changing an area |
| [`docs/roadmap.md`](docs/roadmap.md) | what comes next and the decisions for it |
| [`docs/schematics/`](docs/schematics) | Tab5 schematic pages |
| [`spikes/tidal-download/`](spikes/tidal-download/README.md) | host tool: download a track from your TIDAL account (V2 groundwork) |

## Quick start

```sh
cd firmware
cargo run --release --bin wakey      # first build ~10 min (ESP-IDF)
cd ../wakey-core && cargo test       # host tests
```

Put `config.json` with `{ "wifi": { "ssid": "…", "password": "…" } }` and sound files on the SD card. Details in [firmware/README.md](firmware/README.md).

Each directory with a `Cargo.toml` is its own workspace with its own toolchain: the firmware uses a pinned nightly and an ESP target, the others the host toolchain. Run Cargo from inside them.
