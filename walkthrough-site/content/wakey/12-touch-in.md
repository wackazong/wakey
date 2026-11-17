+++
title = "The render loop: touch in"
description = "One `async` function that is the UI thread: its stages, and how a touch finds its way to the list or to Slint."
weight = 12

[extra]
map = ["ui"]
map_note = "`ui` is a service below `app`. Its render loop is the one place where touch events, Slint's timers and the display meet."
goals = [
  "the stages of one pass through `render_loop::run`",
  "how a touch event travels from the queue to a `FrameHook` or to Slint, and what `Route` decides",
  "why `hook.as_mut()` appears every time the loop uses its hook",
  "how the loop waits for a timer and a touch at once, and how it carries out sleep and wake",
]
takeaways = [
  "`render_loop::run` is the UI thread's one loop. Each pass handles touch, then timers, then draws a frame, and then waits if nothing moves.",
  "Every touch event is offered to the hook first. The hook answers with a `Route`, an enum of three variants, and the loop's `match` must handle each one.",
  "`hook` is an `Option<&mut dyn FrameHook>`. `as_mut()` borrows it for one use, so the loop still has it on the next pass.",
  "The idle wait is a `select` between a timer and `EVENTS.receive()`. A received event has left the queue, so the loop keeps it in `woken_by`.",
  "Sleep and wake are requests in an atomic. Nothing wakes the loop for them, so it never waits longer than 100 ms.",
]
+++

The [previous chapter](@/wakey/11-view-models.md) showed how Rust code fills the pages and reacts to callbacks. All of that code runs because something calls Slint again and again. That something is `render_loop::run` in `firmware/src/ui/render_loop.rs`, the function on which the `app` task ends in [chapter 4](@/wakey/04-startup.md). It never returns.

Two chapters share this function. This one is the input half: the shape of the loop, where touch events go, timers, the wait when nothing happens, and sleep. [Chapter 13](@/wakey/13-pixels-out.md) is the output half, which is drawing.

## One pass through the loop

Before we read code, here is the order of one pass. The loop records where it is by storing a `Stage` in an atomic: `Touch`, `Timers`, `BackBuffer`, `Render`, `Present` or `Sleep`. A seventh value, `Start`, is what the atomic holds before the first pass. The `ui-watch` thread from [chapter 5](@/wakey/05-threads.md) reads that atomic when the loop has stopped, so that the log says in which stage.

{% <diagram caption="One pass through `render_loop::run`. Drawing is the three steps from `back_buffer` to `present`, and chapter 13 covers them."> %}
sequenceDiagram
    participant T as touch thread
    participant L as render loop
    participant H as hook
    participant S as Slint
    participant D as display
    T->>L: events wait in EVENTS
    Note over L: take a sleep or wake request
    L->>H: pointer(event)
    H-->>L: a Route
    L->>S: dispatch, if the Route is Slint
    L->>S: update timers and animations
    Note over L: if a page change was requested, slide and start again
    L->>D: back_buffer, waits for the last flip
    L->>S: render what changed
    L->>D: present
    Note over L: if nothing moves, wait for a timer or a touch
{% </diagram> %}

## Where the events come from

The events are produced on another thread. [Chapter 8](@/wakey/08-channels.md) showed the `touch` thread putting them into the static channel `input::EVENTS` with `try_send`. Here we add how that thread decides what an event is.

Its function `poll` in `ui/input.rs` asks the touch controller for a point every 10 ms. The first reading with a finger becomes a `Down` event. Later readings become `Move` events, but only if the position has changed. A release needs more care. A comment in `poll` says that the controller sometimes reports nothing for one poll in the middle of a drag, so one empty reading is not yet a release. The thread counts empty readings, and the third in a row (`RELEASE_POLLS`) sends `Up` with the last known position.

## The hook and its answer

The loop has two possible receivers for a touch. One is Slint's window. The other is optional, and the loop knows it only as a trait, `FrameHook`. In the product the hook is the list layer, which scrolls the long lists by itself. The excerpt shows the answer type and then one method of the trait. Look at the three variants and at the last line.

{% <code file="firmware/src/ui/render_loop.rs" lines="28-49" hl="30-39 49"> %}{% raw %}
/// Where a touch event goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Route {
    /// To the main Slint window.
    Slint,
    /// The hook used it.
    Hook,
    /// The hook takes over the touch Slint has had so far: Slint gets a
    /// cancel (the pointer left the window); this and the following events
    /// of the touch are the hook's.
    TakeOver,
}
// ... @44
pub trait FrameHook {
    // ... @48
    /// A touch event, before Slint sees it.
    fn pointer(&mut self, ev: &TouchEvent) -> Route;
{% endraw %}{% </code> %}

`Route` on lines 30 to 39 is what the hook answers when it is shown an event. `Slint` means "not mine, pass it on". `Hook` means "I used it, Slint must not see it". `TakeOver` is for a touch that started as Slint's and turns out to be the hook's. A finger comes down on a list row, and Slint gets the press, because it may be a tap. Then the finger moves up, and the list layer sees a scroll. It answers `TakeOver` once, and `Hook` for the rest of the touch.

The trait has seven methods, and line 49 is the one that belongs to this chapter. `pointer` receives each event before Slint does and returns a `Route`. The other six are about the hook's own drawing and its background work. We meet `idle_work` below, and chapter 13 mentions `start` and `finish`.

{% <tip title="An enum as a callback's answer" id="route-enum-answer"> %}
`pointer` could have returned a `bool` for "handled". But a `bool` has two values, and there are three cases here.

With an enum, each case has a name and a doc comment, and the `match` in the loop must cover every variant. If someone adds a fourth, the compiler rejects that `match` until it handles the new case.
{% </tip> %}

## Taking events and routing them

Now the loop itself. Its signature ends with the parameter `mut hook: Option<&mut dyn FrameHook>`. The hook is optional, and it is a *trait object*: the loop does not know the concrete type behind it and calls its methods through a table of function pointers. The excerpt is the whole `Touch` stage. Watch the three highlighted lines: where an event comes from, who is asked, and what happens to the answer.

{% <code file="firmware/src/ui/render_loop.rs" lines="250-279" hl="252 259 266"> %}{% raw %}
        enter(Stage::Touch);
        // Touch: route each event to the hook (e.g. list drags), or Slint.
        while let Some(ev) = woken_by.take().or_else(|| input::EVENTS.try_receive().ok()) {
            if ev.phase == Phase::Down {
                drop_touch = false;
            } else if drop_touch {
                drop_touch = ev.phase != Phase::Up;
                continue;
            }
            let route = hook.as_mut().map_or(Route::Slint, |h| h.pointer(&ev));
            // ... @266
            match route {
                Route::Slint => {
                    dispatch(&window, &ev);
                    if ev.phase == Phase::Down && transition::pending() {
                        drop_touch = true;
                        // Whatever the old page's touch area holds ends.
                        window.dispatch_event(WindowEvent::PointerExited);
                    }
                }
                Route::Hook => {}
                // A pressed touch area gets `PointerEventKind.cancel`.
                Route::TakeOver => window.dispatch_event(WindowEvent::PointerExited),
            }
        }
{% endraw %}{% </code> %}

Line 252 takes one event per round. `woken_by` holds at most one event, and we will see where it is filled. `take()` empties it. If it was empty, `or_else` asks the channel with `try_receive`, which never waits and returns an error when the queue is empty. `ok()` turns that error into `None`, and `None` ends the `while let`.

Line 259 asks the hook. Without a hook, `map_or` gives `Route::Slint`. With one, it calls `pointer`.

The `match` on line 266 carries the answer out. For `Route::Slint`, the helper `dispatch` translates our `TouchEvent` into the `WindowEvent` that Slint expects and hands it to the window. This is the moment a button's callback from chapter 11 runs. For `Route::Hook` there is nothing left to do. For `Route::TakeOver`, line 277 tells Slint that the pointer has left the window. A row that was shown as pressed is released without being clicked.

{% <tip title="`as_mut` borrows what is inside an `Option`" id="hook-as-mut"> %}
Why `hook.as_mut().map_or(…)` and not `hook.map_or(…)`? `map_or` takes the `Option` by value. An `Option<&mut dyn FrameHook>` is not `Copy`, because two copies of a `&mut` must not exist. So the plain call would move `hook` away, and the next round of the loop would have none. The compiler rejects that.

`as_mut()` takes `&mut hook` and returns a new, short-lived `Option` with a reference to the content. That one may be used up. `hook` itself stays where it is, which is why `hook.as_mut()` appears at every use in this function.
{% </tip> %}

Lines 253 to 258 and 269 to 273 belong together, and they solve one problem with the flag `drop_touch`.

{% <pitfall title="A press that changes the page must not release on the new one" id="touch-drop-rest"> %}
Some buttons act when they are pressed, and some of those change the page. The finger is then still down, and its `Up` has yet to arrive. Sent to Slint, that `Up` would land on the new page, as a tap on whatever lies under the finger there.

So after a `Down` is dispatched, line 269 asks `transition::pending()` whether that press requested a page change. If so, the loop sets `drop_touch`. Lines 255 to 257 then skip every later event of this touch, up to and including its `Up`. The next `Down` clears the flag.
{% </pitfall> %}

After the touch stage the loop calls `slint::platform::update_timers_and_animations()`. Slint has no thread of its own, so this call is where its timers fire, the ones from chapter 11 included. If a page change was requested, the loop then plays the slide and starts the next pass. Otherwise it renders and presents a frame, which is chapter 13.

## Waiting when nothing moves

The next excerpt is the end of the function, where the loop decides whether to go round at once. The highlighted lines are the hook's background work and the wait.

{% <code file="firmware/src/ui/render_loop.rs" lines="397-411" hl="401-403 408-410"> %}{% raw %}
        if !window.has_active_animations() && !hook_active && !other_buffer_stale {
            stats.maybe_log();
            // Background work first (one piece), then sleep until the next
            // Slint timer or a touch.
            if hook.as_mut().is_some_and(|h| h.idle_work(rot)) {
                continue;
            }
            enter(Stage::Sleep);
            let wait = slint::platform::duration_until_next_timer_update().map_or(MAX_SLEEP, |d| {
                Duration::from_micros(d.as_micros() as u64).min(MAX_SLEEP)
            });
            if let Either::Second(ev) = select(Timer::after(wait), input::EVENTS.receive()).await {
                woken_by = Some(ev);
            }
        }
{% endraw %}{% </code> %}

The condition on line 397 has three parts. No Slint animation is running, the hook is not changing the screen, and no frame buffer has to catch up (chapter 13). Only then may the loop rest. First, on line 401, the hook gets a turn for one small piece of background work. If it says there is more, the loop goes round again, so a touch is noticed between two pieces.

Lines 405 to 407 work out how long to wait. Slint tells how long until its next timer is due. The loop waits that long, but never longer than `MAX_SLEEP`, 100 ms.

Line 408 is the wait. `select` from `embassy_futures` takes two futures and finishes when the first of them does. Here they are a timer and `EVENTS.receive()`, and this is the `await` that chapter 8 pointed to. `Either::First` means the time is up. `Either::Second(ev)` means a touch arrived. `receive` has already taken that event out of the queue, so the loop keeps it in `woken_by`. Line 252 finds it there on the next pass.

[Chapter 9](@/wakey/09-flags-and-locks.md) gave the reason for the 100 ms limit: a request stored in an atomic wakes nobody. The loop has to come round and look.

## Sleep and wake

The requests in question are the ones in `ui::power`. At the top of every pass the loop calls `power::take_request()`. If the request asks for a state the display is not in, the loop calls `switch_refresh`. For sleep, that function sets the `asleep` flag, waits 150 ms so that the light meter's thread can switch the backlight off, and halves the panel's refresh rate.

While the display is asleep, each pass takes a short cut right after this check, in lines 230 to 249. It still runs Slint's timers. That matters, because the check that wakes the display for a ringing alarm is a Slint timer in `app/power.rs`. Then the loop looks for a touch event in the same way as the touch stage does. A `Down` calls `power::wake()` and sets `drop_touch`, so the touch that wakes the display presses nothing. Without an event, the loop waits in the same `select` as above, for `MAX_SLEEP`. Every path ends in `continue`, so nothing is drawn.

The next pass finds the wake request. `switch_refresh` restores the refresh rate, and the loop sets a local flag, `waking`, so that the short cut no longer applies and the pass draws a frame. Only when that frame has been presented does the loop call `power::set_asleep(false)`.

For our two questions: the parts work together here because one loop takes turns between them. The state of this chapter lives in two places. Local variables of `run` (`woken_by`, `drop_touch`, `waking`) belong to the UI thread alone. The atomics in `ui::power` and the `Stage` are what other threads may read or write. The [next chapter](@/wakey/13-pixels-out.md) follows the same loop from `back_buffer` to `present`.
