# Wakey

A bedside alarm clock for the [M5Stack Tab5](https://docs.m5stack.com/en/core/Tab5): a 5" touch screen with a sky that follows the sun, alarms set by touch, and wake-up sounds from the microSD card, a TIDAL playlist, or sent from an iPhone or Mac by AirDrop. The firmware is written in Rust on ESP-IDF.

<p align="center">
  <img src="docs/images/clock-morning.jpg" width="49%" alt="Clock face in the morning: large time, date, next alarm">
  <img src="docs/images/clock-evening.jpg" width="49%" alt="Clock face at sunset with an orange sky">
</p>
<p align="center">
  <img src="docs/images/alarm-list.png" width="32%" alt="Alarm list with switches">
  <img src="docs/images/alarm.png" width="32%" alt="Alarm page with time wheels, weekdays and sound">
  <img src="docs/images/sound-tidal.png" width="32%" alt="Sound page with the TIDAL playlist">
</p>

The screenshots are rendered from the firmware's own UI code by [`tools/ui-preview`](tools/ui-preview/src/main.rs), pixel for pixel as on the device. The UI is in German.

## Features

- **Clock face** with large rolling digits, the date and the next alarm. The background sky follows the sun's elevation through the day: blue noon, orange sunset, a dark purple night.
- **Several alarms**, each with a time, weekdays, a name, a sound, volume and fade-in. They are set on the touch screen with time wheels that click like a mechanical dial. Snooze with a tap, stop by holding.
- **Wake-up sounds** from `sounds/` on the microSD card (WAV, AIFF, FLAC, MP3, AAC and ALAC in M4A), a random one each day, or tracks from a **TIDAL playlist**, downloaded to the card in lossless quality so an alarm never depends on the network.
- **AirDrop receiver:** open the sound page's file tab and send sound files from an iPhone or a Mac to "Wakey". The Tab5's ESP32-C6 runs a receiver for the time the tab is open.
- **Speakers:** a USB speaker (USB Audio Class 1.0, e.g. a Creative Pebble V3) on the Tab5's USB-A port, or the built-in speaker as a fallback.
- **Kind to a bedroom:** the camera works as a light meter and dims the screen in a dark room; the display sleeps in the dark and wakes on touch, at daylight or when an alarm rings.
- **Network only when needed:** Wi-Fi is on for a clock sync (NTP) once a day and while TIDAL is in use, and off otherwise. The real-time clock keeps the time in between.
- **Smooth touch UI:** 66 fps scrolling and swiping on an ESP32-P4, with lists composed by the 2D-DMA engine and pages rendered by Slint through the pixel-processing accelerator.

## Hardware

- [M5Stack Tab5](https://docs.m5stack.com/en/core/Tab5) (ESP32-P4 with ESP32-C6, 1280×720 touch panel). The firmware supports the ST7123 panel variant.
- A microSD card (FAT).
- Optional: a USB speaker for the USB-A port.

Details on the board, pins and power: [docs/hardware.md](docs/hardware.md).

## Getting started

Prerequisites: [rustup](https://rustup.rs) and `cargo install espflash ldproxy`. The firmware's pinned nightly toolchain installs itself; the first build downloads and compiles ESP-IDF (about ten minutes).

```sh
git clone https://github.com/wackazong/wakey && cd wakey/firmware
cargo run --release --bin wakey     # build, flash over USB-C, open the log
```

Then prepare the microSD card:

1. Create `config.json` in its root with your Wi-Fi (a 2.4 GHz network): `{ "wifi": { "ssid": "MyNetwork", "password": "secret" } }`. Everything else has defaults and is added to the file at the first start.
2. Put sound files into a folder `sounds/`. The file name is what the clock shows (`_` becomes a space).
3. Insert the card and start the clock. Tap the alarm line at the bottom of the clock face to set alarms.

On the first start with network the clock also installs its own firmware on the ESP32-C6 (Wi-Fi with a small addition, and the AirDrop receiver), in the background in about a minute; nothing needs to be done by hand. Building and flashing in detail: [firmware/README.md](firmware/README.md).

## Configuration

The clock reads `config.json` from the card at start and writes the alarms back when they are changed on the screen. Settings missing from the file are added with their defaults. A file with a JSON error stops the clock on an error page that shows the parser's message; the file is not overwritten.

```json
{
  "wifi": { "ssid": "MyNetwork", "password": "secret" },
  "sync_interval_h": 24,
  "alarms": [
    { "id": 1, "name": "Arbeit", "enabled": true, "hour": 6, "minute": 30,
      "weekdays": [1, 1, 1, 1, 1, 0, 0], "sound": "random",
      "volume": 60, "fade_in_s": 10 }
  ],
  "ringing": { "snooze_min": 9, "max_snoozes": 3, "ring_timeout_min": 10 },
  "display": { "night_pixels_percent": 35 },
  "power": { "sleep_after_s": 60, "sleep_when": "dark" },
  "tidal": { "client_id": "…", "client_secret": "…", "playlist": "Wecker" }
}
```

| Setting | Meaning |
| --- | --- |
| `wifi` | `ssid` and `password` of a 2.4 GHz network. Without it the clock scans once and shows the networks in range. |
| `sync_interval_h` | Hours between clock syncs over Wi-Fi (NTP), 1–168, default 24. |
| `alarms` | The alarms as the screen edits them: `id`, optional `name`, `enabled`, `hour`, `minute`, `weekdays` (Monday to Sunday, 0 or 1), `sound`, `volume` (1–100 %), `fade_in_s`. |
| `sound` | `"random"` (a different file from `sounds/` each day, the default), a path such as `"sounds/morning.flac"`, a TIDAL track `"tidal:<track id>"`, or `"tidal:random"`. A built-in tone plays if the sound cannot be played. |
| `ringing` | For all alarms: `snooze_min` (9), `max_snoozes` (3), `ring_timeout_min` (10; then the alarm stops for the day). |
| `display` | `night_pixels_percent`: pixel brightness in a dark room, 10–100, default 35. |
| `power` | `sleep_after_s`: the display sleeps after this long without a touch on the clock face, 10–3600, default 60. `sleep_when`: `"dark"` (in a dark room, and always on battery; the default), `"always"` or `"never"`. |
| `tidal` | Optional. `client_id` and `client_secret` of a TIDAL API client that may use the device login (Wakey ships none), and `playlist`, the playlist in your collection the alarms take their songs from (default "Wecker"). Log in on the sound page's TIDAL tab with the code it shows; the clock then stores its `session` here, so keep the card private. Requires a TIDAL subscription. |

Next to `config.json` the clock keeps `tidal/` (the playlist and downloaded tracks). Development builds can take Wi-Fi credentials and a test alarm from environment variables instead: see [firmware/README.md](firmware/README.md#sd-card).

## How it works

Rust on ESP-IDF 5.5 (std) with the Embassy executor on the ESP32-P4; the UI is [Slint](https://slint.dev) with a custom platform that renders line by line through the P4's pixel-processing accelerator (PPA) and composes scrolling lists with its 2D-DMA engine. The ESP32-C6 is the Wi-Fi radio through Espressif's esp_hosted, and switches to an AirDrop receiver (based on [ESP32Drop](https://github.com/s-iwaki-d/ESP32Drop)) on request. Everything that does not need the hardware (alarm state machine, configuration, audio decoding, the TIDAL protocol, scroll physics, the sky) is in [`wakey-core`](wakey-core/src/lib.rs) and tested on a PC.

| Document | Content |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | Design: layers, threads, rendering, audio, time, network, AirDrop |
| [docs/hardware.md](docs/hardware.md) | The Tab5: chips, pins, I2C devices, power |
| [docs/pitfalls.md](docs/pitfalls.md) | Problems met on this hardware, their causes and fixes |
| [docs/upgrades.md](docs/upgrades.md) | What to re-check when a dependency is updated |
| [docs/roadmap.md](docs/roadmap.md) | Known limitations and possible next steps |
| [firmware/README.md](firmware/README.md) | Building, flashing, the source layout |
| [firmware/c6/README.md](firmware/c6/README.md) | The ESP32-C6 images and how they are built |

| Path | Content |
| --- | --- |
| [`firmware/`](firmware) | The firmware for the ESP32-P4 (its own Cargo workspace and toolchain) |
| [`firmware/c6/`](firmware/c6) | The ESP32-C6's images (C, ESP-IDF): esp_hosted with Wakey's addition, the AirDrop receiver, a recovery tool |
| [`wakey-core/`](wakey-core) | Hardware-independent logic, tested on the host |
| [`tools/ui-preview/`](tools/ui-preview) | Renders every page on the host to PNG and checks the list layer's pixels |

## Limitations

- The UI is German; the time zone (Europe/Zurich) and the location for the sky (Zurich) are compiled in ([`firmware/src/clock.rs`](firmware/src/clock.rs), [`wakey-core/src/sky.rs`](wakey-core/src/sky.rs)).
- Only the ST7123 display variant of the Tab5 is supported.
- AirDrop and the TIDAL API are used without official support from Apple or TIDAL; an update on their side can break either. AirDrop is visible to senders set to receive from "Everyone".

More in [docs/roadmap.md](docs/roadmap.md).

## License

Wakey's own code is licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option. Contributions are accepted under the same terms (see [CONTRIBUTING.md](CONTRIBUTING.md)).

The firmware links [Slint](https://slint.dev), which is available under GPL-3.0, a royalty-free license that excludes embedded systems, or a commercial license. A firmware binary built from this repository therefore uses Slint under GPL-3.0, which makes the binary as a whole subject to the GPL-3.0; the source code in this repository keeps its permissive license. `wakey-core` does not depend on Slint.

Third-party parts: the Inter font ([OFL-1.1](firmware/ui/fonts/OFL.txt)); vendored and patched crates in [`firmware/vendor`](firmware/vendor/README.md) under their own licenses; the prebuilt ESP32-C6 images and their components in [firmware/c6/THIRD_PARTY_NOTICES.md](firmware/c6/THIRD_PARTY_NOTICES.md).

AirDrop and TIDAL are trademarks of their respective owners. This project is not affiliated with Apple or TIDAL.
