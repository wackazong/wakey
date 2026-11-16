//! The sound page's view model (`SoundPage` in `ui/wakey/sound_page.slint`):
//! sound, volume and fade-in of the alarm being edited (the draft), and a
//! preview on the speaker. The TIDAL tab logs in (QR code of the device
//! code link, [`crate::tidal`]); its songs follow. While the Files tab
//! shows, AirDrop receives new sound files ([`crate::airdrop`]): its note
//! ends the list, a file arriving is a row above it.

use super::screens::{AppWindow, SoundListLayout, SoundPicker, SoundRow, Theme};
use super::{Ctx, PAGE_ALARM, PAGE_SOUND, PAGE_SOUND_ROW, alarm_page, show_page};
use crate::airdrop;
use crate::sounds;
use crate::tidal::{State, TrackStatus};
use crate::ui::list_layer::{Buffers, Layout, ListLayer, ListSource, SwipeEnd, SwipePart};
use crate::ui::platform;
use slint::Weak;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
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
    picker.set_shown_tab(tab);
    picker.set_files_scroll(0.0);
    picker.set_tidal_scroll(0.0);
    picker.set_layer_active(false);
    picker.set_volume(volume.into());
    picker.set_fade(fade);
    picker.set_previewing(false);
    select_tidal(ctx);
    fill_rows(window, ctx);
    tidal_tab(window, ctx, tab == TAB_TIDAL);
    files_tab(window, tab == TAB_FILES);
    show_page(window, ctx, PAGE_SOUND);
}

thread_local! {
    /// Polls AirDrop's state while the Files tab is open.
    static AIRDROP_TIMER: slint::Timer = slint::Timer::default();
    /// What the Files tab shows of AirDrop: the state and the files' version.
    // Clippy flags this block although both are as `const` as they can be
    // (`Timer::default` is not a const fn).
    #[allow(clippy::missing_const_for_thread_local)]
    static AIRDROP_SHOWN: RefCell<Option<(airdrop::State, u32)>> = const { RefCell::new(None) };
}

/// The page is left (`super::show_page`): TIDAL's tab and AirDrop close,
/// a preview stops.
pub fn leave(window: &AppWindow, ctx: &Ctx) {
    tidal_tab(window, ctx, false);
    files_tab(window, false);
    ctx.alarm.borrow_mut().stop_preview();
}

/// The Files tab opened or closed: AirDrop on or off, and followed.
fn files_tab(window: &AppWindow, open: bool) {
    airdrop::set_wanted(open);
    AIRDROP_TIMER.with(|t| {
        if !open {
            t.stop();
            return;
        }
        let weak = window.as_weak();
        t.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                if let Some(window) = weak.upgrade() {
                    window.global::<SoundPicker>().invoke_airdrop_tick();
                }
            },
        );
    });
}

/// The Files tab's last rows for AirDrop: a file arriving, then the note.
fn airdrop_rows(state: &airdrop::State) -> Vec<SoundRow> {
    let row = |title: String, sub: String, kind, progress| SoundRow {
        title: title.into(),
        sub: sub.into(),
        selected: false,
        kind,
        cover: Image::default(),
        progress,
        failed: false,
    };
    let note = match state {
        airdrop::State::Off | airdrop::State::Starting => "AirDrop startet …".to_string(),
        airdrop::State::Ready | airdrop::State::Receiving(_) => {
            "AirDrop bereit für neue Klänge".to_string()
        }
        airdrop::State::Failed => "AirDrop nicht verfügbar".to_string(),
    };
    let mut rows = Vec::new();
    if let airdrop::State::Receiving(p) = state {
        rows.push(row(
            p.name.clone(),
            "wird empfangen …".into(),
            5,
            fraction(p),
        ));
    }
    rows.push(row(note, String::new(), 4, -1.0));
    rows
}

fn fraction(p: &wakey_core::airdrop::Progress) -> f32 {
    if p.size == 0 {
        0.0
    } else {
        (f64::from(p.got) / f64::from(p.size)) as f32
    }
}

/// Follows AirDrop on the Files tab: the list again when files arrived or
/// the state changed, else only the arriving file's row.
fn airdrop_tick(window: &AppWindow, ctx: &Ctx) {
    let state = airdrop::state();
    let files = airdrop::files_version();
    let shown = AIRDROP_SHOWN.with(|s| s.borrow().clone());
    let same_kind = |a: &airdrop::State, b: &airdrop::State| match (a, b) {
        (airdrop::State::Receiving(x), airdrop::State::Receiving(y)) => x.name == y.name,
        (a, b) => a == b,
    };
    match shown {
        Some((s, f)) if f == files && same_kind(&s, &state) => {
            if s == state {
                return;
            }
            // The same file further on: its row in place.
            if let airdrop::State::Receiving(p) = &state {
                let rows = window.global::<SoundPicker>().get_rows();
                let i = rows.row_count().saturating_sub(2);
                // As TIDAL's bar: only a visible step repaints the row.
                if let Some(mut row) = rows.row_data(i)
                    && row.kind == 5
                    && (row.progress - fraction(p)).abs() >= 0.005
                {
                    row.progress = fraction(p);
                    rows.set_row_data(i, row);
                }
            }
        }
        Some((_, f)) if f != files => {
            *ctx.sound_files.borrow_mut() = sounds::list();
            fill_list(window, ctx, TAB_FILES);
        }
        _ => fill_list(window, ctx, TAB_FILES),
    }
    AIRDROP_SHOWN.with(|s| *s.borrow_mut() = Some((state, files)));
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
    let ids = sounds
        .iter()
        .filter_map(|s| match s {
            Sound::TidalTrack(id) => id.parse().ok(),
            _ => None,
        })
        .collect();
    let all = sounds.iter().any(|s| **s == Sound::TidalRandom);
    let draft_track = match &draft {
        Sound::TidalTrack(id) => id.parse().ok(),
        _ => None,
    };
    ctx.tidal.set_selected(ids, all, draft_track);
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
    let mut seen = (tidal.state(), tidal.version());
    ctx.tidal_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            let Some(window) = weak.upgrade() else { return };
            let picker = window.global::<SoundPicker>();
            let now = (tidal.state(), tidal.version());
            if picker.get_tab() != TAB_TIDAL {
                return;
            }
            if now != seen {
                seen = now;
                picker.invoke_refresh();
            }
            picker.invoke_tick();
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
    let has_rows = picker.get_tidal_rows().row_count() > 0;
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
            picker.set_tidal_empty("".into());
            return;
        }
        State::Syncing if has_rows => "",
        State::Syncing => "Lade die Playlist …",
        State::LoggedIn if has_rows => "",
        State::LoggedIn => "Die Playlist ist leer.",
        State::Failed(e) => {
            picker.set_tidal_view(0);
            let why = super::texts::tidal_problem(e);
            picker.set_tidal_empty(format!("{why} Zum Wiederholen den Tab erneut öffnen.").into());
            return;
        }
    };
    picker.set_tidal_view(0);
    picker.set_tidal_code("".into());
    picker.set_tidal_empty(note.into());
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

/// A cover from the card, once (kept in `ctx.covers`); empty while it is
/// not there yet.
fn cover_image(ctx: &Ctx, id: &str) -> Image {
    if let Some(image) = ctx.covers.borrow().get(id) {
        return image.clone();
    }
    let Some(rgb) = crate::tidal::cover_rgb(id) else {
        return Image::default();
    };
    let px = wakey_core::tidal::sync::COVER_PX;
    let image = Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(
        &rgb, px, px,
    ));
    ctx.covers
        .borrow_mut()
        .insert(id.to_string(), image.clone());
    image
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
    fill_list(window, ctx, TAB_FILES);
    fill_list(window, ctx, TAB_TIDAL);
}

/// Fills one tab's list.
fn fill_list(window: &AppWindow, ctx: &Ctx, tab: i32) {
    let picker = window.global::<SoundPicker>();
    let selected = ctx.draft.borrow().sound.clone();
    let progress = ctx.tidal.progress();
    let row = |title: &str, sub: &str, sound: &Sound, kind| SoundRow {
        progress: match (sound, progress) {
            (Sound::TidalTrack(t), Some((id, f))) if *t == id.to_string() => f,
            _ => -1.0,
        },
        title: title.into(),
        sub: sub.into(),
        selected: *sound == selected,
        kind,
        failed: false,
        cover: Image::default(),
    };
    // Each row and the sound it stands for.
    let choices: Vec<(SoundRow, Sound)> = if tab == TAB_FILES {
        // The built-in tone is only the fallback (a sound that cannot be
        // played), never a choice of its own.
        let files = ctx.sound_files.borrow();
        let mut choices = Vec::new();
        // No note below the rows: AirDrop's ends the list (`airdrop_rows`).
        picker.set_empty("".into());
        // Only with a choice: with one file it is that file, without any
        // only the built-in tone. Kept while an alarm being edited has it.
        if files.len() > 1 || (files.len() == 1 && selected == Sound::RandomFile) {
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
        // The playlist, then tracks that left it but an alarm uses; the
        // login or the state shown by `show_state`.
        let tracks = ctx.tidal.listed();
        let mut choices = Vec::new();
        if tracks.len() > 1 || (tracks.len() == 1 && selected == Sound::TidalRandom) {
            choices.push((
                row("Zufällig", "wechselt jeden Tag", &Sound::TidalRandom, 1),
                Sound::TidalRandom,
            ));
        }
        choices.extend(tracks.iter().map(|(t, removed)| {
            let sound = Sound::TidalTrack(t.id.to_string());
            let status = ctx.tidal.track_status(t.id);
            let sub = match status {
                // Failed: the row's icon says it.
                TrackStatus::Ready | TrackStatus::Remote | TrackStatus::Failed => t.artist.clone(),
                TrackStatus::Downloading => format!("{} · lädt …", t.artist),
                TrackStatus::Missing => format!("{} · wird geladen", t.artist),
            };
            // First, so a long artist cannot cut it off.
            let sub = if *removed {
                format!("Nicht mehr in der Playlist · {sub}")
            } else {
                sub
            };
            let mut r = row(&t.title, &sub, &sound, 3);
            r.failed = status == TrackStatus::Failed;
            if let Some(c) = &t.cover {
                r.cover = cover_image(ctx, c);
            }
            (r, sound)
        }));
        choices
    };
    let (mut rows, sounds): (Vec<SoundRow>, Vec<Sound>) = choices.into_iter().unzip();
    // After the choices, so a row's index still finds its sound.
    if tab == TAB_FILES {
        let state = airdrop::state();
        rows.extend(airdrop_rows(&state));
        AIRDROP_SHOWN.with(|s| *s.borrow_mut() = Some((state, airdrop::files_version())));
    }
    // A shorter list may leave the view past its end.
    let layout = window.global::<SoundListLayout>();
    let max = (rows.len() as f32 * layout.get_pitch() - layout.get_height()).max(0.0);
    if tab == TAB_FILES && picker.get_files_scroll() > max {
        picker.set_files_scroll(max);
    } else if tab == TAB_TIDAL && picker.get_tidal_scroll() > max {
        picker.set_tidal_scroll(max);
    }
    if tab == TAB_FILES {
        *ctx.sound_choices.borrow_mut() = sounds;
    } else {
        *ctx.tidal_choices.borrow_mut() = sounds;
    }
    // As many rows as shown: only the rows that differ, in place. A new
    // model makes Slint repaint the whole list, ~40 ms under the list layer
    // while it scrolls (TIDAL's sync every 5 s, AirDrop's state).
    let model = if tab == TAB_FILES {
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
    if tab == TAB_FILES {
        picker.set_rows(rows);
    } else {
        picker.set_tidal_rows(rows);
    }
}

/// The sounds behind the rows of the list shown.
fn choices<'a>(window: &AppWindow, ctx: &'a Ctx) -> std::cell::Ref<'a, Vec<Sound>> {
    if window.global::<SoundPicker>().get_shown_tab() == TAB_TIDAL {
        ctx.tidal_choices.borrow()
    } else {
        ctx.sound_choices.borrow()
    }
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    set_corner_caps(window);
    let picker = window.global::<SoundPicker>();

    picker.on_back(ctx.with(|window, ctx| {
        alarm_page::fill(window, ctx);
        show_page(window, ctx, PAGE_ALARM);
    }));
    picker.on_select_tab(ctx.with_arg(|window, ctx, tab: i32| {
        // The highlight slides now; the list changes when it has arrived
        // (its repaint would stall the slide). Both lists are filled.
        window.global::<SoundPicker>().set_tab(tab);
        tidal_tab(window, ctx, tab == TAB_TIDAL);
        files_tab(window, tab == TAB_FILES);
        let weak = window.as_weak();
        slint::Timer::single_shot(Duration::from_millis(200), move || {
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
        // A failed download cannot be the alarm's sound.
        if let Sound::TidalTrack(id) = &sound
            && id
                .parse()
                .is_ok_and(|id| ctx.tidal.track_status(id) == TrackStatus::Failed)
        {
            return;
        }
        // A TIDAL track not on the card: downloaded next, before others.
        if let Sound::TidalTrack(id) = &sound
            && let Ok(id) = id.parse()
        {
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
    picker.on_refresh(ctx.with(|window, ctx| {
        if window.global::<SoundPicker>().get_tab() == TAB_TIDAL {
            fill_list(window, ctx, TAB_TIDAL);
            show_tidal(window, ctx);
        }
    }));
    picker.on_tick(ctx.with(|window, ctx| {
        // Only the downloading row's bar, in place (no list rebuild).
        let Some((id, fraction)) = ctx.tidal.progress() else {
            return;
        };
        let want = Sound::TidalTrack(id.to_string());
        let Some(i) = ctx.tidal_choices.borrow().iter().position(|s| *s == want) else {
            return;
        };
        let rows = window.global::<SoundPicker>().get_tidal_rows();
        if let Some(mut row) = rows.row_data(i)
            && (row.progress - fraction).abs() >= 0.005
        {
            row.progress = fraction;
            rows.set_row_data(i, row);
        }
    }));
    picker.on_airdrop_tick(ctx.with(airdrop_tick));
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

/// Corner radius of the list card (`SoundPage`'s card: 28 px).
const CORNER_RADIUS: usize = 28;

/// The caps over the list card's corners: the page background outside the
/// rounding, with the weights the list layer blends ([`corner_alpha`]).
fn set_corner_caps(window: &AppWindow) {
    let bg = window.global::<Theme>().get_background();
    let r = CORNER_RADIUS;
    let alpha = crate::ui::list_layer::corner_alpha(r);
    let cap = |mirror_x: bool, mirror_y: bool| {
        let mut buf = SharedPixelBuffer::<slint::Rgba8Pixel>::new(r as u32, r as u32);
        for (k, p) in buf.make_mut_slice().iter_mut().enumerate() {
            let (x, y) = (k % r, k / r);
            let (i, j) = (
                if mirror_x { r - 1 - x } else { x },
                if mirror_y { r - 1 - y } else { y },
            );
            let a = alpha[j * r + i];
            *p = slint::Rgba8Pixel {
                r: bg.red(),
                g: bg.green(),
                b: bg.blue(),
                a: ((u32::from(a) * 255 + 8) / 16) as u8,
            };
        }
        Image::from_rgba8(buf)
    };
    let picker = window.global::<SoundPicker>();
    picker.set_corner_tl(cap(false, false));
    picker.set_corner_tr(cap(true, false));
    picker.set_corner_bl(cap(false, true));
    picker.set_corner_br(cap(true, true));
}

/// The list layer for the sound page's list (the tab shown), drawing into
/// `shared` (the alarm list's buffers: never on screen together). Call
/// after `AppWindow` was created (its offscreen row window is the next one,
/// see `ui::platform`).
pub fn layer(window: &AppWindow, shared: Buffers) -> ListLayer<SoundRows> {
    let l = window.global::<SoundListLayout>();
    let px = |v: f32| v.round() as usize;
    let layout = Layout {
        x: px(l.get_x()),
        y: px(l.get_y()),
        width: px(l.get_width()),
        height: px(l.get_height()),
        row_h: px(l.get_pitch()),
        // A whole playlist rendered ahead (as much as the shared ring
        // holds: 36 × 112 × 660 px of the alarm list's 16 × 162 × 1200).
        slots: 36,
        slop: l.get_slop(),
        background: crate::ui::rgb565(window.global::<Theme>().get_card()),
        page_draws_still: true,
        swipe: false,
        // The card's (`SoundPage`): the page around it shows outside.
        corners: Some((
            CORNER_RADIUS,
            crate::ui::rgb565(window.global::<Theme>().get_background()),
        )),
    };
    let row_window = platform::offscreen_window(layout.width as u32, layout.row_h as u32);
    let view = AppWindow::new().expect("sound row view");
    view.set_page(PAGE_SOUND_ROW);
    view.show().expect("show sound row view");
    let rows = SoundRows {
        window: window.as_weak(),
        view,
    };
    ListLayer::new_in(layout, row_window, rows, Some(shared))
}

/// The sound page's list as the list layer sees it: the rows of the tab
/// shown, drawn one at a time by `view`, an `AppWindow` showing only a row.
pub struct SoundRows {
    window: Weak<AppWindow>,
    view: AppWindow,
}

impl SoundRows {
    fn with_picker<T>(&self, f: impl FnOnce(SoundPicker<'_>) -> T) -> Option<T> {
        self.window.upgrade().map(|w| f(w.global::<SoundPicker>()))
    }

    /// The tab shown and its rows.
    fn list(&self) -> Option<(i32, ModelRc<SoundRow>)> {
        self.with_picker(|p| {
            let tab = p.get_shown_tab();
            let rows = if tab == TAB_TIDAL {
                p.get_tidal_rows()
            } else {
                p.get_rows()
            };
            (tab, rows)
        })
    }

    fn row(&self, i: usize) -> Option<(i32, SoundRow)> {
        let (tab, rows) = self.list()?;
        rows.row_data(i).map(|r| (tab, r))
    }
}

impl ListSource for SoundRows {
    fn shown(&self) -> bool {
        self.window.upgrade().is_some_and(|w| {
            let p = w.global::<SoundPicker>();
            // Not while the TIDAL tab shows the login.
            w.get_page() == PAGE_SOUND
                && !(p.get_shown_tab() == TAB_TIDAL && p.get_tidal_view() == 1)
        })
    }

    fn rows(&self) -> usize {
        self.list().map_or(0, |(_, rows)| rows.row_count())
    }

    fn stamp(&self, i: usize) -> u64 {
        let Some((tab, r)) = self.row(i) else {
            return 0;
        };
        let mut h = std::hash::DefaultHasher::new();
        let cover = r.cover.size();
        (
            tab,
            r.title.as_str(),
            r.sub.as_str(),
            r.selected,
            r.kind,
            r.failed,
            (r.progress * 1000.0) as i32,
            cover.width,
            cover.height,
        )
            .hash(&mut h);
        h.finish()
    }

    fn prepare(&mut self, i: usize) {
        if let Some((tab, r)) = self.row(i) {
            self.view.set_sound_row(r);
            self.view.set_sound_row_tab(tab);
        }
    }

    fn prepare_swipe(&mut self, _: usize, _: SwipePart) {}

    fn offset(&self) -> f32 {
        self.with_picker(|p| {
            if p.get_shown_tab() == TAB_TIDAL {
                p.get_tidal_scroll()
            } else {
                p.get_files_scroll()
            }
        })
        .unwrap_or(0.0)
    }

    fn layer_started(&mut self) {
        // Rows in view with scrolling text are rendered again with it where
        // the page shows it now (`changes_on_scroll`).
        if let Some(ms) = self.with_picker(|p| {
            p.set_layer_active(true);
            p.get_marquee_ms()
        }) {
            self.view.set_sound_row_ms(ms);
        }
    }

    fn changes_on_scroll(&mut self, i: usize) -> bool {
        // Only a row whose second line scrolls; the others look the same at
        // any time (re-rendering all visible rows cost the first frame
        // ~20 ms, a visible bump at the start of every scroll).
        self.prepare(i);
        self.view.get_sound_row_scrolls()
    }

    fn scroll_ended(&mut self, offset: f32) {
        self.with_picker(|p| {
            if p.get_shown_tab() == TAB_TIDAL {
                p.set_tidal_scroll(offset);
            } else {
                p.set_files_scroll(offset);
            }
            p.set_layer_active(false);
        });
    }

    fn swipe_ended(&mut self, _: usize, _: SwipeEnd) {}
}
