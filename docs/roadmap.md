# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, several alarms that play a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF/FLAC, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi, alarm; missing settings written back); the alarm (state machine in `wakey-core`, state in NVS, ringing with fade-in and the built-in tone as fallback, ringing screen: tap to snooze up to 3 times, hold the corner button 1.5 s to stop, silenced for good after the ring timeout); several alarms with their own sound, volume and fade-in; settings pages in the design agreed on 2026-11-10 (alarm list with swipe-to-delete, alarm page with time wheels that click on the built-in speaker, sound page with "Zufällig", name keyboard); the built-in speaker (ES8388) for the wheel click and as the alarm speaker when no USB speaker is connected; the clock face's sky that follows the sun; brightness from the room's light with the camera as light meter (night mode with darkened pixels); `config.json` `display` and `power` sections; the display asleep (backlight off) in the dark after a minute without a touch, woken by a touch, an alarm or daylight, with the panel at 33 Hz and USB-A off (power states and measurements: [hardware.md](hardware.md#power)).

## Next steps

**Sound files by AirDrop (in the firmware since 2026-11-14; how it works: [architecture.md](architecture.md#airdrop)).** An iPhone sends a sound file to "Wakey" from its share sheet, with nothing installed and no shared Wi-Fi. The C6 receiver and its measurements: [`firmware/c6/airdrop`](../firmware/c6/airdrop/README.md) (ESP32Drop on the C6; 20–136 kB/s on 2.4 GHz channel 6). Decisions:

- **When:** AirDrop is on while the sound page's Files tab is shown; Wi-Fi is off meanwhile. The C6 runs one or the other (switching restarts it, a few seconds). Wi-Fi as today: from opening the alarm list while the settings pages are open (TIDAL), on the clock face only for downloads and the NTP sync.
- **The list:** the Files tab ends with "AirDrop bereit für neue Klänge" (also when empty); a file being received is a row with a progress bar, as a TIDAL download.
- **Formats:** iPhones send .m4a: AAC and ALAC (and MP3) are added to playback (`symphonia` in `wakey-core`).
- **C6:** two images in the C6's two app slots: esp_hosted (our build, `firmware/c6/`) and the AirDrop receiver (`c6/airdrop`, C, ESP32Drop with the spike's patches). esp_hosted gets one addition: a peer-data message (`esp_hosted_send_custom_data`) that makes the other slot the next boot and restarts, if it holds the receiver. The receiver makes esp_hosted the next boot again at its start, so a power cycle of the C6 ends AirDrop. The receiver is installed into the slot esp_hosted does not run from by esp_hosted's OTA (without activation), again after an esp_hosted update.
- **Data to the P4:** while the receiver runs there is no esp_hosted link; it sends frames (file begin, data, end, verdict, status, log; CRC each) over C6 IO2 → P4 GPIO14 as UART (3 Mbaud; one way, no flow control). The P4 writes `sounds/<name>.part` and renames it after the verdict; AppleDouble `._` files and unplayable types are dropped.
- **Open:** TIDAL downloads pause while the Files tab is shown (the C6 does one thing at a time); several files at once and a Mac as sender are untested.
- **Open:** the C6 part is C (an exception to "Rust only", like esp_hosted itself); the protocols are Apple's, reverse-engineered, and an iOS update can break them.

## Development tools (idea)

A `devtools` feature for a coding agent on the Mac over the USB-C cable: screenshots on demand (QOI-compressed, 0.1–0.4 s), injected touches through the same path as the touch task, state as JSON, a fake clock for alarm tests, stay-awake; plus a Mac-side CLI and a Slint simulator for the screens with fake state.