//! The alarm list's view model (`AlarmListPage` in
//! `ui/wakey/alarm_list.slint`): one row per alarm, sorted by time, the
//! next one marked. Opens from the alarm line on the clock page.

use super::alarm::AlarmClock;
use super::screens::{AlarmList, AlarmRow, AppWindow, Clock};
use super::{Ctx, PAGE_CLOCK, PAGE_LIST, alarm_page};
use slint::{ComponentHandle, ModelRc, VecModel};
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
            days: ModelRc::new(VecModel::from(a.weekdays.map(|d| d != 0).to_vec())),
            enabled: a.enabled,
            next: next == Some(a.id),
        })
        .collect();
    window
        .global::<AlarmList>()
        .set_rows(ModelRc::new(VecModel::from(rows)));
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let list = window.global::<AlarmList>();

    window
        .global::<Clock>()
        .on_open_alarm(ctx.with(|window, ctx| {
            refresh(window, &ctx.alarm.borrow());
            window.set_page(PAGE_LIST);
        }));
    list.on_back(ctx.with(|window, _| window.set_page(PAGE_CLOCK)));
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
