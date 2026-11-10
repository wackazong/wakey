//! The sound page's view model (`SoundPage` in `ui/wakey/sound_page.slint`):
//! sound, volume and fade-in of the alarm being edited (the draft), and a
//! preview on the speaker. TIDAL is a placeholder until it is set up.

use super::screens::{AppWindow, SoundPicker, SoundRow};
use super::{Ctx, PAGE_ALARM, PAGE_SOUND, alarm_page};
use crate::sounds;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;
use wakey_core::alarm::Sound;

/// Fade-in choices (the pills), seconds.
const FADES: [u16; 4] = [0, 10, 30, 60];

const TAB_FILES: i32 = 0;
const TAB_TIDAL: i32 = 1;

/// Shows the page for the draft.
pub fn open(window: &AppWindow, ctx: &Ctx) {
    *ctx.sound_files.borrow_mut() = sounds::list();
    let picker = window.global::<SoundPicker>();
    let (tab, volume, fade) = {
        let cfg = ctx.draft.borrow();
        let tab = match cfg.sound {
            Sound::TidalTrack(_) | Sound::TidalRandom => TAB_TIDAL,
            _ => TAB_FILES,
        };
        let fade = FADES
            .iter()
            .enumerate()
            .min_by_key(|(_, f)| f.abs_diff(cfg.fade_in_s))
            .map_or(1, |(i, _)| i as i32);
        (tab, cfg.volume, fade)
    };
    picker.set_tab(tab);
    picker.set_volume(volume.into());
    picker.set_fade(fade);
    picker.set_previewing(false);
    fill_rows(window, ctx);
    window.set_page(PAGE_SOUND);
}

fn fill_rows(window: &AppWindow, ctx: &Ctx) {
    let picker = window.global::<SoundPicker>();
    let selected = ctx.draft.borrow().sound.clone();
    let row = |title: &str, sub: &str, sound: &Sound, kind| SoundRow {
        title: title.into(),
        sub: sub.into(),
        selected: *sound == selected,
        kind,
    };
    let rows = if picker.get_tab() == TAB_FILES {
        let files = ctx.sound_files.borrow();
        let random_sub = if files.is_empty() {
            "keine Dateien in sounds/"
        } else {
            "wechselt jeden Tag"
        };
        let mut rows = vec![
            row("Eingebauter Ton", "Piepen", &Sound::Beep, 0),
            row("Zufällig", random_sub, &Sound::RandomFile, 1),
        ];
        rows.extend(
            files
                .iter()
                .map(|f| row(&f.title, "", &Sound::File(f.path.clone()), 2)),
        );
        rows
    } else {
        picker.set_empty("TIDAL ist noch nicht eingerichtet.".into());
        Vec::new()
    };
    picker.set_rows(ModelRc::new(VecModel::from(rows)));
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let picker = window.global::<SoundPicker>();

    picker.on_back(ctx.with(|window, ctx| {
        ctx.alarm.borrow_mut().stop_preview();
        alarm_page::fill(window, ctx);
        window.set_page(PAGE_ALARM);
    }));
    picker.on_select_tab(ctx.with_arg(|window, ctx, tab: i32| {
        window.global::<SoundPicker>().set_tab(tab);
        fill_rows(window, ctx);
    }));
    picker.on_select(ctx.with_arg(|window, ctx, i: i32| {
        if window.global::<SoundPicker>().get_tab() != TAB_FILES {
            return;
        }
        let sound = match i {
            0 => Sound::Beep,
            1 => Sound::RandomFile,
            i => match ctx.sound_files.borrow().get(i as usize - 2) {
                Some(f) => Sound::File(f.path.clone()),
                None => return,
            },
        };
        ctx.draft.borrow_mut().sound = sound;
        fill_rows(window, ctx);
        // A running preview switches to the new sound.
        let previewing = ctx.alarm.borrow().is_previewing();
        if previewing {
            let cfg = ctx.draft.borrow().clone();
            ctx.alarm.borrow_mut().preview(&cfg);
        }
    }));
    picker.on_volume_changed(ctx.with_arg(|_, ctx, v: i32| {
        let v = v.clamp(0, 100) as u8;
        ctx.draft.borrow_mut().volume = v;
        ctx.alarm.borrow_mut().preview_volume(v);
    }));
    picker.on_fade_changed(ctx.with_arg(|_, ctx, i: i32| {
        if let Some(&f) = FADES.get(i as usize) {
            ctx.draft.borrow_mut().fade_in_s = f;
        }
    }));
    picker.on_preview(ctx.with(|window, ctx| {
        let previewing = ctx.alarm.borrow().is_previewing();
        if previewing {
            ctx.alarm.borrow_mut().stop_preview();
        } else {
            let cfg = ctx.draft.borrow().clone();
            ctx.alarm.borrow_mut().preview(&cfg);
        }
        let now = ctx.alarm.borrow().is_previewing();
        window.global::<SoundPicker>().set_previewing(now);
    }));
}
