//! The sound page's TIDAL tab: the login while there is no session (a QR
//! code of the device code link), then the playlist's tracks, followed
//! while the tab is open.

use super::{fill_list, random_choice, row};
use crate::app::screens::{AppWindow, SoundPicker, SoundRow, SoundRowKind, SoundTab, TidalView};
use crate::app::{Ctx, texts};
use crate::tidal::{State, TrackStatus};
use slint::{ComponentHandle, Image, Model, Rgb8Pixel, SharedPixelBuffer, TimerMode};
use std::time::Duration;
use wakey_core::alarm::Sound;
use wakey_core::lang::fill;

/// The tab opened (log in if needed, follow the state) or closed.
pub(super) fn tab(window: &AppWindow, ctx: &Ctx, open: bool) {
    let timer = &ctx.sound.tidal_timer;
    if !open {
        ctx.tidal.close();
        timer.stop();
        return;
    }
    ctx.tidal.open();
    show_state(window, &ctx.tidal.state());
    let weak = window.as_weak();
    let tidal = ctx.tidal.clone();
    let mut seen = (tidal.state(), tidal.version());
    timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        let Some(window) = weak.upgrade() else { return };
        let picker = window.global::<SoundPicker>();
        if picker.get_tab() != SoundTab::Tidal {
            return;
        }
        let now = (tidal.state(), tidal.version());
        if now != seen {
            seen = now;
            picker.invoke_refresh();
        }
        picker.invoke_tick();
    });
}

/// TIDAL's state changed: the tab filled again.
pub(super) fn refresh(window: &AppWindow, ctx: &Ctx) {
    if window.global::<SoundPicker>().get_tab() == SoundTab::Tidal {
        fill_list(window, ctx, SoundTab::Tidal);
        show_state(window, &ctx.tidal.state());
    }
}

/// Moves only the downloading row's bar, in place (no list rebuild).
pub(super) fn tick(window: &AppWindow, ctx: &Ctx) {
    let Some((id, fraction)) = ctx.tidal.progress() else {
        return;
    };
    let want = Sound::TidalTrack(id.to_string());
    let Some(i) = ctx
        .sound
        .tidal_choices
        .borrow()
        .iter()
        .position(|s| *s == want)
    else {
        return;
    };
    let rows = window.global::<SoundPicker>().get_tidal_rows();
    if let Some(mut row) = rows.row_data(i)
        && (row.progress - fraction).abs() >= 0.005
    {
        row.progress = fraction;
        rows.set_row_data(i, row);
    }
}

/// The tab's choices: the playlist, then tracks that left it but an alarm
/// uses; each row and the sound it stands for. Without tracks the tab
/// shows the login or the state ([`show_state`]).
pub(super) fn choices(ctx: &Ctx, selected: &Sound) -> Vec<(SoundRow, Sound)> {
    let tracks = ctx.tidal.listed();
    let progress = ctx.tidal.progress();
    let tidal = &texts::get().tidal;
    let mut choices = Vec::new();
    if tracks.len() > 1 || (tracks.len() == 1 && *selected == Sound::TidalRandom) {
        choices.push(random_choice(Sound::TidalRandom, selected));
    }
    choices.extend(tracks.iter().map(|(t, removed)| {
        let sound = Sound::TidalTrack(t.id.to_string());
        let status = ctx.tidal.track_status(t.id);
        let sub = match status {
            // Failed: the row's icon says it.
            TrackStatus::Ready | TrackStatus::Remote | TrackStatus::Failed => t.artist.clone(),
            TrackStatus::Downloading => fill(&tidal.downloading, &[("artist", &t.artist)]),
            TrackStatus::Missing => fill(&tidal.queued, &[("artist", &t.artist)]),
        };
        let sub = if *removed {
            fill(&tidal.removed, &[("artist", &sub)])
        } else {
            sub
        };
        let mut r = row(&t.title, &sub, &sound, selected, SoundRowKind::TidalTrack);
        r.failed = status == TrackStatus::Failed;
        if let Some((id, fraction)) = progress
            && id == t.id
        {
            r.progress = fraction;
        }
        if let Some(c) = &t.cover {
            r.cover = cover_image(ctx, c);
        }
        (r, sound)
    }));
    choices
}

/// Shows a TIDAL state on the tab (the QR code made anew only when the
/// code changes).
fn show_state(window: &AppWindow, state: &State) {
    let picker = window.global::<SoundPicker>();
    let has_rows = picker.get_tidal_rows().row_count() > 0;
    let tidal = &texts::get().tidal;
    let note: &str = match state {
        State::NoCredentials => &tidal.no_credentials,
        State::LoggedOut | State::Connecting => &tidal.connecting,
        State::Code { url, code } => {
            let line = fill(&tidal.code, &[("code", code)]);
            if picker.get_tidal_code().as_str() != line {
                if let Some(qr) = qr_image(url) {
                    picker.set_tidal_qr(qr);
                }
                picker.set_tidal_code(line.into());
            }
            picker.set_tidal_view(TidalView::Login);
            picker.set_tidal_empty("".into());
            return;
        }
        State::Syncing if has_rows => "",
        State::Syncing => &tidal.syncing,
        State::LoggedIn if has_rows => "",
        State::LoggedIn => &tidal.empty,
        State::Failed(e) => {
            picker.set_tidal_view(TidalView::List);
            let problem = texts::tidal_problem(e);
            picker.set_tidal_empty(fill(&tidal.retry, &[("problem", &problem)]).into());
            return;
        }
    };
    picker.set_tidal_view(TidalView::List);
    picker.set_tidal_code("".into());
    picker.set_tidal_empty(note.into());
}

/// A QR code of `text`, black on white with a quiet zone, about 300 px.
fn qr_image(text: &str) -> Option<Image> {
    use qrcodegen::{QrCode, QrCodeEcc};
    const BORDER: i32 = 3;
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).ok()?;
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

/// A cover from the card, read once (then kept); empty while it is not
/// there yet.
fn cover_image(ctx: &Ctx, id: &str) -> Image {
    if let Some(image) = ctx.sound.covers.borrow().get(id) {
        return image.clone();
    }
    let Some(rgb) = crate::tidal::cover_rgb(id) else {
        return Image::default();
    };
    let px = wakey_core::tidal::sync::COVER_PX;
    let image = Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
        &rgb, px, px,
    ));
    ctx.sound
        .covers
        .borrow_mut()
        .insert(id.to_string(), image.clone());
    image
}
