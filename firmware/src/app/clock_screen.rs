//! The clock screen's view model: once a second, aligned to the clock's
//! second, the time, date, Wi-Fi and speaker state go into the
//! `ClockScreen` properties. Only fields that changed are set, so Slint
//! repaints only those (a seconds tick dirties ~110 panel rows).

use super::screens::ClockScreen;
use crate::audio::uac::UsbAudio;
use crate::clock;
use crate::net::{Failure, Status, Wifi};
use slint::{ComponentHandle, SharedString, Timer, TimerMode};
use std::rc::Rc;

/// Keeps the screen updating while alive.
pub struct Ticker {
    _align: Timer,
    _every_second: Rc<Timer>,
}

/// Fills the screen now and then every second.
pub fn start(screen: &ClockScreen, speaker: UsbAudio, wifi: Wifi) -> Ticker {
    let weak = screen.as_weak();
    let tick = move || {
        if let Some(screen) = weak.upgrade() {
            update(&screen, &speaker, &wifi);
        }
    };
    tick();
    // A one-shot timer to the next full second, then a repeating one.
    let every_second = Rc::new(Timer::default());
    let align = Timer::default();
    let repeat = every_second.clone();
    align.start(
        TimerMode::SingleShot,
        clock::until_next_second() + core::time::Duration::from_millis(2),
        move || {
            tick();
            repeat.start(
                TimerMode::Repeated,
                core::time::Duration::from_secs(1),
                tick.clone(),
            );
        },
    );
    Ticker {
        _align: align,
        _every_second: every_second,
    }
}

fn update(screen: &ClockScreen, speaker: &UsbAudio, wifi: &Wifi) {
    fn set_text(current: SharedString, new: String, set: impl Fn(SharedString)) {
        if current.as_str() != new {
            set(new.into());
        }
    }
    let t = clock::now();
    set_text(screen.get_hours(), format!("{:02}", t.hour), |v| {
        screen.set_hours(v)
    });
    set_text(screen.get_minutes(), format!("{:02}", t.minute), |v| {
        screen.set_minutes(v)
    });
    set_text(screen.get_seconds(), format!("{:02}", t.second), |v| {
        screen.set_seconds(v)
    });
    set_text(screen.get_date(), t.date_de(), |v| screen.set_date(v));
    set_text(screen.get_alarm(), "Kein Wecker gestellt".into(), |v| {
        screen.set_alarm(v)
    });

    if screen.get_speaker_connected() != speaker.connected() {
        screen.set_speaker_connected(speaker.connected());
    }

    let line = wifi_line(&wifi.status());
    set_text(screen.get_wifi_text(), line.text, |v| {
        screen.set_wifi_text(v)
    });
    if screen.get_wifi_state() != line.state as i32 {
        screen.set_wifi_state(line.state as i32);
    }
    if screen.get_wifi_bars() != line.bars {
        screen.set_wifi_bars(line.bars);
    }
}

/// Colour of the Wi-Fi line (`wifi-state` in `wakey.slint`).
#[derive(Clone, Copy)]
enum WifiState {
    Off = 0,
    Busy = 1,
    Connected = 2,
    Error = 3,
}

struct WifiLine {
    text: String,
    state: WifiState,
    /// Signal bars, 0–4.
    bars: i32,
}

fn wifi_line(s: &Status) -> WifiLine {
    let line = |text: String, state, bars| WifiLine { text, state, bars };
    match s {
        Status::Starting => line("WLAN startet…".into(), WifiState::Busy, 0),
        Status::NotConfigured { networks: Some(n) } => line(
            format!("WLAN nicht eingerichtet · {n} Netze in Reichweite"),
            WifiState::Off,
            0,
        ),
        Status::NotConfigured { networks: None } => {
            line("WLAN nicht eingerichtet".into(), WifiState::Off, 0)
        }
        Status::Connecting { ssid } => line(format!("Verbinde mit {ssid}…"), WifiState::Busy, 0),
        Status::Connected {
            ssid,
            ip,
            rssi,
            time_synced,
        } => {
            let bars = match rssi {
                Some(r) if *r >= -55 => 4,
                Some(r) if *r >= -65 => 3,
                Some(r) if *r >= -75 => 2,
                Some(_) => 1,
                None => 4,
            };
            let ntp = if *time_synced { " · Zeit via NTP" } else { "" };
            line(format!("{ssid} · {ip}{ntp}"), WifiState::Connected, bars)
        }
        Status::Failed { ssid, error } => {
            let reason = match error {
                Failure::Driver => "WLAN-Modul startet nicht".into(),
                Failure::Timeout => "Zeitüberschreitung".into(),
                Failure::Other(code) => format!("Fehler {code}"),
            };
            line(format!("{ssid}: {reason}"), WifiState::Error, 0)
        }
    }
}
