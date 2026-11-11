# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, several alarms that play a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi, alarm; missing settings written back); the alarm (state machine in `wakey-core`, state in NVS, ringing with fade-in and the built-in tone as fallback, ringing screen: tap to snooze up to 3 times, hold the corner button 1.5 s to stop, silenced for good after the ring timeout); several alarms with their own sound, volume and fade-in; settings pages in the design agreed on 2026-11-10 (alarm list with swipe-to-delete, alarm page with time wheels that click on the built-in speaker, sound page with "Zufällig", name keyboard); the built-in speaker (ES8388) for the wheel click and as the alarm speaker when no USB speaker is connected; the clock face's sky that follows the sun; brightness from the room's light with the camera as light meter (night mode with darkened pixels); `config.json` `display` and `power` sections; the display asleep (backlight off) in the dark after a minute without a touch, woken by a touch, an alarm or daylight.

**V1 (next):**

1. **Built-in speaker volume:** the alarm's volume curve is the Pebble's; check by ear how loud 60 % is on the built-in speaker (preview without the Pebble) and calibrate it separately if needed.
2. **Screens:** the sound list on the list layer (as the alarm list) once it can be long (TIDAL); check page switches while audio plays (~130 ms during a preview, measured before the internal RAM fix).
3. **Light sleep** (below): the chip asleep while the display is, measured step by step.
4. **Wi-Fi only when needed:** done (sessions every `sync_interval_h`, C6 off in between); open: each session leaks ~2.3 KiB internal RAM (`docs/pitfalls.md`).
5. FLAC playback (`claxon`; `sounds/` is scanned for WAV/AIFF today).

**V2: TIDAL.** Songs are chosen in the TIDAL app in a playlist ("Wecker"); the device syncs that playlist silently each time the sound page's TIDAL tab opens, shows it with covers (160×160 RGB565, decoded once) and "Zufällige Auswahl" on top, and downloads tracks as FLAC to `sounds/`. Before login the tab shows the login code. An alarm stores `"tidal:<track id>"` or `"tidal:random"` (already parsed; it plays the built-in tone until then). Groundwork: [`spikes/tidal-download`](../spikes/tidal-download) (host tool, working login and download). On the device: HTTP via `esp_idf_svc::http::client`, login code shown on the display, token in the config.

**Not planned:** radio, OTA, USB hub.

## Power concept

Two states, decided 2026-11-11:

| State | Display | Wi-Fi | USB-A 5 V | Wakes on |
| --- | --- | --- | --- | --- |
| On | on, brightness from the room's light | on | on (speaker ready) | – |
| Light sleep | backlight off | off | off | touch, timer (alarm, light check) |

Light sleep starts after `power.sleep_after_s` (60 s) without a touch on the clock page, when `power.sleep_when` allows it: `"dark"` (default) in a dark room (the light meter's night mode) or on battery; `"always"`; `"never"`. It ends on a touch (which only wakes), when an alarm rings, and when the condition ends (daylight on USB power).

**Done:** the display part: backlight off, nothing drawn, light meter running ([`app/power.rs`](../firmware/src/app/power.rs), `ui::power`). The panel is not put into sleep mode: the ST7123's touch stops scanning then.

**Next:** the chip itself in light sleep while the display sleeps (estimate 3–5 mA, touch wake 0.1–0.3 s): stop audio, uninstall USB host, `USB5V_EN` off → stop Wi-Fi, `WLAN_PWR_EN` off → `SPK_EN` off → light sleep, woken by `TP_INT` (GPIO23, if the touch controller signals it while polled rarely), by a timer before the next alarm (USB speaker enumeration needs a lead, to measure) and every minute for the light meter. Deep sleep (reboot on wake, `TP_INT` wired to GPIO1) is the fallback if light sleep with USB host, DSI and PSRAM proves unreliable.

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
- Light sleep current after each step of the sleep sequence; 100 wake cycles with USB host and DSI working afterwards.
- Time from waking to the USB speaker being ready again (the lead before an alarm).
