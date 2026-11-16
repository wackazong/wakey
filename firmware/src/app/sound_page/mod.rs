//! The sound page's view model (`SoundPage` in `ui/wakey/sound_page.slint`):
//! sound, volume and fade-in of the alarm being edited (the draft), and a
//! preview on the speaker.
//!
//! - [`tidal`]: the TIDAL tab: the login (QR code of the device code link,
//!   [`crate::tidal`]), then the playlist's tracks.
//! - [`airdrop`]: the Files tab receives new sound files by AirDrop while it
//!   shows ([`crate::airdrop`]): its note ends the list, a file arriving is
//!   a row above it.
//! - [`rows`]: the list as the list layer scrolls it.

mod airdrop;
mod rows;
mod tidal;

use super::screens::{
    AppWindow, Page, SoundListLayout, SoundPicker, SoundRow, SoundRowKind, SoundTab,
};
use super::{Ctx, alarm_page, show_page};
use crate::sounds::{self, SoundFile};
use crate::tidal::TrackStatus;
use slint::{ComponentHandle, Image, Model, ModelRc, Timer, VecModel};
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;
use wakey_core::alarm::Sound;

pub use rows::layer;

/// Fade-in choices (the pills), seconds.
const FADES: [u16; 4] = [0, 10, 30, 60];

/// What the sound page keeps (in [`Ctx`]).
#[derive(Default)]
pub struct SoundPageState {
    /// `sounds/` as listed when the page opened or a file arrived.
    files: RefCell<Vec<SoundFile>>,
    /// The sound each row of the Files tab stands for.
    file_choices: RefCell<Vec<Sound>>,
    /// The same for the TIDAL tab's rows.
    tidal_choices: RefCell<Vec<Sound>>,
    /// Follows TIDAL's state while the TIDAL tab is open.
    tidal_timer: Timer,
    /// Album covers read from the card, by cover id (RGB888, 88 px: ~23 KiB
    /// each).
    covers: RefCell<HashMap<String, Image>>,
    /// Polls AirDrop's state while the Files tab is open.
    airdrop_timer: Timer,
    /// What the Files tab shows of AirDrop: its state and the files' version.
    airdrop_shown: RefCell<Option<(crate::airdrop::State, u32)>>,
}

/// Shows the page for the draft.
pub fn open(window: &AppWindow, ctx: &Ctx) {
    *ctx.sound.files.borrow_mut() = sounds::list();
    let picker = window.global::<SoundPicker>();
    let (tab, volume, fade) = {
        let cfg = ctx.draft.borrow();
        let tab = match cfg.sound {
            Sound::TidalTrack(_) | Sound::TidalRandom => SoundTab::Tidal,
            _ => SoundTab::Files,
        };
        let fade = FADES
            .iter()
            .enumerate()
            .min_by_key(|(_, f)| f.abs_diff(cfg.fade_in_s))
            .map_or(1, |(i, _)| i as i32);
        (tab, cfg.volume, fade)
    };
    picker.set_tab(tab);
    picker.set_shown_tab(tab);
    picker.set_files_scroll(0.0);
    picker.set_tidal_scroll(0.0);
    picker.set_layer_active(false);
    picker.set_volume(volume.into());
    picker.set_fade(fade);
    picker.set_previewing(false);
    select_tidal(ctx);
    fill_rows(window, ctx);
    tidal::tab(window, ctx, tab == SoundTab::Tidal);
    airdrop::files_tab(window, ctx, tab == SoundTab::Files);
    show_page(window, ctx, Page::Sound);
}

/// The page is left (`super::show_page`): TIDAL's tab and AirDrop close,
/// a preview stops.
pub fn leave(window: &AppWindow, ctx: &Ctx) {
    tidal::tab(window, ctx, false);
    airdrop::files_tab(window, ctx, false);
    ctx.alarm.borrow_mut().stop_preview();
}

/// Tells TIDAL which tracks to have on the card: those of the saved
/// alarms and the one picked in the alarm being edited.
pub fn select_tidal(ctx: &Ctx) {
    let draft = ctx.draft.borrow().sound.clone();
    let alarm = ctx.alarm.borrow();
    let sounds: Vec<&Sound> = alarm
        .alarms()
        .iter()
        .map(|a| &a.sound)
        .chain(std::iter::once(&draft))
        .collect();
    let ids = sounds.iter().filter_map(|s| tidal_id(s)).collect();
    let all = sounds.iter().any(|s| **s == Sound::TidalRandom);
    ctx.tidal.set_selected(ids, all, tidal_id(&draft));
}

/// The TIDAL track id of `sound`, if it is one.
fn tidal_id(sound: &Sound) -> Option<u64> {
    match sound {
        Sound::TidalTrack(id) => id.parse().ok(),
        _ => None,
    }
}

/// Sichern: only after a change, and with at least one day.
fn update_can_save(window: &AppWindow, ctx: &Ctx) {
    let any_day = ctx.draft.borrow().any_day();
    window
        .global::<SoundPicker>()
        .set_can_save(any_day && alarm_page::changed(ctx));
}

/// Fills both tabs' lists (both ready when the tab changes).
fn fill_rows(window: &AppWindow, ctx: &Ctx) {
    update_can_save(window, ctx);
    fill_list(window, ctx, SoundTab::Files);
    fill_list(window, ctx, SoundTab::Tidal);
}

/// A row of the list for `sound`, as chosen or not in the draft.
fn row(title: &str, sub: &str, sound: &Sound, selected: &Sound, kind: SoundRowKind) -> SoundRow {
    SoundRow {
        title: title.into(),
        sub: sub.into(),
        selected: sound == selected,
        kind,
        cover: Image::default(),
        progress: -1.0,
        failed: false,
    }
}

/// The Files tab's choices: each row and the sound it stands for. The
/// built-in tone is only the fallback (a sound that cannot be played),
/// never a choice of its own.
fn file_choices(ctx: &Ctx, selected: &Sound) -> Vec<(SoundRow, Sound)> {
    let files = ctx.sound.files.borrow();
    let mut choices = Vec::new();
    // Only with a choice: with one file it is that file, without any only
    // the built-in tone. Kept while the alarm being edited has it.
    if files.len() > 1 || (files.len() == 1 && *selected == Sound::RandomFile) {
        let sound = Sound::RandomFile;
        choices.push((
            row(
                "Zufällig",
                "wechselt jeden Tag",
                &sound,
                selected,
                SoundRowKind::Random,
            ),
            sound,
        ));
    }
    choices.extend(files.iter().map(|f| {
        let sound = Sound::File(f.path.clone());
        (
            row(&f.title, "", &sound, selected, SoundRowKind::File),
            sound,
        )
    }));
    choices
}

/// Fills one tab's list.
fn fill_list(window: &AppWindow, ctx: &Ctx, tab: SoundTab) {
    let picker = window.global::<SoundPicker>();
    let selected = ctx.draft.borrow().sound.clone();
    let choices = if tab == SoundTab::Files {
        file_choices(ctx, &selected)
    } else {
        tidal::choices(ctx, &selected)
    };
    let (mut rows, sounds): (Vec<SoundRow>, Vec<Sound>) = choices.into_iter().unzip();
    // After the choices, so a row's index still finds its sound.
    if tab == SoundTab::Files {
        rows.extend(airdrop::rows(ctx));
    }
    // A shorter list may leave the view past its end.
    let layout = window.global::<SoundListLayout>();
    let max = (rows.len() as f32 * layout.get_pitch() - layout.get_height()).max(0.0);
    if tab == SoundTab::Files && picker.get_files_scroll() > max {
        picker.set_files_scroll(max);
    } else if tab == SoundTab::Tidal && picker.get_tidal_scroll() > max {
        picker.set_tidal_scroll(max);
    }
    if tab == SoundTab::Files {
        *ctx.sound.file_choices.borrow_mut() = sounds;
    } else {
        *ctx.sound.tidal_choices.borrow_mut() = sounds;
    }
    // As many rows as shown: only the rows that differ, in place. A new
    // model makes Slint repaint the whole list, ~40 ms under the list layer
    // while it scrolls (TIDAL's sync every 5 s, AirDrop's state).
    let model = if tab == SoundTab::Files {
        picker.get_rows()
    } else {
        picker.get_tidal_rows()
    };
    if model.row_count() == rows.len() && !rows.is_empty() {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
        return;
    }
    let rows = ModelRc::new(VecModel::from(rows));
    if tab == SoundTab::Files {
        picker.set_rows(rows);
    } else {
        picker.set_tidal_rows(rows);
    }
}

/// The sounds behind the rows of the list shown.
fn choices<'a>(window: &AppWindow, ctx: &'a Ctx) -> Ref<'a, Vec<Sound>> {
    if window.global::<SoundPicker>().get_shown_tab() == SoundTab::Tidal {
        ctx.sound.tidal_choices.borrow()
    } else {
        ctx.sound.file_choices.borrow()
    }
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    rows::set_corner_caps(window);
    let picker = window.global::<SoundPicker>();

    picker.on_back(ctx.with(|window, ctx| {
        alarm_page::fill(window, ctx);
        show_page(window, ctx, Page::Alarm);
    }));
    picker.on_select_tab(ctx.with_arg(|window, ctx, tab: SoundTab| {
        // The highlight slides now; the list changes when it has arrived
        // (its repaint would stall the slide). Both lists are filled.
        window.global::<SoundPicker>().set_tab(tab);
        tidal::tab(window, ctx, tab == SoundTab::Tidal);
        airdrop::files_tab(window, ctx, tab == SoundTab::Files);
        let weak = window.as_weak();
        Timer::single_shot(Duration::from_millis(200), move || {
            if let Some(window) = weak.upgrade() {
                let picker = window.global::<SoundPicker>();
                if picker.get_tab() == tab {
                    picker.set_shown_tab(tab);
                }
            }
        });
    }));
    picker.on_select(ctx.with_arg(|window, ctx, i: i32| {
        let Some(sound) = choices(window, ctx).get(i as usize).cloned() else {
            return;
        };
        if let Some(id) = tidal_id(&sound) {
            // A failed download cannot be the alarm's sound.
            if ctx.tidal.track_status(id) == TrackStatus::Failed {
                return;
            }
            // Not on the card: downloaded next, before others.
            ctx.tidal.prefer(id);
        }
        ctx.draft.borrow_mut().sound = sound;
        select_tidal(ctx);
        fill_rows(window, ctx);
        // A running preview switches to the new sound.
        let previewing = ctx.alarm.borrow().is_previewing();
        if previewing {
            let cfg = ctx.draft.borrow().clone();
            ctx.alarm.borrow_mut().preview(&cfg, &ctx.tidal);
        }
    }));
    picker.on_refresh(ctx.with(tidal::refresh));
    picker.on_tick(ctx.with(tidal::tick));
    picker.on_airdrop_tick(ctx.with(airdrop::tick));
    picker.on_volume_changed(ctx.with_arg(|window, ctx, v: i32| {
        let v = v.clamp(0, 100) as u8;
        ctx.draft.borrow_mut().volume = v;
        ctx.alarm.borrow_mut().preview_volume(v);
        update_can_save(window, ctx);
    }));
    picker.on_fade_changed(ctx.with_arg(|window, ctx, i: i32| {
        if let Some(&f) = FADES.get(i as usize) {
            ctx.draft.borrow_mut().fade_in_s = f;
        }
        update_can_save(window, ctx);
    }));
    picker.on_save(ctx.with(alarm_page::save));
    picker.on_preview(ctx.with(|window, ctx| {
        let previewing = ctx.alarm.borrow().is_previewing();
        if previewing {
            ctx.alarm.borrow_mut().stop_preview();
        } else {
            let cfg = ctx.draft.borrow().clone();
            ctx.alarm.borrow_mut().preview(&cfg, &ctx.tidal);
        }
        let now = ctx.alarm.borrow().is_previewing();
        window.global::<SoundPicker>().set_previewing(now);
    }));
}
