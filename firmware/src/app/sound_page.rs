//! The sound page's view model (`SoundPage` in `ui/wakey/sound_page.slint`):
//! sound, volume and fade-in of the alarm being edited (the draft), and a
//! preview on the speaker. The TIDAL tab logs in (QR code of the device
//! code link, [`crate::tidal`]); its songs follow.

use super::screens::{AppWindow, SoundPicker, SoundRow};
use super::{Ctx, PAGE_ALARM, PAGE_SOUND, alarm_page, show_page};
use crate::sounds;
use crate::tidal::State;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use std::rc::Rc;
use std::time::Duration;
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
    tidal_tab(window, ctx, tab == TAB_TIDAL);
    show_page(window, PAGE_SOUND);
}

/// The TIDAL tab opened (log in if needed, follow the state) or closed.
fn tidal_tab(window: &AppWindow, ctx: &Ctx, open: bool) {
    if !open {
        ctx.tidal.close();
        ctx.tidal_timer.stop();
        return;
    }
    ctx.tidal.open();
    *ctx.tidal_shown.borrow_mut() = None;
    show_tidal(window, ctx);
    let weak = window.as_weak();
    let tidal = ctx.tidal.clone();
    let mut seen = tidal.state();
    ctx.tidal_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            let Some(window) = weak.upgrade() else { return };
            let picker = window.global::<SoundPicker>();
            let state = tidal.state();
            if picker.get_tab() == TAB_TIDAL && state != seen {
                seen = state;
                picker.invoke_refresh();
            }
        },
    );
}

fn show_tidal(window: &AppWindow, ctx: &Ctx) {
    let state = ctx.tidal.state();
    show_state(window, &state);
    *ctx.tidal_shown.borrow_mut() = Some(state);
}

/// Shows a TIDAL state on the tab (the QR code made anew only when the
/// link changes).
fn show_state(window: &AppWindow, state: &State) {
    let picker = window.global::<SoundPicker>();
    let has_rows = picker.get_rows().row_count() > 0;
    let note = match state {
        State::NoCredentials => {
            "TIDAL ist nicht eingerichtet: die Zugangsdaten fehlen in config.json."
        }
        State::LoggedOut | State::Connecting => "Verbinde mit TIDAL …",
        State::Code { url, code } => {
            if picker.get_tidal_code().as_str() != code {
                if let Some(qr) = qr_image(url) {
                    picker.set_tidal_qr(qr);
                }
                picker.set_tidal_code(code.as_str().into());
            }
            picker.set_tidal_view(1);
            picker.set_empty("".into());
            return;
        }
        State::Syncing if has_rows => "",
        State::Syncing => "Lade die Playlist …",
        State::LoggedIn if has_rows => "",
        State::LoggedIn => "Die Playlist ist leer.",
        State::Failed(e) => {
            picker.set_tidal_view(0);
            picker.set_empty(format!("{e} Zum Wiederholen den Tab erneut öffnen.").into());
            return;
        }
    };
    picker.set_tidal_view(0);
    picker.set_tidal_code("".into());
    picker.set_empty(note.into());
}

/// A QR code of `text`, black on white with a quiet zone, about 300 px.
fn qr_image(text: &str) -> Option<Image> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
    const BORDER: i32 = 3;
    let modules = qr.size() + 2 * BORDER;
    let scale = (300 / modules).max(1);
    let px = (modules * scale) as u32;
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(px, px);
    let data = buf.make_mut_slice();
    for y in 0..px as i32 {
        for x in 0..px as i32 {
            let dark = qr.get_module(x / scale - BORDER, y / scale - BORDER);
            let v = if dark { 0 } else { 255 };
            data[(y * px as i32 + x) as usize] = Rgb8Pixel { r: v, g: v, b: v };
        }
    }
    Some(Image::from_rgb8(buf))
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
    // Each row and the sound it stands for.
    let choices: Vec<(SoundRow, Sound)> = if picker.get_tab() == TAB_FILES {
        let files = ctx.sound_files.borrow();
        let mut choices = vec![(
            row("Eingebauter Ton", "Piepen", &Sound::Beep, 0),
            Sound::Beep,
        )];
        if files.is_empty() {
            // "Zufällig" would only play the built-in tone.
            picker.set_empty("Keine Klänge auf der Speicherkarte (Ordner „sounds“).".into());
        } else {
            picker.set_empty("".into());
            choices.push((
                row("Zufällig", "wechselt jeden Tag", &Sound::RandomFile, 1),
                Sound::RandomFile,
            ));
        }
        choices.extend(files.iter().map(|f| {
            let sound = Sound::File(f.path.clone());
            (row(&f.title, "", &sound, 2), sound)
        }));
        choices
    } else {
        // The playlist; the login or the state shown by `show_state`.
        let tracks = ctx.tidal.playlist();
        let mut choices = Vec::new();
        if !tracks.is_empty() {
            choices.push((
                row("Zufällig", "wechselt jeden Tag", &Sound::TidalRandom, 1),
                Sound::TidalRandom,
            ));
        }
        choices.extend(tracks.iter().map(|t| {
            let sound = Sound::TidalTrack(t.id.to_string());
            (row(&t.title, &t.artist, &sound, 2), sound)
        }));
        choices
    };
    let (rows, sounds): (Vec<SoundRow>, Vec<Sound>) = choices.into_iter().unzip();
    *ctx.sound_choices.borrow_mut() = sounds;
    picker.set_rows(ModelRc::new(VecModel::from(rows)));
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let picker = window.global::<SoundPicker>();

    picker.on_back(ctx.with(|window, ctx| {
        tidal_tab(window, ctx, false);
        ctx.alarm.borrow_mut().stop_preview();
        alarm_page::fill(window, ctx);
        show_page(window, PAGE_ALARM);
    }));
    picker.on_select_tab(ctx.with_arg(|window, ctx, tab: i32| {
        window.global::<SoundPicker>().set_tab(tab);
        fill_rows(window, ctx);
        tidal_tab(window, ctx, tab == TAB_TIDAL);
    }));
    picker.on_select(ctx.with_arg(|window, ctx, i: i32| {
        let Some(sound) = ctx.sound_choices.borrow().get(i as usize).cloned() else {
            return;
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
    picker.on_refresh(ctx.with(|window, ctx| {
        if window.global::<SoundPicker>().get_tab() == TAB_TIDAL {
            fill_rows(window, ctx);
            show_tidal(window, ctx);
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
