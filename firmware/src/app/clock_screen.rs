//! The clock screen's view model: once a second, aligned to the clock's
//! second, the alarm is advanced and the time, date, alarm, Wi-Fi and
//! speaker state go into the `ClockScreen` properties. Only fields that
//! changed are set, so Slint repaints only those (a seconds tick dirties
//! ~110 panel rows). The ringing controls are on this screen too.

use super::alarm::{AlarmClock, View};
use super::screens::{AppWindow, Clock};
use crate::audio::uac::UsbAudio;
use crate::clock;
use crate::net::{Failure, Status, Wifi};
use slint::{ComponentHandle, SharedString, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use wakey_core::alarm::Input;
use wakey_core::datetime::{DateTime, WEEKDAYS_DE};

/// Keeps the screen updating while alive.
pub struct Ticker {
    _align: Timer,
    _every_second: Rc<Timer>,
}

/// Fills the screen now and then every second, and connects the ringing
/// controls to `alarm`. When the alarm rings, the clock page is shown.
pub fn start(
    window: &AppWindow,
    speaker: UsbAudio,
    wifi: Wifi,
    alarm: Rc<RefCell<AlarmClock>>,
) -> Ticker {
    let weak = window.as_weak();
    let refresh = {
        let alarm = alarm.clone();
        Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                update(&window, &speaker, &wifi, &alarm.borrow());
            }
        })
    };
    let on_input = |input: Input| {
        let alarm = alarm.clone();
        let refresh = refresh.clone();
        move || {
            alarm.borrow_mut().input(input);
            refresh();
        }
    };
    let globals = window.global::<Clock>();
    globals.on_snooze(on_input(Input::Snooze));
    globals.on_stop(on_input(Input::Stop));
    let tick = move || {
        alarm.borrow_mut().tick();
        refresh();
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

fn update(window: &AppWindow, speaker: &UsbAudio, wifi: &Wifi, alarm: &AlarmClock) {
    let screen = window.global::<Clock>();
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

    let view = alarm.view();
    set_text(screen.get_alarm(), alarm_line(&view, &t), |v| {
        screen.set_alarm(v)
    });
    if matches!(view, View::Ringing { .. }) && window.get_page() != super::PAGE_CLOCK {
        window.set_page(super::PAGE_CLOCK);
    }
    let (phase, snoozes_left) = match view {
        View::Idle { .. } => (0, 0),
        View::Ringing { snoozes_left } => (1, snoozes_left),
        View::Snoozed { snoozes_left, .. } => (2, snoozes_left),
    };
    if screen.get_alarm_phase() != phase {
        screen.set_alarm_phase(phase);
    }
    if screen.get_snoozes_left() != i32::from(snoozes_left) {
        screen.set_snoozes_left(snoozes_left.into());
    }

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

/// The bottom-left line: the next alarm, or the snooze.
fn alarm_line(view: &View, now: &DateTime) -> String {
    match view {
        View::Idle { next: None } => "Kein Wecker gestellt".into(),
        View::Idle { next: Some(t) } => {
            let day = if t.date() == now.date() {
                "heute"
            } else if t.date() == now.add_days(1).date() {
                "morgen"
            } else {
                &WEEKDAYS_DE[usize::from(t.weekday) % 7][..2]
            };
            format!("Wecker {day} {:02}:{:02}", t.hour, t.minute)
        }
        View::Ringing { .. } => "Wecker klingelt".into(),
        View::Snoozed {
            until,
            snoozes_left: 0,
        } => format!(
            "Schlummern bis {:02}:{:02} · letztes Mal",
            until.hour, until.minute
        ),
        View::Snoozed {
            until,
            snoozes_left,
        } => format!(
            "Schlummern bis {:02}:{:02} · noch {snoozes_left}×",
            until.hour, until.minute
        ),
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
