# Wakey

A bedside alarm clock on the M5Stack Tab5 (ESP32-P4, 5" 1280×720 touch panel) that plays sound files from its microSD card on a Creative Pebble V3 USB speaker. Written in Rust (std) on ESP-IDF with the Embassy executor. **Rust only:** no project-authored C/C++ and no C board support packages.

Working today: clock screen with the RTC time (Europe/Zurich), several alarms (each with time, weekdays, name, sound or a random one, volume and fade-in, set on the touch screen with clicking time wheels) ringing on the speaker with snooze, Wi-Fi and NTP through the on-board ESP32-C6, touch, and a list layer that scrolls at the panel's 66 fps. Next: the built-in speaker as fallback and power states ([roadmap](docs/roadmap.md)).

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
| [`tools/ui-preview/`](tools/ui-preview/src/main.rs) | host tool: renders the firmware's pages to PNG with sample data |
| [`spikes/tidal-download/`](spikes/tidal-download/README.md) | host tool: download a track from your TIDAL account (V2 groundwork) |

## Quick start

```sh
cd firmware
cargo run --release --bin wakey      # first build ~10 min (ESP-IDF)
cd ../wakey-core && cargo test       # host tests
```

Put `config.json` with `{ "wifi": { "ssid": "…", "password": "…" } }` and sound files on the SD card (see [Configure the clock](#configure-the-clock)). Build details in [firmware/README.md](firmware/README.md).

Each directory with a `Cargo.toml` is its own workspace with its own toolchain: the firmware uses a pinned nightly and an ESP target, the others the host toolchain. Run Cargo from inside them.

## Configure the clock

The clock reads `config.json` from the root of the microSD card (FAT) at boot. Alarms are set on the touch screen, which writes them back into the file. Settings missing from the file are added with their defaults, so after the first boot the file lists everything. A missing file starts with the defaults; a file that cannot be read (a JSON error) stops the clock on an error page with the parser's message and is not overwritten.

```json
{
  "wifi": { "ssid": "MeinNetz", "password": "geheim" },
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
| `sync_interval_h` | Hours between clock syncs over Wi-Fi (NTP), 1–168, default 24. Wi-Fi is off in between. |
| `alarms` | The alarms, as the settings pages edit them: `id`, optional `name`, `enabled`, `hour`, `minute`, `weekdays` (Monday to Sunday, 0/1), `sound`, `volume` (1–100 %), `fade_in_s`. A file with the old single `alarm` section is migrated. |
| `sound` | `"beep"` (the built-in tone), `"random"` (a different file from `sounds/` each day), a path such as `"sounds/morgen.flac"`, or a TIDAL track: `"tidal:<track id>"`, `"tidal:random"`. |
| `ringing` | For all alarms: `snooze_min` (9), `max_snoozes` (3), `ring_timeout_min` (10; then the alarm stops for the day). |
| `display` | `night_pixels_percent`: pixel brightness in a dark room, 10–100, default 35. |
| `power` | `sleep_after_s`: the display sleeps after this long without a touch on the clock page, 10–3600, default 60. `sleep_when`: `"dark"` (default: in a dark room, and always on battery), `"always"` or `"never"`. |
| `tidal` | The TIDAL app credentials `client_id` and `client_secret` (from `cargo run -- clock-config` in [`spikes/tidal-download`](spikes/tidal-download/README.md)) and `playlist`, the playlist in your TIDAL collection the alarms take their songs from (default "Wecker"). After the login on the sound page's TIDAL tab the clock adds `session` here: it gives access to the account, so keep the card private. |

On the card next to `config.json`:

- `sounds/`: sound files for the alarms (WAV, AIFF/AIFC or FLAC; PCM 8–32 bit, mono or stereo, any sample rate), listed by file name (`_` shown as a space).
- `tidal/`: written by the clock: the playlist and the downloaded tracks.

For development, the build can set Wi-Fi credentials and a test alarm instead of the file's (see [firmware/README.md](firmware/README.md#sd-card)).
