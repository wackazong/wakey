//! The alarm page's view model (`AlarmPage` in `ui/wakey/alarm_page.slint`):
//! edits a draft of one alarm ([`super::Ctx::draft`]), shared with the sound
//! and name pages. The wheels and chips keep their own state and report
//! changes; Sichern hands the draft to [`AlarmClock::save`] (written to
//! `config.json`, then in effect), Abbrechen drops it.

use super::alarm::AlarmClock;
use super::screens::{AlarmEditor, AppWindow};
use super::{Ctx, PAGE_ALARM, PAGE_LIST, alarm_list, name_page, show_page, sound_page};
use crate::clock;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;
use wakey_core::alarm::AlarmConfig;

/// Shows the page for `cfg` (id 0: a new alarm).
pub fn open(window: &AppWindow, ctx: &Ctx, cfg: AlarmConfig) {
    // The wheels click: the speaker starts now, not at the first click.
    ctx.click.wake();
    // Wi-Fi up and the TIDAL playlist synced before the sound page opens.
    ctx.tidal.warm();
    let editor = window.global::<AlarmEditor>();
    editor.set_hour(cfg.hour.into());
    editor.set_minute(cfg.minute.into());
    editor.set_days(ModelRc::new(VecModel::from(
        cfg.weekdays.map(|d| d != 0).to_vec(),
    )));
    editor.set_error("".into());
    *ctx.original.borrow_mut() = (cfg.id != 0).then(|| cfg.clone());
    *ctx.draft.borrow_mut() = cfg;
    fill(window, ctx);
    show_page(window, PAGE_ALARM);
}

/// Everything derived from the draft: name, sound, volume, "Klingelt in".
pub fn fill(window: &AppWindow, ctx: &Ctx) {
    let editor = window.global::<AlarmEditor>();
    let cfg = ctx.draft.borrow();
    editor.set_name(cfg.name.as_str().into());
    editor.set_any_day(cfg.any_day());
    editor.set_changed(changed(ctx));
    editor.set_sound(super::texts::sound_title(&cfg.sound).into());
    editor.set_volume_bars((i32::from(cfg.volume) + 19) / 20);
    editor.set_fade(fade_text(cfg.fade_in_s).into());
    editor.set_relative(relative(&ctx.alarm.borrow(), &cfg).into());
}

/// The draft differs from the alarm as saved (a new alarm always does);
/// switched on or off does not count (the list's switch does that).
pub fn changed(ctx: &Ctx) -> bool {
    let draft = ctx.draft.borrow();
    match &*ctx.original.borrow() {
        Some(saved) => {
            AlarmConfig {
                enabled: saved.enabled,
                ..draft.clone()
            } != *saved
        }
        None => true,
    }
}

/// Sichern (alarm and sound page): saves the draft, switched on, and shows
/// the list; an error stays on the alarm page.
pub fn save(window: &AppWindow, ctx: &Ctx) {
    // Saving a changed alarm means it should ring: switched on.
    let cfg = AlarmConfig {
        enabled: true,
        ..ctx.draft.borrow().clone()
    };
    let result = ctx.alarm.borrow_mut().save(cfg);
    // The saved alarms decide which TIDAL tracks stay on the card.
    sound_page::select_tidal(ctx);
    match result {
        Ok(()) => {
            alarm_list::refresh(window, &ctx.alarm.borrow());
            show_page(window, PAGE_LIST);
        }
        Err(e) => {
            log::error!("alarm page: saving: {e}");
            fill(window, ctx);
            window
                .global::<AlarmEditor>()
                .set_error("Speichern fehlgeschlagen – SD-Karte prüfen".into());
            show_page(window, PAGE_ALARM);
        }
    }
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let editor = window.global::<AlarmEditor>();

    editor.on_edited(ctx.with(|window, ctx| {
        let editor = window.global::<AlarmEditor>();
        {
            let mut cfg = ctx.draft.borrow_mut();
            cfg.hour = editor.get_hour().clamp(0, 23) as u8;
            cfg.minute = editor.get_minute().clamp(0, 59) as u8;
        }
        fill(window, ctx);
    }));
    editor.on_ticked({
        let ctx = ctx.clone();
        move || ctx.click.click()
    });
    editor.on_day_toggled(ctx.with_args(|window, ctx, day: i32, on: bool| {
        if let Some(d) = ctx.draft.borrow_mut().weekdays.get_mut(day as usize) {
            *d = u8::from(on);
        }
        fill(window, ctx);
    }));
    editor.on_edit_name(ctx.with(name_page::open));
    editor.on_open_sound(ctx.with(sound_page::open));
    editor.on_cancel(ctx.with(|window, _| show_page(window, PAGE_LIST)));
    editor.on_save(ctx.with(save));
}

pub fn fade_text(fade_in_s: u16) -> String {
    if fade_in_s == 0 {
        "aus".into()
    } else {
        format!("{fade_in_s} s")
    }
}

/// "Klingelt in 9 Std. 14 Min."
fn relative(alarm: &AlarmClock, cfg: &AlarmConfig) -> String {
    if !cfg.any_day() {
        return String::new();
    }
    let enabled = AlarmConfig {
        enabled: true,
        ..cfg.clone()
    };
    let Some(next) = alarm.next_of(&enabled) else {
        return "Klingelt nicht".into();
    };
    let now = clock::now();
    // Whole minutes, rounded up (06:29:30 → 06:30 is "1 Min.").
    let mins = (next.to_unix() - now.to_unix() + 59) / 60;
    let (d, h, m) = (mins / 1440, mins % 1440 / 60, mins % 60);
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d} Tg."));
    }
    if h > 0 {
        parts.push(format!("{h} Std."));
    }
    if m > 0 || parts.is_empty() {
        parts.push(format!("{m} Min."));
    }
    format!("Klingelt in {}", parts.join(" "))
}
