//! The name page's view model (`NamePage` in `ui/wakey/name_page.slint`): an
//! on-screen keyboard for the draft alarm's name. The keys only report;
//! the text is edited here (Slint has no way to remove a character).

use super::screens::{AppWindow, NameEditor};
use super::{Ctx, PAGE_ALARM, PAGE_NAME, alarm_page, show_page};
use slint::ComponentHandle;
use std::rc::Rc;
use wakey_core::alarm::NAME_MAX;

/// Shows the keyboard for the draft's name.
pub fn open(window: &AppWindow, ctx: &Ctx) {
    let name = ctx.draft.borrow().name.clone();
    let editor = window.global::<NameEditor>();
    editor.set_shift(name.is_empty());
    editor.set_numbers(false);
    editor.set_text(name.as_str().into());
    *ctx.name.borrow_mut() = name;
    show_page(window, PAGE_NAME);
}

fn show(window: &AppWindow, text: &str, shift: bool) {
    let editor = window.global::<NameEditor>();
    editor.set_text(text.into());
    editor.set_shift(shift);
}

/// Connects the page's callbacks.
pub fn start(window: &AppWindow, ctx: &Rc<Ctx>) {
    let editor = window.global::<NameEditor>();

    editor.on_key(ctx.with_arg(|window, ctx, key: slint::SharedString| {
        let mut name = ctx.name.borrow_mut();
        if name.chars().count() < NAME_MAX {
            name.push_str(&key);
        }
        // A capital at the start only.
        show(window, &name, false);
    }));
    editor.on_backspace(ctx.with(|window, ctx| {
        let mut name = ctx.name.borrow_mut();
        name.pop();
        show(window, &name, name.is_empty());
    }));
    editor.on_toggle_shift(ctx.with(|window, ctx| {
        let shift = !window.global::<NameEditor>().get_shift();
        show(window, &ctx.name.borrow(), shift);
    }));
    editor.on_done(ctx.with(|window, ctx| {
        let name = ctx.name.borrow().trim().to_string();
        ctx.draft.borrow_mut().name = name;
        alarm_page::fill(window, ctx);
        show_page(window, PAGE_ALARM);
    }));
    editor.on_cancel(ctx.with(|window, _| show_page(window, PAGE_ALARM)));
}
