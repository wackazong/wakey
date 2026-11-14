# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, several alarms that play a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF/FLAC, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi, alarm; missing settings written back); the alarm (state machine in `wakey-core`, state in NVS, ringing with fade-in and the built-in tone as fallback, ringing screen: tap to snooze up to 3 times, hold the corner button 1.5 s to stop, silenced for good after the ring timeout); several alarms with their own sound, volume and fade-in; settings pages in the design agreed on 2026-11-10 (alarm list with swipe-to-delete, alarm page with time wheels that click on the built-in speaker, sound page with "Zufällig", name keyboard); the built-in speaker (ES8388) for the wheel click and as the alarm speaker when no USB speaker is connected; the clock face's sky that follows the sun; brightness from the room's light with the camera as light meter (night mode with darkened pixels); `config.json` `display` and `power` sections; the display asleep (backlight off) in the dark after a minute without a touch, woken by a touch, an alarm or daylight, with the panel at 33 Hz and USB-A off (power states and measurements: [hardware.md](hardware.md#power)).

## Next steps

**Sound files by AirDrop (spike works, not decided).** An iPhone sends a sound file to "Wakey" from its share sheet, with nothing installed and no shared Wi-Fi. [`spikes/airdrop-c6`](../spikes/airdrop-c6/README.md) runs ESP32Drop (an independent re-implementation of AWDL and the AirDrop receiver, 0BSD) on the C6 instead of esp_hosted: a 416 KB .m4a and a 4 MB file arrived intact at 20–136 kB/s (2.4 GHz channel 6 only, and the iPhone decides how much of its time it spends there; a 3 MB song takes 25 s to 2.5 minutes). For the clock: one C6 firmware with both roles (esp_hosted for Wi-Fi, AirDrop while a "Receive" screen is open, never both), the files streamed to the P4 over esp_hosted's custom-data channel into `.part` files, AAC/MP3 decoding (iPhones send .m4a). Open: the C6 part is C (an exception to "Rust only", like esp_hosted itself), and it relies on reverse-engineered Apple protocols that an iOS update can break. A QR code to a web page on the clock was the alternative considered (works for any phone and laptop).

## Development tools (idea)

A `devtools` feature for a coding agent on the Mac over the USB-C cable: screenshots on demand (QOI-compressed, 0.1–0.4 s), injected touches through the same path as the touch task, state as JSON, a fake clock for alarm tests, stay-awake; plus a Mac-side CLI and a Slint simulator for the screens with fake state.