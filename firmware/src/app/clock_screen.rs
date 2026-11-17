//! The clock screen's view model: once a second, aligned to the clock's
//! second, the alarm is advanced and the time, date, alarm, the sky
//! (`wakey_core::sky`, from the sun's elevation at the configured place) and the
//! missing-speaker and missing-network flags go into the `Clock`
//! properties. Only fields that changed are set, so Slint repaints only
//! those (a seconds tick dirties ~110 panel rows). The ringing controls are
//! on this screen too.

use super::alarm::View;
use super::screens::{AlarmPhase, AppWindow, Clock, Page};
use super::{Ctx, show_page, texts};
use crate::audio::uac::UsbAudio;
use crate::clock;
use crate::net::{Status, Wifi};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::rc::Rc;
use std::time::{Duration, Instant};
use wakey_core::alarm::Input;
use wakey_core::datetime::DateTime;
use wakey_core::lang::fill;
use wakey_core::sky;

/// Keeps the screen updating while alive.
pub struct Ticker {
    _align: Timer,
    _every_second: Rc<Timer>,
    /// `sky-demo` builds: runs the sky through the day.
    _sky_demo: Option<Timer>,
}

/// Fills the screen now and then every second, and connects the ringing
/// controls to the alarm. When the alarm rings, the clock page is shown.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>, speaker: UsbAudio, wifi: Wifi) -> Ticker {
    let alarm = ctx.alarm.clone();
    let face = Face::new(window);
    let refresh = {
        let ctx = ctx.clone();
        Rc::new(move || {
            if let Some(window) = ctx.window.upgrade() {
                update(&window, &ctx, &face, &speaker, &wifi);
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
        clock::until_next_second() + Duration::from_millis(2),
        move || {
            tick();
            repeat.start(TimerMode::Repeated, Duration::from_secs(1), tick.clone());
        },
    );
    Ticker {
        _align: align,
        _every_second: every_second,
        _sky_demo: cfg!(feature = "sky-demo").then(|| sky_demo(window, ctx.location)),
    }
}

/// One real minute for the whole of today, the sun as over `place`: the sky
/// steps every 200 ms (~5 minutes of the day), the bottom line shows that
/// time and the sun's elevation.
fn sky_demo(window: &AppWindow, place: sky::Location) -> Timer {
    const DAY_IN: Duration = Duration::from_secs(60);
    const STEP: Duration = Duration::from_millis(200);
    let weak = window.as_weak();
    let start = Instant::now();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, STEP, move || {
        let Some(window) = weak.upgrade() else { return };
        let now = clock::moment();
        let t = now.local;
        let midnight =
            now.unix - i64::from(t.hour) * 3600 - i64::from(t.minute) * 60 - i64::from(t.second);
        let day_part =
            (start.elapsed().as_millis() % DAY_IN.as_millis()) as f64 / DAY_IN.as_millis() as f64;
        let of_day = (day_part * 86_400.0) as i64;
        let elevation = set_sky(&window, place, midnight + of_day);
        window.global::<Clock>().set_alarm(
            format!(
                "{:02}:{:02} · sun {elevation:.0}°",
                of_day / 3600,
                of_day / 60 % 60
            )
            .into(),
        );
    });
    timer
}

/// Sets the properties of `$global` whose value differs from the new one.
macro_rules! set_changed {
    ($global:expr; $($get:ident, $set:ident = $v:expr;)*) => {$(
        if $global.$get() != $v {
            $global.$set($v);
        }
    )*};
}

/// Sets the sky over `place` for Unix time `unix`; returns the sun's
/// elevation.
fn set_sky(window: &AppWindow, place: sky::Location, unix: i64) -> f32 {
    let screen = window.global::<Clock>();
    let elevation = sky::sun_elevation(unix, place);
    let rising = sky::sun_rising(unix, place);
    let gradient = sky::gradient(elevation, rising);
    let [top, upper, middle, lower, bottom] =
        gradient.map(|c| slint::Color::from_rgb_u8(c.0, c.1, c.2));
    let light = sky::bottom_is_light(&gradient);
    // Each stop only when it changed (a change repaints the screen).
    set_changed! { screen;
        get_sky_top, set_sky_top = top;
        get_sky_upper, set_sky_upper = upper;
        get_sky_middle, set_sky_middle = middle;
        get_sky_lower, set_sky_lower = lower;
        get_sky_bottom, set_sky_bottom = bottom;
        get_bottom_light, set_bottom_light = light;
    }
    elevation
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

/// Sets a text property if `new` differs from `current`.
fn set_text(current: SharedString, new: String, set: impl Fn(SharedString)) {
    if current.as_str() != new {
        set(new.into());
    }
}

/// Brings the screen up to the current time and the alarm's state.
fn update(window: &AppWindow, ctx: &Ctx, face: &Face, speaker: &UsbAudio, wifi: &Wifi) {
    let screen = window.global::<Clock>();
    let now = clock::moment();
    let t = now.local;
    // The sky changes at most once a minute (a whole-screen repaint, ~66
    // ms per frame buffer): it is computed for the minute, not the second.
    if !cfg!(feature = "sky-demo") {
        set_sky(window, ctx.location, now.unix - now.unix.rem_euclid(60));
    }
    set_chars(&face.time, &format!("{:02}:{:02}", t.hour, t.minute));
    set_chars(&face.seconds, &format!("{:02}", t.second));
    set_text(screen.get_date(), texts::get().date(&t), |v| {
        screen.set_date(v)
    });

    let view = ctx.alarm.borrow().view();
    if !cfg!(feature = "sky-demo") {
        set_text(screen.get_alarm(), alarm_line(&view, &t), |v| {
            screen.set_alarm(v)
        });
    }
    if matches!(view, View::Ringing { .. }) && window.get_page() != Page::Clock {
        show_page(window, ctx, Page::Clock);
    }
    let (phase, snoozes_left) = match view {
        View::Idle { .. } => (AlarmPhase::Idle, 0),
        View::Ringing { snoozes_left } => (AlarmPhase::Ringing, snoozes_left),
        View::Snoozed { snoozes_left, .. } => (AlarmPhase::Snoozed, snoozes_left),
    };
    let hint = fill(&texts::get().clock.snooze_hint, &[("n", &snoozes_left)]);
    set_text(screen.get_snooze_hint(), hint, |v| {
        screen.set_snooze_hint(v)
    });
    let snoozes_left = i32::from(snoozes_left);
    let no_speaker = speaker.missing();
    // Starting and connecting are not errors (yet).
    let no_network = matches!(
        wifi.status(),
        Status::NotConfigured { .. } | Status::Failed { .. }
    );
    set_changed! { screen;
        get_alarm_phase, set_alarm_phase = phase;
        get_snoozes_left, set_snoozes_left = snoozes_left;
        get_no_speaker, set_no_speaker = no_speaker;
        get_no_network, set_no_network = no_network;
    }
}

/// The bottom line: the next alarm ("Tomorrow 07:00"), or the snooze.
fn alarm_line(view: &View, now: &DateTime) -> String {
    let all = texts::get();
    let texts = &all.clock;
    let time = |t: &DateTime| format!("{:02}:{:02}", t.hour, t.minute);
    match view {
        View::Idle { next: None } => texts.no_alarm.clone(),
        View::Idle { next: Some(t) } => {
            let day = if t.date() == now.date() {
                &texts.today
            } else if t.date() == now.add_days(1).date() {
                &texts.tomorrow
            } else {
                all.weekday(t)
            };
            fill(&texts.next_alarm, &[("day", &day), ("time", &time(t))])
        }
        // The page shows what a tap does instead.
        View::Ringing { .. } => String::new(),
        View::Snoozed {
            until,
            snoozes_left: 0,
        } => fill(&texts.snoozed_last, &[("time", &time(until))]),
        View::Snoozed {
            until,
            snoozes_left,
        } => fill(
            &texts.snoozed,
            &[("time", &time(until)), ("n", snoozes_left)],
        ),
    }
}
