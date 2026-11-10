//! The alarm settings page's view model (`AlarmPage` in
//! `ui/wakey/alarm_page.slint`). Tapping the alarm line on the clock page
//! copies the current settings into the `AlarmEditor` global and shows the
//! page; on/off and the time are edited in Slint, the weekday selection is
//! kept here (the chips report toggles, see the `.slint` file for why);
//! Save hands the result to [`AlarmClock::set_config`] (written to
//! `config.json`, then in effect). The page closes by itself after a minute
//! without a change, and when the alarm rings (`clock_screen`).

use super::alarm::AlarmClock;
use super::screens::{AlarmEditor, AppWindow, Clock};
use super::{PAGE_ALARM, PAGE_CLOCK};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use wakey_core::alarm::{AlarmConfig, weekdays_de};

/// The page closes after this long without a change.
const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// Keeps the inactivity check running while alive.
pub struct AlarmPage {
    _idle: Timer,
}

/// What is being edited, besides the values in the `AlarmEditor` global.
struct Edit {
    alarm: Rc<RefCell<AlarmClock>>,
    /// Monday to Sunday.
    days: Cell<[bool; 7]>,
    last_change: Cell<Instant>,
}

impl Edit {
    /// The settings as edited; fields the page does not edit stay.
    fn config(&self, editor: &AlarmEditor) -> AlarmConfig {
        AlarmConfig {
            enabled: editor.get_enabled(),
            hour: editor.get_hour().clamp(0, 23) as u8,
            minute: editor.get_minute().clamp(0, 59) as u8,
            weekdays: self.days.get().map(u8::from),
            ..self.alarm.borrow().config().clone()
        }
    }

    /// After any change: summary, save button, inactivity timer.
    fn changed(&self, editor: &AlarmEditor) {
        self.last_change.set(Instant::now());
        let cfg = self.config(editor);
        editor.set_any_day(self.days.get().contains(&true));
        editor.set_summary(summary(&cfg).into());
        editor.set_error("".into());
    }
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, alarm: Rc<RefCell<AlarmClock>>) -> AlarmPage {
    let edit = Rc::new(Edit {
        alarm,
        days: Cell::new([false; 7]),
        last_change: Cell::new(Instant::now()),
    });
    let editor = window.global::<AlarmEditor>();

    // Each callback gets the window and the editor global.
    let with = |f: fn(&AppWindow, &AlarmEditor, &Edit)| {
        let weak = window.as_weak();
        let edit = edit.clone();
        move || {
            if let Some(window) = weak.upgrade() {
                f(&window, &window.global::<AlarmEditor>(), &edit);
            }
        }
    };

    window
        .global::<Clock>()
        .on_open_alarm(with(|window, editor, edit| {
            let cfg = edit.alarm.borrow().config().clone();
            let days = cfg.weekdays.map(|d| d != 0);
            edit.days.set(days);
            editor.set_enabled(cfg.enabled);
            editor.set_hour(cfg.hour.into());
            editor.set_minute(cfg.minute.into());
            editor.set_days(ModelRc::new(VecModel::from(days.to_vec())));
            edit.changed(editor);
            window.set_page(PAGE_ALARM);
        }));

    editor.on_edited(with(|_, editor, edit| edit.changed(editor)));

    editor.on_day_toggled({
        let weak = window.as_weak();
        let edit = edit.clone();
        move |day, on| {
            let Some(window) = weak.upgrade() else { return };
            let mut days = edit.days.get();
            if let Some(d) = days.get_mut(day as usize) {
                *d = on;
            }
            edit.days.set(days);
            edit.changed(&window.global::<AlarmEditor>());
        }
    });

    editor.on_cancel(with(|window, _, _| window.set_page(PAGE_CLOCK)));

    editor.on_save(with(|window, editor, edit| {
        let cfg = edit.config(editor);
        let result = edit.alarm.borrow_mut().set_config(cfg);
        match result {
            Ok(()) => window.set_page(PAGE_CLOCK),
            Err(e) => {
                log::error!("alarm page: saving: {e}");
                editor.set_error("Speichern fehlgeschlagen – SD-Karte prüfen".into());
            }
        }
    }));

    let idle = Timer::default();
    idle.start(
        TimerMode::Repeated,
        Duration::from_secs(5),
        with(|window, _, edit| {
            if window.get_page() == PAGE_ALARM && edit.last_change.get().elapsed() >= IDLE_CLOSE {
                log::info!("alarm page: closed after {} s idle", IDLE_CLOSE.as_secs());
                window.set_page(PAGE_CLOCK);
            }
        }),
    );
    AlarmPage { _idle: idle }
}

/// "Klingelt Mo–Fr um 06:30".
fn summary(cfg: &AlarmConfig) -> String {
    if !cfg.enabled {
        return "Klingelt nicht".into();
    }
    if !cfg.weekdays.contains(&1) {
        return String::new();
    }
    format!(
        "Klingelt {} um {:02}:{:02}",
        weekdays_de(&cfg.weekdays),
        cfg.hour,
        cfg.minute
    )
}
