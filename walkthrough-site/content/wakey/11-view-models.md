+++
title = "View models: `Ctx`, callbacks and timers"
description = "How Rust fills the pages and reacts to them, with one shared context and no global state."
weight = 11

[extra]
map = ["app"]
map_note = "We stay in `app`, the top layer. Each page of the user interface has a small module here that fills the page and answers its callbacks."
goals = [
  "how a dozen callbacks share the same state without a single `static`",
  "why `Ctx` keeps a weak handle to the window and not a strong one",
  "how `Ctx::with` turns a plain function into a callback for Slint",
  "why the view models set a property only when its value has changed",
]
takeaways = [
  "The state of the user interface lives in one `Ctx`, shared through an `Rc`. It needs no lock, because everything that touches it runs on the `app` thread.",
  "Each field that changes is a `RefCell` of its own. A `RefCell` checks its borrows while the program runs, so a handler ends one borrow before it starts the next.",
  "`Ctx::with`, `with_arg` and `with_args` take a plain `fn` pointer and return a closure. The closure holds the context and upgrades the weak window handle on every call.",
  "A Slint `Timer` stops when it is dropped, so `app::app` binds each one to a name such as `_idle` for as long as the task runs.",
  "View models write to Slint only what differs from what is already there. The code's comments give the reason: Slint then repaints only the parts that changed.",
]
+++

The [previous chapter](@/wakey/10-slint.md) introduced Slint and ended with a window of type `AppWindow`, generated from the `.slint` files. That window has properties that Rust can set and callbacks that Rust can answer. This chapter is about the Rust code that does both. The firmware calls it a page's *view model*: a small module per page that fills the page with data and reacts when the user touches something.

When the user flips a switch, the callback has to reach the alarms, and often the window as well. We start with how the callbacks get hold of both.

## One context for all pages

A Slint callback is a closure that Slint stores and calls later, so it cannot borrow local variables of `app::app`. A common way out is a `static`, which every function can reach. This firmware does not use one here. It collects what the pages share in one struct. The highlighted lines are the fields we will meet again in this chapter.

{% <code file="firmware/src/app/mod.rs" lines="102-121" hl="104-105 109 112 114"> %}{% raw %}
/// What the settings pages share.
pub struct Ctx {
    window: slint::Weak<AppWindow>,
    alarm: Rc<RefCell<AlarmClock>>,
    /// Where the clock stands, for the sky.
    location: wakey_core::sky::Location,
    /// The alarm being edited (alarm, sound and name pages).
    draft: RefCell<AlarmConfig>,
    /// The alarm as saved when its page opened (`None`: a new alarm);
    /// saving only after a change.
    original: RefCell<Option<AlarmConfig>>,
    /// The name being typed.
    name: RefCell<String>,
// ... @121
}
{% endraw %}{% </code> %}

`app::app` creates exactly one `Ctx` and puts it into an `Rc`, the reference-counted pointer for a single thread. Every callback gets a clone of that `Rc`. An `Rc` is enough, and no `Arc` or `Mutex` is needed, because Slint calls all callbacks on the `app` thread.

An `Rc` only gives out shared references, so a callback sees `&Ctx` and never `&mut Ctx`. That is why each field that changes is wrapped in a `RefCell` of its own, so that a handler can borrow one field while another is in use. The fields `draft`, `original` and `name` belong to the page that edits one alarm, and [chapter 15](@/wakey/15-state-on-device.md) explains them together with `AlarmClock`. `alarm` has an `Rc` of its own around its `RefCell`, because `app::app` also hands the same `AlarmClock` to code outside `Ctx`.

The first field, `window`, is the handle to the Slint window, and its type is `slint::Weak`. An `AppWindow` is a reference-counted handle, much like an `Rc`. `as_weak` gives a handle that does not keep the window alive, and `upgrade` turns it back into a real one, or into `None` if the window is gone.

The weak handle is there because of who owns whom. The window stores the callbacks. Each callback, as we will see next, owns a clone of the `Rc<Ctx>`. If `Ctx` owned the window in turn, the three would form a ring.

{% <pitfall title="A ring of strong handles is never freed" id="ctx-weak-window"> %}
Reference counting frees a value when its count reaches zero. In a ring, every member is held by the one before it, so no count ever reaches zero, even when nothing outside the ring points to it any more.

Here the ring would be window, callback, `Ctx`, window. The weak handle in `Ctx` breaks it at one place.
{% </pitfall> %}

## From a function to a callback

`Ctx::with` takes a function and returns the closure that Slint will store. Watch the type of `f` in the highlighted signature.

{% <code file="firmware/src/app/mod.rs" lines="123-132" hl="125-126 128"> %}{% raw %}
impl Ctx {
    /// A callback that runs `f` with the window and the context.
    fn with(self: &Rc<Self>, f: fn(&AppWindow, &Ctx)) -> impl Fn() + 'static {
        let ctx = self.clone();
        move || {
            if let Some(window) = ctx.window.upgrade() {
                f(&window, &ctx);
            }
        }
    }
{% endraw %}{% </code> %}

The receiver in line 125 is `self: &Rc<Self>`, so line 126 can clone the `Rc`. The closure in lines 127 to 131 owns this clone. That is what makes it `'static`: it borrows nothing, so Slint may keep it for as long as it likes.

{% <tip title="`self: &Rc<Self>` as a receiver" id="ctx-rc-self-receiver"> %}
A method usually takes `&self`, and from a `&Ctx` there is no way back to the `Rc` that holds it. Rust also allows a few smart pointers as the type of `self`, and `&Rc<Self>` is one of them.

Such a method can only be called on an `Rc<Ctx>`, and inside it `self.clone()` makes a new owner of the same context. That is what a closure needs when it has to outlive the call.
{% </tip> %}

When Slint calls the closure, line 128 upgrades the weak handle. If the window still exists, the closure calls `f` with a reference to the window and a reference to the context.

The parameter `f` is a `fn` pointer, the first of the three ways to pass a function from [chapter 2](@/wakey/02-layers.md#passing-functions). A `fn` pointer accepts a named function, or a closure that captures nothing. The effect is that a handler has exactly two sources of data, its arguments `window` and `ctx`. A named function with the right signature can be passed directly, as in `editor.on_save(ctx.with(save))` on the alarm page.

`with_arg` and `with_args` are the same method for callbacks that carry one or two arguments from Slint. Here is `with_args` in use in the alarm list.

{% <code file="firmware/src/app/alarm_list.rs" lines="79-85" hl="80 84"> %}{% raw %}
    list.on_toggle(ctx.with_args(|window, ctx, id: i32, on: bool| {
        let result = ctx.alarm.borrow_mut().set_enabled(id as u32, on);
        if let Err(e) = result {
            log::error!("alarm list: saving: {e}");
        }
        refresh(window, &ctx.alarm.borrow());
    }));
{% endraw %}{% </code> %}

These lines are from `alarm_list::start`, which `app::app` calls once. `list` is the Slint global `AlarmList`, and `on_toggle` is the setter that Slint generated for its callback `toggle(int, bool)`. The inner `ctx` is a parameter of the closure, not a capture, so the closure captures nothing and fits a `fn` pointer.

The `toggle` handler runs when the user flips an alarm's switch. Line 80 borrows the `AlarmClock` mutably and switches the alarm on or off. Line 84 borrows it again, this time shared, and passes it to `refresh`, which rebuilds the rows of the list.

{% <pitfall title="A `RefCell` checks its borrows at run time" id="ctx-refcell-two-statements"> %}
`borrow_mut` panics if any other borrow of the same `RefCell` is still alive. The compiler cannot warn about that, because the check happens while the program runs.

Lines 80 and 84 are safe because they are two statements. The guard from `borrow_mut` in line 80 has no name, so it is dropped at the end of that statement, as [chapter 7](@/wakey/07-sharing-hardware.md#temporary-guard) showed for a `MutexGuard`.
{% </pitfall> %}

The diagram shows the whole path of this one callback.

{% <diagram caption="One `toggle` callback. Slint only knows the closure. The closure supplies the window and the context, and the handler does the work."> %}
sequenceDiagram
    participant S as Slint
    participant C as closure from with_args
    participant F as toggle handler
    participant A as ctx.alarm
    participant W as AppWindow
    S->>C: toggle(id, on)
    C->>W: upgrade the weak handle
    W-->>C: Some(window)
    C->>F: f(window, ctx, id, on)
    F->>A: borrow_mut, set_enabled
    A-->>F: result, borrow ends
    F->>A: borrow
    F->>W: refresh sets the changed rows
    F-->>S: return
{% </diagram> %}

## Filling a page, and only what changed

The other half of a view model writes to the page. `refresh` first builds a fresh `Vec<AlarmRow>` from the alarms. A Slint list reads its rows from a *model*, here a `VecModel`. The excerpt is the end of the function, where it decides how to hand the new rows over.

{% <code file="firmware/src/app/alarm_list.rs" lines="40-55" hl="44 49-51"> %}{% raw %}
    // Same alarms in the same order (on/off toggled, the next one moved):
    // update only the rows that changed, in place, so their switches
    // animate and the others are not repainted.
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
{% endraw %}{% </code> %}

The last line is the plain way: wrap the new rows in a new model and replace the old one. The `if` before it is a faster path for the common case. Line 44 gets the model that is already in the property back as a `VecModel`. Lines 45 and 46 check that it has the same number of rows with the same ids in the same order. If so, lines 48 to 52 compare row by row and write only the rows that differ.

The comment gives the two reasons. The switch of a row that is updated in place animates. And the other rows are not repainted. The comparison in line 49 is needed for that, because `set_row_data` of a `VecModel` reports the row as changed whatever the value is (`model.rs` in the copy of Slint's core in `firmware/vendor`).

The clock screen follows the same rule for single properties. Its macro `set_changed!` takes a list of getter, setter and value, and calls the setter only where the getter returns something else. The module's comment gives the reason: only the fields that changed are set, so Slint repaints only those, and a seconds tick redraws about 110 rows of the panel and not the whole screen.

One property gets special care, the window's `page`. Only `show_page` in `app/mod.rs` sets it for the pages the user moves between. Before it does, it tells the TIDAL service whether an alarm is being edited, cleans up after the sound page when that page is left, and requests a slide animation in the right direction. Because every handler goes through this one function, none of these steps can be forgotten.

## Timers that live as long as their binding

Some work has no callback to start it. For that, Slint has timers, which the render loop fires on the `app` thread ([chapter 12](@/wakey/12-touch-in.md)). `close_when_idle` shows the pattern: it creates a `Timer`, starts it with a closure from `ctx.with` that runs every five seconds, and returns the timer.

The return value matters, because a Slint `Timer` stops when it is dropped. So `app::app` binds what its four start functions return to `_idle`, `_dim`, `_power` and `_ticker`. A bare `_` would drop each timer at once, as the pitfall in [chapter 6](@/wakey/06-board.md#underscore-binding) explains.

`_ticker` comes from `clock_screen::start`, which updates the clock every second. Here is how it lines the updates up with the full second.

{% <code file="firmware/src/app/clock_screen.rs" lines="55-71" hl="61 64-66 69"> %}{% raw %}
    let tick = move || {
        alarm.borrow_mut().tick();
        refresh();
    };
    tick();
    // A one-shot timer to the next full second, then a repeating one.
    let every_second = Rc::new(Timer::default());
    let align = Timer::default();
    let repeat = every_second.clone();
    align.start(
        TimerMode::SingleShot,
        clock::until_next_second() + Duration::from_millis(2),
        move || {
            tick();
            repeat.start(TimerMode::Repeated, Duration::from_secs(1), tick.clone());
        },
    );
{% endraw %}{% </code> %}

`tick` advances the alarm and then calls `refresh`, a closure built a few lines earlier that redraws the clock. `refresh` is written by hand and not with `ctx.with`, because it captures the speaker and Wi-Fi handles, and a `fn` pointer cannot capture.

Line 59 fills the screen once at start-up. Then two timers work together. `align` fires once, 2 ms after the system clock's next full second. Its closure ticks and, in line 69, starts the second timer, which repeats every second from that moment. The second timer is in an `Rc` because two places need it: the closure that starts it, and the `Ticker` struct that `start` returns, which keeps both timers alive.

For our two questions, this chapter answers the second one for the user interface. Its state lives in one `Ctx` on the `app` thread, and the Slint properties are a copy of that state, written only where it changed. It also adds a way for parts to work together: the pages and the Rust code meet in closures that Slint calls. What calls Slint, so that a touch arrives there at all, is the render loop, and the [next chapter](@/wakey/12-touch-in.md) begins with it.
