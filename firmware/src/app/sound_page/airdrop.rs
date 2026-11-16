//! The Files tab and AirDrop: while the tab shows, the C6 is the AirDrop
//! receiver ([`crate::airdrop`]). Its note ends the list, a file arriving
//! is a row above it, and a file received fills the list again.

use super::fill_list;
use crate::airdrop::{self, State};
use crate::app::Ctx;
use crate::app::screens::{AppWindow, SoundPicker, SoundRow, SoundRowKind, SoundTab};
use crate::sounds;
use slint::{ComponentHandle, Image, Model, TimerMode};
use std::time::Duration;

/// The Files tab opened or closed: AirDrop on or off, and followed.
pub(super) fn files_tab(window: &AppWindow, ctx: &Ctx, open: bool) {
    airdrop::set_wanted(open);
    let timer = &ctx.sound.airdrop_timer;
    if !open {
        timer.stop();
        return;
    }
    let weak = window.as_weak();
    timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        if let Some(window) = weak.upgrade() {
            window.global::<SoundPicker>().invoke_airdrop_tick();
        }
    });
}

/// The Files tab's last rows: a file arriving, then AirDrop's note. Shown
/// from now on (`tick` compares with it).
pub(super) fn rows(ctx: &Ctx) -> Vec<SoundRow> {
    let state = airdrop::state();
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
        State::Off | State::Starting => "AirDrop startet …",
        State::Ready | State::Receiving(_) => "AirDrop bereit für neue Klänge",
        State::Failed => "AirDrop nicht verfügbar",
    };
    let mut rows = Vec::new();
    if let State::Receiving(p) = &state {
        rows.push(row(
            p.name.clone(),
            "wird empfangen …".into(),
            SoundRowKind::AirdropFile,
            fraction(p),
        ));
    }
    rows.push(row(
        note.into(),
        String::new(),
        SoundRowKind::AirdropNote,
        -1.0,
    ));
    *ctx.sound.airdrop_shown.borrow_mut() = Some((state, airdrop::files_version()));
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
pub(super) fn tick(window: &AppWindow, ctx: &Ctx) {
    let state = airdrop::state();
    let files = airdrop::files_version();
    let shown = ctx.sound.airdrop_shown.borrow().clone();
    let same_kind = |a: &State, b: &State| match (a, b) {
        (State::Receiving(x), State::Receiving(y)) => x.name == y.name,
        (a, b) => a == b,
    };
    match shown {
        Some((s, f)) if f == files && same_kind(&s, &state) => {
            if s == state {
                return;
            }
            // The same file further on: its row in place.
            if let State::Receiving(p) = &state {
                let rows = window.global::<SoundPicker>().get_rows();
                let i = rows.row_count().saturating_sub(2);
                // As TIDAL's bar: only a visible step repaints the row.
                if let Some(mut row) = rows.row_data(i)
                    && row.kind == SoundRowKind::AirdropFile
                    && (row.progress - fraction(p)).abs() >= 0.005
                {
                    row.progress = fraction(p);
                    rows.set_row_data(i, row);
                }
            }
            *ctx.sound.airdrop_shown.borrow_mut() = Some((state, files));
        }
        Some((_, f)) if f != files => {
            *ctx.sound.files.borrow_mut() = sounds::list();
            fill_list(window, ctx, SoundTab::Files);
        }
        _ => fill_list(window, ctx, SoundTab::Files),
    }
}
