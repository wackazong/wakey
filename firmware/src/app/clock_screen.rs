//! The clock screen's view model: once a second, aligned to the clock's
//! second, the alarm is advanced and the time, date, alarm and the
//! missing-speaker and missing-network flags go into the `Clock`
//! properties. Only fields that changed are set, so Slint repaints only
//! those (a seconds tick dirties ~110 panel rows). The ringing controls are
//! on this screen too.

use super::alarm::{AlarmClock, View};
use super::screens::{AppWindow, Clock};
use super::{PAGE_CLOCK, show_page};
use crate::audio::uac::UsbAudio;
use crate::clock;
use crate::net::{Status, Wifi};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
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
    let face = Face::new(window);
    let refresh = {
        let alarm = alarm.clone();
        Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                update(&window, &face, &speaker, &wifi, &alarm.borrow());
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

/// The time's characters, one model entry each: a changed entry is a
/// changed digit, which rolls on the page.
struct Face {
    time: Rc<VecModel<SharedString>>,
    seconds: Rc<VecModel<SharedString>>,
}

impl Face {
    fn new(window: &AppWindow) -> Self {
        let face = Self {
            time: Rc::new(VecModel::from(chars("--:--"))),
            seconds: Rc::new(VecModel::from(chars("--"))),
        };
        let screen = window.global::<Clock>();
        screen.set_time(ModelRc::from(face.time.clone()));
        screen.set_seconds(ModelRc::from(face.seconds.clone()));
        face
    }
}

fn chars(s: &str) -> Vec<SharedString> {
    s.chars()
        .map(|c| SharedString::from(c.encode_utf8(&mut [0; 4]) as &str))
        .collect()
}

/// Sets the entries of `model` that differ from `text`'s characters.
fn set_chars(model: &VecModel<SharedString>, text: &str) {
    for (i, c) in chars(text).into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&c) {
            model.set_row_data(i, c);
        }
    }
}

fn update(window: &AppWindow, face: &Face, speaker: &UsbAudio, wifi: &Wifi, alarm: &AlarmClock) {
    let screen = window.global::<Clock>();
    fn set_text(current: SharedString, new: String, set: impl Fn(SharedString)) {
        if current.as_str() != new {
            set(new.into());
        }
    }
    let t = clock::now();
    set_chars(&face.time, &format!("{:02}:{:02}", t.hour, t.minute));
    set_chars(&face.seconds, &format!("{:02}", t.second));
    set_text(screen.get_date(), t.date_de(), |v| screen.set_date(v));

    let view = alarm.view();
    set_text(screen.get_alarm(), alarm_line(&view, &t), |v| {
        screen.set_alarm(v)
    });
    if matches!(view, View::Ringing { .. }) && window.get_page() != PAGE_CLOCK {
        show_page(window, PAGE_CLOCK);
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

    let no_speaker = !speaker.connected();
    if screen.get_no_speaker() != no_speaker {
        screen.set_no_speaker(no_speaker);
    }
    // Starting and connecting are not errors (yet).
    let no_network = matches!(
        wifi.status(),
        Status::NotConfigured { .. } | Status::Failed { .. }
    );
    if screen.get_no_network() != no_network {
        screen.set_no_network(no_network);
    }
}

/// The bottom line: the next alarm ("Morgen 07:00"), or the snooze.
fn alarm_line(view: &View, now: &DateTime) -> String {
    match view {
        View::Idle { next: None } => "Kein Wecker gestellt".into(),
        View::Idle { next: Some(t) } => {
            let day = if t.date() == now.date() {
                "Heute"
            } else if t.date() == now.add_days(1).date() {
                "Morgen"
            } else {
                WEEKDAYS_DE[usize::from(t.weekday) % 7]
            };
            format!("{day} {:02}:{:02}", t.hour, t.minute)
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
