# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, several alarms that play a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF/FLAC, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi, alarm; missing settings written back); the alarm (state machine in `wakey-core`, state in NVS, ringing with fade-in and the built-in tone as fallback, ringing screen: tap to snooze up to 3 times, hold the corner button 1.5 s to stop, silenced for good after the ring timeout); several alarms with their own sound, volume and fade-in; settings pages in the design agreed on 2026-11-10 (alarm list with swipe-to-delete, alarm page with time wheels that click on the built-in speaker, sound page with "Zufällig", name keyboard); the built-in speaker (ES8388) for the wheel click and as the alarm speaker when no USB speaker is connected; the clock face's sky that follows the sun; brightness from the room's light with the camera as light meter (night mode with darkened pixels); `config.json` `display` and `power` sections; the display asleep (backlight off) in the dark after a minute without a touch, woken by a touch, an alarm or daylight, with the panel at 33 Hz and USB-A off (see the power concept).

**V1 (next):**

1. **Built-in speaker volume:** the alarm's volume curve is the Pebble's; check by ear how loud 60 % is on the built-in speaker (preview without the Pebble) and calibrate it separately if needed.
2. **Screens:** the sound list on the list layer (as the alarm list) once it can be long (TIDAL); check page switches while audio plays (~130 ms during a preview, measured before the internal RAM fix).
3. **Power, the rest** (below): fewer wake-ups (touch by `TP_INT` instead of 10 ms polls, the USB client's 10 ms pump; not planned for now); the periodic ~20–35 mA dip seen with the meter.
4. **Wi-Fi only when needed:** done (sessions every `sync_interval_h`, C6 off in between); open: each session leaks ~2.3 KiB internal RAM (`docs/pitfalls.md`).

**V2: TIDAL.** Songs are chosen in the TIDAL app in a playlist ("Wecker"); the device syncs that playlist silently each time the sound page's TIDAL tab opens, shows it with covers (160×160 RGB565, decoded once) and "Zufällige Auswahl" on top, and downloads tracks as FLAC to `sounds/`. Before login the tab shows the login code. An alarm stores `"tidal:<track id>"` or `"tidal:random"` (already parsed; it plays the built-in tone until then). Groundwork: [`spikes/tidal-download`](../spikes/tidal-download) (host tool, working login and download). On the device: HTTP via `esp_idf_svc::http::client`, login code shown on the display, the session in `config.json`'s `tidal` section (with the app credentials, pasted in from `cargo run -- clock-config` in the spike).

Steps: 1. FLAC playback (done). 2. The protocol in `wakey_core::tidal` (done, host-tested): device code login and refresh, playlists and their tracks, cover URLs, the stream manifest (BTS mirrors, DASH segments; lossless arrives as DASH, FLAC in fragmented MP4, unpacked to `.flac` while downloading), the sync plan (`tidal/playlist.json`, `tidal/<id>.flac`, `tidal/covers/<id>.rgb`). 3. HTTPS on the device and the login (done: QR code of the device code link on the sound page's TIDAL tab, session saved to `config.json`). 4. Playlist sync (done: the tab lists „Zufällig“ and the playlist's tracks, kept in `tidal/playlist.json`). 5. Downloads and `tidal:` alarms (done: the tracks the alarms use, all for „Zufällig“; progress bar in the row; ~1 MiB/s). 6. Covers (JPEG → RGB565, 160 × 160). 7. Open: a failed download says so in its row (now „wird geladen“ stays) and is not retried until the next start; the first sync after opening the tab is faster (Wi-Fi up when the alarm page opens, one connection for the requests, no playlist search once known); the TIDAL tracks in the list view; „Probehören“ of a track that is still downloading plays it as soon as enough has arrived instead of the built-in tone; downloads faster still (profile core 0 over a whole download, feature `profile-download`).

**Not planned:** radio, OTA, USB hub.

## Power concept

Two states, decided 2026-11-11; the second is a display sleep, not chip light sleep (decided 2026-11-12, see below):

| State | Display | Wi-Fi | USB-A 5 V | Wakes on |
| --- | --- | --- | --- | --- |
| On | on, brightness from the room's light | only for a sync session | only on the alarm pages and from 60 s before something rings | – |
| Display sleep | backlight off, panel at 33 Hz | as above | as above | any touch, alarm, daylight |

Display sleep starts after `power.sleep_after_s` (60 s) without a touch on the clock page, when `power.sleep_when` allows it: `"dark"` (default) in a dark room (the light meter's night mode) or on battery; `"always"`; `"never"`. It ends on a touch (which only wakes), when an alarm rings, and when the condition ends (daylight on USB power).

**Done:** backlight off, nothing drawn, light meter running ([`app/power.rs`](../firmware/src/app/power.rs), `ui::power`); the panel streams at 33 Hz instead of 66 Hz (the display link is rebuilt for it, 0.45 s each way); USB-A is off except on the alarm pages and from 60 s before something rings. The panel is not put into sleep mode: the ST7123's touch stops scanning then. Measured at the USB-C input (5 V, Pebble switched on): screen on 237–242 mA, display sleep before these changes 237 mA, USB-A off −80 mA, 33 Hz −21 mA; ~138 mA asleep; the built-in speaker's codec and I2S off while silent: 132 mA (dipping to ~99 mA now and then).

**Not chip light sleep:** a single tap must wake the clock, and in light sleep it cannot (spike `chip_sleep`, 2026-11-12): the ST7123 senses touches only while the P4 streams video, which light sleep stops; in its "Smart Wake Up" mode it keeps sensing but reports only gestures (double tap, long press), never a single tap. A double-tap wake from light sleep works (with `gpio_sleep_sel_dis(23)` and the whole display link rebuilt after waking) and would save roughly another 0.1–0.2 W (estimated).

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

## Open measurements

- Pebble current at gain "L" and the planned volume cap (target ≤ 0.5 A); `SYS_USB5V` under load (≥ 4.75 V, no reset).
- The periodic ~20–35 mA dip in display sleep.
- Time from waking to the USB speaker being ready again (the lead before an alarm).
