# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, several alarms that play a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi, alarm; missing settings written back); the alarm (state machine in `wakey-core`, state in NVS, ringing with fade-in and the built-in tone as fallback, ringing screen: tap to snooze up to 3 times, hold the corner button 1.5 s to stop, silenced for good after the ring timeout); several alarms with their own sound, volume and fade-in; settings pages in the design agreed on 2026-11-10 (alarm list with swipe-to-delete, alarm page with time wheels that click on the built-in speaker, sound page with "Zufällig", name keyboard); the built-in speaker (ES8388) for the wheel click.

**V1 (next):**

1. **No speaker:** if the Pebble is not connected when the alarm rings, nothing is audible today; the built-in ES8388 speaker (already used for the wheel click) as the fallback.
2. **Screens:** the alarm list on the list layer for smooth scrolling (Slint redraws it at ~24 fps now; the layer copies rows by DMA), and the sound list once it can be long (TIDAL); check page switches while audio plays (~130 ms during a preview, measured before the internal RAM fix).
3. **Config:** the remaining `config.json` fields (below).
4. **Power states** (below), measured step by step.
5. **Wi-Fi only when needed:** sync at most every 24 h while the display is on, then C6 unpowered (`WLAN_PWR_EN` off).
6. FLAC playback (`claxon`; `sounds/` is scanned for WAV/AIFF today).

**V2: TIDAL.** Songs are chosen in the TIDAL app in a playlist ("Wecker"); the device syncs that playlist silently each time the sound page's TIDAL tab opens, shows it with covers (160×160 RGB565, decoded once) and "Zufällige Auswahl" on top, and downloads tracks as FLAC to `sounds/`. Before login the tab shows the login code. An alarm stores `"tidal:<track id>"` or `"tidal:random"` (already parsed; it plays the built-in tone until then). Groundwork: [`spikes/tidal-download`](../spikes/tidal-download) (host tool, working login and download). On the device: HTTP via `esp_idf_svc::http::client`, login code shown on the display, token in the config.

**Not planned:** radio, OTA, USB hub.

## Power concept

| State | Display | Wi-Fi | USB-A 5 V | Wakes on |
| --- | --- | --- | --- | --- |
| Active | on | only during a sync | on (speaker ready) | – |
| Dimmed (after 30 s) | 10 % | off | off | touch |
| Sleep (after 60 s) | off, panel sleep-in | off | off | touch (GPIO23), timer |
| Ringing | on | off | on, streaming | – |
| Power-off (battery only, next alarm > 12 h) | unpowered | off | off | RTC alarm, button, IMU tap |

Sleep is **light sleep** with peripherals switched off first (estimate 3–5 mA, touch wake 0.1–0.3 s). Deep sleep needs a wire from `TP_INT` (R92) to GPIO1 (2×5 header pin 8) for touch wake and reboots on wake; it is the fallback if light sleep with USB host, DSI and PSRAM proves unreliable.

Sleep sequence (reverse on wake): stop audio, uninstall USB host, `USB5V_EN` off → stop Wi-Fi, `WLAN_PWR_EN` off → `SPK_EN` off, codecs powered down → backlight off, panel sleep-in (DCS 0x28, 0x10), DSI off → INA226 shutdown → touch in a low-power mode with its interrupt active → sleep until the next alarm (light sleep: GPIO23 + timer).

From power-off: the RTC alarm fires `rtc_lead_s` (≈ 15 s, to be measured) before the alarm, the PMS150 powers the board, the firmware reads and clears the alarm flag at boot (no ringing loop), brings up the speaker and rings at the exact time. Before powering off: RTC alarm set, BMI270 tap wake armed (needs Bosch's 8 kB config blob, see M5Unified), then `PWROFF_PULSE`.

Estimates (5 V input, factor-2 uncertainty; only measurements count): active 400–500 mA, dimmed ≈ 200 mA, light sleep 3–5 mA, deep sleep 2–3 mA, power-off ≈ 0.3 mA; the CH9102F adds 1–3 mA when powered via USB-C.

## `config.json` (planned fields)

`wifi`, `alarms` and `ringing` are read today (see [architecture.md](architecture.md#alarm)); `sound` is `"beep"`, `"random"`, a file path or (V2) `"tidal:…"`. Missing fields get defaults and are written back.

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
  "display": { "dim_after_s": 30, "dim_percent": 10 },
  "power": {
    "sleep_after_s": 60, "night_mode": "light",
    "poweroff_on_battery": true, "min_poweroff_gap_min": 720,
    "imu_tap_wake": true, "rtc_lead_s": 15
  }
}
```

`weekdays` counts Monday to Sunday. SD card layout: `config.json`, `sounds/*.wav|aiff|flac`, later `covers/`.

## Development tools (idea)

A `devtools` feature for a coding agent on the Mac over the USB-C cable: screenshots on demand (QOI-compressed, 0.1–0.4 s), injected touches through the same path as the touch task, state as JSON, a fake clock for alarm tests, stay-awake; plus a Mac-side CLI and a Slint simulator for the screens with fake state.

## Open measurements

- Pebble current at gain "L" and the planned volume cap (target ≤ 0.5 A); `SYS_USB5V` under load (≥ 4.75 V, no reset).
- Light sleep current after each step of the sleep sequence; 100 wake cycles with USB host and DSI working afterwards.
- Power-off current on battery; RTC alarm restart; boot to first picture and to the speaker being ready (→ `rtc_lead_s`).
- BMI270 tap wake without false triggers overnight.
