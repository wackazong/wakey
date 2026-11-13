# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, several alarms that play a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF/FLAC, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi, alarm; missing settings written back); the alarm (state machine in `wakey-core`, state in NVS, ringing with fade-in and the built-in tone as fallback, ringing screen: tap to snooze up to 3 times, hold the corner button 1.5 s to stop, silenced for good after the ring timeout); several alarms with their own sound, volume and fade-in; settings pages in the design agreed on 2026-11-10 (alarm list with swipe-to-delete, alarm page with time wheels that click on the built-in speaker, sound page with "Zufällig", name keyboard); the built-in speaker (ES8388) for the wheel click and as the alarm speaker when no USB speaker is connected; the clock face's sky that follows the sun; brightness from the room's light with the camera as light meter (night mode with darkened pixels); `config.json` `display` and `power` sections; the display asleep (backlight off) in the dark after a minute without a touch, woken by a touch, an alarm or daylight, with the panel at 33 Hz and USB-A off (power states and measurements: [hardware.md](hardware.md#power)).

## Next steps

- move the tidal track list to the list layer
- Show just the TIDAL song name on the alarm settings screen
- Ablum covers
- Show failed downloads, retry after restart
- open: each C6 session leaks ~2.3 KiB internal RAM (`docs/pitfalls.md`).
- play downloading songs as soon as possible, never play the built-in tone as a replacement in preview
- make the first sync after opening the tab faster (Wi-Fi up when the alarm page opens, one connection for the requests, no playlist search once known)

## `config.json`

All sections are read (see [architecture.md](architecture.md#alarm) and `firmware/README.md`); `sound` is `"beep"`, `"random"`, a file path or (V2) `"tidal:…"`. Missing fields get defaults and are written back.

```json
{
  "wifi": { "ssid": "MeinNetz", "password": "geheim" },
  "alarms": [
    { "id": 1, "name": "Arbeit", "enabled": true, "hour": 6, "minute": 30,
      "weekdays": [1, 1, 1, 1, 1, 0, 0], "sound": "random",
      "volume": 60, "fade_in_s": 10 }
  ],
  "ringing": { "snooze_min": 9, "max_snoozes": 3, "ring_timeout_min": 10 },
  "display": { "night_pixels_percent": 35 },
  "power": { "sleep_after_s": 60, "sleep_when": "dark" }
}
```

`weekdays` counts Monday to Sunday. SD card layout: `config.json`, `sounds/*.wav|aiff|flac`, later `covers/`.

## Development tools (idea)

A `devtools` feature for a coding agent on the Mac over the USB-C cable: screenshots on demand (QOI-compressed, 0.1–0.4 s), injected touches through the same path as the touch task, state as JSON, a fake clock for alarm tests, stay-awake; plus a Mac-side CLI and a Slint simulator for the screens with fake state.