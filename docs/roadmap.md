# Roadmap

What the clock is meant to become, and the decisions already taken for the parts not built yet. This file is the master plan: change the plan here. The current state is in [architecture.md](architecture.md).

## Product

A bedside alarm clock: large clock face, one alarm that plays a sound file from the microSD card on the Creative Pebble V3 (one USB cable for power and audio, gain switch "L"), dims and sleeps at night, wakes instantly on touch.

**Done:** clock screen with RTC time, Europe/Zurich, NTP sync over Wi-Fi; touch; USB audio with file playback (WAV/AIFF, any rate → 48 kHz); fast scrolling lists; `config.json` (Wi-Fi).

**V1 (next):**

1. **Alarm logic** in `wakey-core` (host-tested): one alarm time with a weekday mask, snooze (minutes, maximum count), ring timeout, "done today". Pure state machine: `next_trigger(cfg, now_local)` and `step(state, cfg, now, input) → (state, actions)`; inputs Tick / Stop / Snooze; actions start ring, stop ring, arm timer, mark done.
2. **Screens:** alarm settings (time, weekdays, sound, volume), ringing (stop, snooze), sound list (the list layer).
3. **Ringing:** `UsbAudio::set_streaming(true)`, player on the chosen file, fade-in, capped volume. If the speaker does not connect within a timeout or the file is missing: embedded fallback tone (internal ES8388 speaker as a later option).
4. **Config:** the remaining `config.json` fields (below), defaults filled in and written back; snooze count and "done today" in NVS so a reboot does not ring twice.
5. **Power states** (below), measured step by step.
6. **Wi-Fi only when needed:** sync at most every 24 h while the display is on, then C6 unpowered (`WLAN_PWR_EN` off).
7. FLAC playback (`claxon`), sound list scan of `sounds/`.

**V2: TIDAL.** Songs are chosen in the TIDAL app in a playlist ("Wecker"); the device syncs that playlist, shows it with covers (160×160 RGB565, decoded once) and downloads tracks as FLAC to `sounds/`. Groundwork: [`spikes/tidal-download`](../spikes/tidal-download) (host tool, working login and download). On the device: HTTP via `esp_idf_svc::http::client`, login code shown on the display, token in the config.

**Not planned:** several alarms, radio, OTA, USB hub.

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

Only `wifi` is read today. Missing fields get defaults and are written back.

```json
{
  "wifi": { "ssid": "MeinNetz", "password": "geheim" },
  "sync_interval_h": 24,
  "alarm": {
    "enabled": true, "hour": 6, "minute": 30,
    "weekdays": [1, 1, 1, 1, 1, 0, 0],
    "sound": "sounds/morgen.flac",
    "volume": 60, "fade_in_s": 10,
    "snooze_min": 9, "max_snoozes": 3, "ring_timeout_min": 10
  },
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
