//! The alarm list's view model (`AlarmListPage` in
//! `ui/wakey/alarm_list.slint`): one row per alarm, sorted by time, the
//! next one marked. Opens from the alarm line on the clock page.
//!
//! Scrolling is the list layer's ([`layer`]): it copies rows rendered by a
//! second `AppWindow` in an offscreen window (`Page::AlarmListRow`) while
//! the list moves, and the page draws the still list.

use super::alarm::AlarmClock;
use super::screens::{AlarmList, AlarmListLayout, AlarmRow, AppWindow, Clock, Page, Theme};
use super::{Ctx, alarm_page, show_page};
use crate::ui::list_layer::{Layout, ListLayer, ListSource, SwipeEnd, SwipePart};
use crate::ui::platform;
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use wakey_core::alarm::{self, AlarmConfig};

/// Fills the rows from the current alarms.
pub fn refresh(window: &AppWindow, alarm: &AlarmClock) {
    let next = alarm.next_id();
    let rows: Vec<AlarmRow> = alarm::sorted(alarm.alarms())
        .iter()
        .map(|a| AlarmRow {
            id: a.id as i32,
            time: format!("{:02}:{:02}", a.hour, a.minute).into(),
            name: a.name.as_str().into(),
            days: a.weekday_bits().into(),
            enabled: a.enabled,
            next: next == Some(a.id),
        })
        .collect();
    // Same alarms in the same order (on/off toggled, the next one moved):
    // update only the rows that changed, in place, so their switches
    // animate and the others are not repainted.
    let list = window.global::<AlarmList>();
    // Fewer rows may leave the list scrolled past its new end.
    let layout = window.global::<AlarmListLayout>();
    let max_scroll = (rows.len() as f32 * layout.get_pitch() - layout.get_height()).max(0.0);
    if list.get_scroll() > max_scroll {
        list.set_scroll(max_scroll);
    }
    let current = list.get_rows();
    if let Some(model) = current.as_any().downcast_ref::<VecModel<AlarmRow>>()
        && model.row_count() == rows.len()
        && model.iter().zip(&rows).all(|(a, b)| a.id == b.id)
    {
        for (i, row) in rows.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&row) {
                model.set_row_data(i, row);
            }
        }
        return;
    }
    list.set_rows(ModelRc::new(VecModel::from(rows)));
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let list = window.global::<AlarmList>();

    window
        .global::<Clock>()
        .on_open_alarm(ctx.with(|window, ctx| {
            refresh(window, &ctx.alarm.borrow());
            window.global::<AlarmList>().set_scroll(0.0);
            show_page(window, ctx, Page::AlarmList);
        }));
    list.on_back(ctx.with(|window, ctx| show_page(window, ctx, Page::Clock)));
    list.on_add(ctx.with(|window, ctx| {
        alarm_page::open(window, ctx, AlarmConfig::default());
    }));
    list.on_open(ctx.with_arg(|window, ctx, id: i32| {
        let cfg = ctx.alarm.borrow().get(id as u32).cloned();
        if let Some(cfg) = cfg {
            alarm_page::open(window, ctx, cfg);
        }
    }));
    list.on_toggle(ctx.with_args(|window, ctx, id: i32, on: bool| {
        let result = ctx.alarm.borrow_mut().set_enabled(id as u32, on);
        if let Err(e) = result {
            log::error!("alarm list: saving: {e}");
        }
        refresh(window, &ctx.alarm.borrow());
    }));
    list.on_delete(ctx.with_arg(|window, ctx, id: i32| {
        let result = ctx.alarm.borrow_mut().delete(id as u32);
        if let Err(e) = result {
            log::error!("alarm list: saving: {e}");
        }
        refresh(window, &ctx.alarm.borrow());
    }));
}

/// The list layer for the alarm list. Call after `AppWindow` was created
/// (its offscreen row window is the next one, see `ui::platform`).
pub fn layer(window: &AppWindow) -> ListLayer<Rows> {
    let l = window.global::<AlarmListLayout>();
    let px = |v: f32| v.round() as usize;
    let layout = Layout {
        x: px(l.get_x()),
        y: px(l.get_y()),
        width: px(l.get_width()),
        height: px(l.get_height()),
        row_h: px(l.get_pitch()),
        // All rows of a typical list: no rendering while scrolling.
        slots: 16,
        slop: l.get_slop(),
        background: crate::ui::rgb565(window.global::<Theme>().get_background()),
        page_draws_still: true,
        swipe: true,
        row_radius: px(l.get_row_radius()),
        corners: None,
    };
    let row_window = platform::offscreen_window(layout.width as u32, layout.row_h as u32);
    let view = AppWindow::new().expect("alarm row view");
    view.set_page(Page::AlarmListRow);
    view.show().expect("show alarm row view");
    let rows = Rows {
        window: window.as_weak(),
        view,
    };
    ListLayer::new(layout, row_window, rows)
}

/// The alarm list as the list layer sees it: the rows in the `AlarmList`
/// global, drawn one at a time by `view`, an `AppWindow` showing only a row.
pub struct Rows {
    window: Weak<AppWindow>,
    view: AppWindow,
}

impl Rows {
    fn with_list<T>(&self, f: impl FnOnce(AlarmList<'_>) -> T) -> Option<T> {
        self.window.upgrade().map(|w| f(w.global::<AlarmList>()))
    }

    fn row(&self, i: usize) -> Option<AlarmRow> {
        self.with_list(|l| l.get_rows().row_data(i)).flatten()
    }
}

impl ListSource for Rows {
    fn shown(&self) -> bool {
        self.window
            .upgrade()
            .is_some_and(|w| w.get_page() == Page::AlarmList)
    }

    fn rows(&self) -> usize {
        self.with_list(|l| l.get_rows().row_count()).unwrap_or(0)
    }

    fn stamp(&self, i: usize) -> u64 {
        let Some(r) = self.row(i) else { return 0 };
        let mut h = std::hash::DefaultHasher::new();
        (
            r.id,
            r.time.as_str(),
            r.name.as_str(),
            r.days,
            r.enabled,
            r.next,
        )
            .hash(&mut h);
        h.finish()
    }

    fn prepare(&mut self, i: usize) {
        if let Some(r) = self.row(i) {
            self.view.set_list_row(r);
            self.view.set_list_row_dx(0.0);
            self.view.set_list_row_highlighted(false);
            self.view.set_list_row_underlay(false);
        }
    }

    fn prepare_swipe(&mut self, i: usize, part: SwipePart) {
        let Some(r) = self.row(i) else { return };
        self.view.set_list_row(r);
        match part {
            SwipePart::Content {
                highlighted,
                underlay,
            } => {
                self.view.set_list_row_dx(0.0);
                self.view.set_list_row_highlighted(highlighted);
                self.view.set_list_row_underlay(underlay);
            }
            SwipePart::Underlay => {
                // The row content moved out entirely; the button keeps its
                // rounded right corners (no square of red behind).
                self.view.set_list_row_dx(-10_000.0);
                self.view.set_list_row_highlighted(false);
                self.view.set_list_row_underlay(false);
            }
        }
    }

    fn offset(&self) -> f32 {
        self.with_list(|l| l.get_scroll()).unwrap_or(0.0)
    }

    fn layer_started(&mut self) {
        self.with_list(|l| l.set_layer_active(true));
    }

    fn scroll_ended(&mut self, offset: f32) {
        self.with_list(|l| {
            l.set_scroll(offset);
            l.set_layer_active(false);
        });
    }

    fn swipe_ended(&mut self, i: usize, end: SwipeEnd) {
        let id = self.row(i).map(|r| r.id);
        self.with_list(|l| {
            l.set_layer_active(false);
            if end == SwipeEnd::Delete
                && let Some(id) = id
            {
                // The page's own delete (`app::alarm_list::start`).
                l.invoke_delete(id);
            }
        });
    }
}
