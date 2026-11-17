# Roadmap

Known limitations of the firmware as it is, and possible next steps. The design is in [architecture.md](architecture.md); workarounds that can go with a dependency update are in [upgrades.md](upgrades.md).

## Known limitations

- **Hardware variants:** only the ST7123 panel variant is tested. The ST7121 shares its touch controller and has an init table, but has not run on a device; the ILI9881C variant is detected by its GT911 touch controller, which is not implemented ([hardware.md](hardware.md)).
- **Locale:** the screen speaks English or German (`language` in `config.json`; one file per language in `wakey-core/lang/`); the 24-hour time is fixed. The time zone is a POSIX rule in `config.json`, not a zone name (no zone database in the C library).
- **Power:** the chip never sleeps; only the display does (a single tap could not wake it from light sleep, see [pitfalls.md](pitfalls.md#touch-and-i2c)). The RTC's alarm, which could switch the board on from power-off, is not used. Running from the battery is detected (display sleep) but otherwise untested; the Pebble's current at its volume cap is not measured ([hardware.md](hardware.md#open-power-measurements)).
- **USB speaker:** switching USB-A off makes the Pebble click, as ESP-IDF 5.5.5's USB host has no suspend. Only UAC 1.0 speakers that take 48 kHz / 16 bit / stereo are supported.
- **Network:** 2.4 GHz Wi-Fi only (the C6); the station is configured for WPA2-Personal, or open without a password.
- **AirDrop:** TIDAL downloads pause while the Files tab is shown (the C6 runs either Wi-Fi or the receiver). The receiver uses 2.4 GHz channel 6 only, 14–136 kB/s, and accepts every sender while the Files tab is shown: its TLS key is public ([firmware/c6/airdrop](../firmware/c6/airdrop/README.md)). Several files at once are untested from an iPhone. A transfer is limited to 256 MiB. The protocols are Apple's, reverse-engineered: an iOS update can break them.
- **TIDAL:** needs a TIDAL API client that may use the device login (Wakey ships none). A track whose download failed is not tried again until the next start, a network failure included. The API is used without official support.
- **Audio:** AAC sounds keep the encoder delay and padding, so a repeated AAC sound has a gap of ~0.1 s.
- **Toolchain:** the firmware builds only on one pinned nightly until rust-lang/libc#5662 is in a nightly ([upgrades.md](upgrades.md#rust-toolchain)).

## Possible next steps

- **Light sleep with double-tap wake:** the ST7123's Smart Wake Up reports a double tap from light sleep; with the display link rebuilt after waking this would save an estimated 0.1–0.2 W more than display sleep, at the cost of a single tap no longer waking the clock.
- **Other panel variants:** the GT911 touch controller for the ILI9881C variant, and a test on an ST7121 unit.
- **Upstreaming:** the vendored Slint, esp-idf-svc and esp_lcd changes ([vendor/README.md](../firmware/vendor/README.md)), so the copies can go.
- **Development tools:** a `devtools` feature for working on the clock from a computer over the USB-C cable: screenshots on demand (QOI-compressed), touches injected through the same queue as the touch thread's, state as JSON, a fake clock for alarm tests, stay-awake; plus a host-side CLI.
