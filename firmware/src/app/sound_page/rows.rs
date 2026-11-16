//! The sound list as the list layer scrolls it: the rows of the tab shown,
//! rendered one at a time by an offscreen `AppWindow`, in a card whose
//! rounded corners the page and the layer cap alike.

use super::TAB_TIDAL;
use crate::app::PAGE_SOUND_ROW;
use crate::app::screens::{AppWindow, SoundListLayout, SoundPicker, SoundRow, Theme};
use crate::ui::list_layer::{Buffers, Layout, ListLayer, ListSource, SwipeEnd, SwipePart};
use crate::ui::platform;
use slint::{ComponentHandle, Image, Model, ModelRc, SharedPixelBuffer, Weak};
use std::hash::{Hash, Hasher};

/// Corner radius of the list card (`SoundPage`'s card: 28 px).
const CORNER_RADIUS: usize = 28;

/// The caps over the list card's corners: the page background outside the
/// rounding, with the weights the list layer blends
/// ([`corner_alpha`](crate::ui::list_layer::corner_alpha)).
pub(super) fn set_corner_caps(window: &AppWindow) {
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
            w.get_page() == crate::app::PAGE_SOUND
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
