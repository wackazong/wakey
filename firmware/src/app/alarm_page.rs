//! The alarm page's view model (`AlarmPage` in `ui/wakey/alarm_page.slint`):
//! edits a draft of one alarm ([`super::Ctx::draft`]), shared with the sound
//! and name pages. The wheels and chips keep their own state and report
//! changes; saving hands the draft to [`AlarmClock::save`] (written to
//! `config.json`, then in effect), cancelling drops it.

use super::alarm::AlarmClock;
use super::screens::{AlarmEditor, AppWindow, Page};
use super::{Ctx, alarm_list, name_page, show_page, sound_page, texts};
use crate::clock;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;
use wakey_core::alarm::AlarmConfig;
use wakey_core::lang::fill as fill_text;

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
    show_page(window, ctx, Page::Alarm);
}

/// Everything derived from the draft: name, sound, volume, "Rings in".
pub fn fill(window: &AppWindow, ctx: &Ctx) {
    let editor = window.global::<AlarmEditor>();
    take_time(&editor, ctx);
    let cfg = ctx.draft.borrow();
    editor.set_name(cfg.name.as_str().into());
    editor.set_any_day(cfg.any_day());
    editor.set_changed(changed(ctx));
    editor.set_sound(texts::sound_title(&cfg.sound).into());
    editor.set_volume_bars((i32::from(cfg.volume) + 19) / 20);
    editor.set_fade(fade_text(cfg.fade_in_s).into());
    editor.set_relative(relative(&ctx.alarm.borrow(), &cfg).into());
}

/// The wheels' time into the draft: the rows in their middle, also while
/// one still turns.
fn take_time(editor: &AlarmEditor, ctx: &Ctx) {
    let mut cfg = ctx.draft.borrow_mut();
    cfg.hour = editor.get_hour().clamp(0, 23) as u8;
    cfg.minute = editor.get_minute().clamp(0, 59) as u8;
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

/// Save (alarm and sound page): saves the draft, switched on, and shows
/// the list; an error stays on the alarm page.
pub fn save(window: &AppWindow, ctx: &Ctx) {
    take_time(&window.global::<AlarmEditor>(), ctx);
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
            show_page(window, ctx, Page::AlarmList);
        }
        Err(e) => {
            log::error!("alarm page: saving: {e}");
            fill(window, ctx);
            window
                .global::<AlarmEditor>()
                .set_error(texts::get().alarm.save_failed.as_str().into());
            show_page(window, ctx, Page::Alarm);
        }
    }
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let editor = window.global::<AlarmEditor>();

    editor.on_edited(ctx.with(fill));
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
    editor.on_cancel(ctx.with(|window, ctx| show_page(window, ctx, Page::AlarmList)));
    editor.on_save(ctx.with(save));
}

/// The fade-in as the sound row shows it.
fn fade_text(fade_in_s: u16) -> String {
    if fade_in_s == 0 {
        texts::get().labels.off.clone()
    } else {
        format!("{fade_in_s} s")
    }
}

/// "Rings in 9 h 14 min"
fn relative(alarm: &AlarmClock, cfg: &AlarmConfig) -> String {
    if !cfg.any_day() {
        return String::new();
    }
    let enabled = AlarmConfig {
        enabled: true,
        ..cfg.clone()
    };
    let texts = texts::get();
    let Some(next) = alarm.next_of(&enabled) else {
        return texts.alarm.rings_never.clone();
    };
    // Whole minutes, rounded up (06:29:30 → 06:30 is "1 min").
    let minutes = (clock::unix(&next) - clock::moment().unix + 59) / 60;
    fill_text(&texts.alarm.rings_in, &[("time", &texts.duration(minutes))])
}
