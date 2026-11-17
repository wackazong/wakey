+++
title = "Rust tips and tricks: the recap"
description = "Every Rust tip, pitfall and `unsafe` note of the tour, sorted by theme."
weight = 21

[extra]
goals = [
  "where to find each Rust tip, pitfall and `unsafe` note again",
  "what each technique is, and why the firmware uses it",
  "how the walkthrough answers its two questions in the end",
]
takeaways = [
  "`wakey-core` holds what can be computed without hardware. `firmware` holds everything that touches the board, in four layers that `app` connects.",
  "Each exclusive piece of hardware has one owner. What several threads need is behind a cheap `Clone` handle, an atomic or a channel.",
  "The state lives on the `app` thread in `Ctx` and `AlarmClock`, in the services' statics and handles, in `config.json` on the card and in NVS in flash.",
]
+++

The twenty chapters marked Rust idioms, patterns and traps where the code shows them. This page repeats every one, sorted by theme.

Each entry says briefly what the technique is and why the firmware uses it. The link "Where the code does this" leads to the original callout and its excerpt.

## Ownership, borrowing and lifetimes

These entries are about who owns a value and how long it lives.

{% <tip title="A singleton you can take apart" id="peripherals-take" chapter="06-board"> %}
`Peripherals::take()` succeeds once per program. Its fields move out one by one, so a second user of the same pin is a compile error.
{% </tip> %}

{% <pitfall title="`_name` keeps a value, `_` drops it" id="underscore-binding" chapter="06-board"> %}
`_sd` is a real variable and keeps the memory card mounted. A bare `_` binds nothing, so the card would be dropped, and unmounted, at the end of the statement.
{% </pitfall> %}

{% <tip title="A guard without a name lives for one statement" id="temporary-guard" chapter="07-sharing-hardware"> %}
A guard without a variable is dropped at the end of its statement. So one line locks the I2C bus for one transfer.
{% </tip> %}

{% <pitfall title="A `RefCell` checks its borrows at run time" id="ctx-refcell-two-statements" chapter="11-view-models"> %}
`borrow_mut` panics while another borrow is alive, and the compiler cannot warn. The alarm list's handler therefore borrows the `AlarmClock` in two separate statements.
{% </pitfall> %}

{% <tip title="`StaticCell`: a `&'static mut` without `static mut`" id="static-cell" chapter="03-executor"> %}
The executor lives in a `StaticCell`, a `static` with room for one value. Its `init` returns a `&'static mut` without `unsafe`, and a second call panics.
{% </tip> %}

{% <tip title="A scoped thread may borrow from its caller" id="scoped-thread" chapter="05-threads"> %}
Threads in `std::thread::scope` are joined before the scope ends, so they may borrow local variables. The `cover` thread borrows the downloaded picture this way.
{% </tip> %}

{% <tip title="`as_mut` borrows what is inside an `Option`" id="hook-as-mut" chapter="12-touch-in"> %}
`map_or` alone would move the render loop's optional hook away. `as_mut()` lends its content for one use, so the hook stays for the next pass.
{% </tip> %}

{% <tip title="`self: &Rc<Self>` as a receiver" id="ctx-rc-self-receiver" chapter="11-view-models"> %}
A method with this receiver can clone the `Rc` it is called on. `Ctx::with` gives each Slint callback such a clone, so the closure borrows nothing.
{% </tip> %}

{% <pitfall title="A ring of strong handles is never freed" id="ctx-weak-window" chapter="11-view-models"> %}
The window stores the callbacks, and each callback owns the `Rc<Ctx>`. So `Ctx` holds only a weak handle to the window, which breaks the ring.
{% </pitfall> %}

## Types, enums and patterns

Here the type system does work that a run-time check would do otherwise.

{% <tip title="An enum variant carries the data of its state" id="alarm-phase-data" chapter="14-state-machine"> %}
`Ringing` and `Snoozed` hold their own fields, and `Idle` holds none. So "idle, with a ring timeout" cannot be written, and reading `until` needs a `match` first.
{% </tip> %}

{% <tip title="Match on a tuple to write a transition table" id="alarm-tuple-match" chapter="14-state-machine"> %}
A tuple of state and input makes each arm read as "in this state, on this input". A guard adds what a pattern cannot express.
{% </tip> %}

{% <tip title="Match arms are tried in order, so the general one goes last" id="one-touch-arm-order" chapter="16-one-touch"> %}
Rust takes the first arm that matches. The list layer's `pointer` lists its guarded special cases first and the plain arm last.
{% </tip> %}

{% <tip title="An enum as a callback's answer" id="route-enum-answer" chapter="12-touch-in"> %}
The hook answers with `Route` and not with a `bool`, because there are three cases. The loop's `match` must cover each, so a new variant cannot be forgotten.
{% </tip> %}

{% <tip title="A branch that never returns fits any `match`" id="never-arm" chapter="04-startup"> %}
An expression of type `!` may stand where any type is expected, as `config_error` does in the start-up. The borrow checker also ignores moves on a path that never comes back.
{% </tip> %}

{% <tip title="A builder in three lines per setting" id="struct-update-builder" chapter="05-threads"> %}
`..self` takes every field that a setter does not name, so no setter changes when the `Thread` struct gains a field.
{% </tip> %}

{% <tip title="`degrade_input_output`: one type for any pin" id="degrade-pin" chapter="06-board"> %}
The call turns a pin of its own type into an `AnyIOPin`, so a function can accept any pin. The pin is moved either way.
{% </tip> %}

{% <tip title="A trait that combines two traits, for `dyn`" id="sound-readseek-blanket" chapter="17-sound"> %}
A trait object may name one ordinary trait. `ReadSeek` requires `Read` and `Seek`, and a blanket impl gives it to every type that has both.
{% </tip> %}

## Closures and functions

The firmware passes behaviour around as values.

{% <tip title="Three ways to pass a function" id="passing-functions" chapter="02-layers"> %}
A `fn` pointer takes only what captures nothing. An `impl Fn` parameter takes any closure but has no name for a struct field. A `Box<dyn Fn>` has a name and costs an allocation.
{% </tip> %}

{% <tip title="What each part of the `Job` type is for" id="boxed-closure-message" chapter="08-channels"> %}
`Box<dyn …>` gives every closure the one type that a channel needs. `FnOnce` lets the job move its captures out, and `Send` lets it cross to the `wifi` thread.
{% </tip> %}

{% <tip title="An atomic that is changed under a lock" id="sound-played-under-lock" chapter="17-sound"> %}
An atomic can be changed through `&`, but a `load` and a `store` are two steps. The fade-in's count is read and written under the ring's lock, so a reset cannot fall between them.
{% </tip> %}

## Handles, shared state and threads

These entries show how up to thirteen threads reach what they share.

{% <tip title="A cheap `Clone` handle over an `Arc`" id="clone-handle" chapter="02-layers"> %}
A service returns a small `Clone` struct with an `Arc` or a channel sender inside. Each user owns a clone, so nobody needs a reference with a lifetime.
{% </tip> %}

{% <tip title="A handle with methods hides the channel" id="handle-hides-channel" chapter="08-channels"> %}
`UsbAudio` keeps its `Sender` and its command enum private and offers methods. The protocol between the two threads stays in one module.
{% </tip> %}

{% <pitfall title="A full queue: drop a move, wait for the rest" id="full-queue-drops" chapter="08-channels"> %}
`try_send` fails when the touch queue is full. A `Move` is dropped and counted, because the next one follows. A `Down` or an `Up` must arrive, so the thread retries until there is room.
{% </pitfall> %}

{% <tip title="`swap` takes a value and leaves an empty one behind" id="atomic-swap-take" chapter="09-flags-and-locks"> %}
`swap` writes the new value and returns the old one in one indivisible step. A `load` and a `store(0)` could erase a request stored in between.
{% </tip> %}

{% <pitfall title="Change the copy and the chip under one lock" id="cached-register" chapter="07-sharing-hardware"> %}
`IoExpanders::with` holds the lock on its copy of a register until the changed byte is on the chip. Otherwise two threads changing different pins could undo each other.
{% </pitfall> %}

{% <pitfall title="A wake-up sent between the check and the wait is lost" id="lost-wakeup" chapter="09-flags-and-locks"> %}
A `Condvar` does not store a notification. The decoder checks `stop` and waits under one lock, and `Player::stop` takes that lock before it notifies.
{% </pitfall> %}

{% <pitfall title="A setting that stays behind after the call" id="spawn-config-per-thread" chapter="05-threads"> %}
ESP-IDF keeps the spawn configuration per calling thread, for every thread it creates from then on. `Thread::configured` sets it, spawns and puts the old one back.
{% </pitfall> %}

{% <pitfall title="Two sources, one ring" id="sound-two-sources-one-ring" chapter="17-sound"> %}
Both speakers' sources drain one ring, and only a comment says that one may do so at a time. `Output` switches one speaker off before the other on.
{% </pitfall> %}

## Async and the UI thread

The `app` thread runs the executor, Slint and the render loop, in turns.

{% <pitfall title="A task cannot interrupt another task" id="cooperative" chapter="03-executor"> %}
A task's turn ends only at an `await` that has to wait, and a page change takes 40 to 60 ms. So reading touch is a thread with a higher priority.
{% </pitfall> %}

{% <pitfall title="A callback runs inside the loop that called it" id="one-touch-callback-blocks" chapter="16-one-touch"> %}
The `toggle` handler writes `config.json` on the render loop's own call stack. Until the card has answered, the loop draws nothing and fires no timer.
{% </pitfall> %}

{% <pitfall title="A feature flag can be a promise" id="slint-single-thread-promise" chapter="10-slint"> %}
Slint's feature `unsafe-single-threaded` is a promise that one thread alone calls Slint. The firmware keeps it by structure: all Slint code runs in the one task.
{% </pitfall> %}

{% <pitfall title="Prepare the window, then create the component" id="slint-window-order" chapter="10-slint"> %}
Slint does not say which component it is creating, so prepared windows are handed out in order. Nothing checks that order, and the code keeps the two steps together.
{% </pitfall> %}

{% <pitfall title="A press that changes the page must not release on the new one" id="touch-drop-rest" chapter="12-touch-in"> %}
After such a press, the finger's `Up` would be a tap on the new page. The loop sets `drop_touch` and skips the rest of that touch.
{% </pitfall> %}

{% <pitfall title="The handler and the loop share a core" id="display-isr-same-core" chapter="13-pixels-out"> %}
`present` reads a counter that the display's interrupt handler changes, so it must run on the handler's core. Both run on core 0, where the `app` thread created the panel.
{% </pitfall> %}

## Drop, and state that stays consistent

These entries are about not leaving things half done.

{% <tip title="A guard whose `Drop` finishes the work" id="net-job-guard" chapter="18-radio"> %}
`JobGuard` calls its job with an error in `Drop` if nobody ran it, so every exit of `session` answers the job. The `Option` inside lets `drop` take the `FnOnce` out.
{% </tip> %}

{% <pitfall title="Drop order is part of the program" id="net-drop-order" chapter="18-radio"> %}
In `session` the Wi-Fi driver must go before the transport. Rust's drop rules give that order, but no type enforces it, so a comment marks the place.
{% </pitfall> %}

{% <tip title="Remember what you did last, and compare" id="state-follow-compare" chapter="15-state-on-device"> %}
`follow` compares the machine's state with what it saved last, and the ringing alarm with what sounds. It writes to flash or commands the player only on a difference.
{% </tip> %}

{% <tip title="Change a copy, then commit it in one place" id="one-touch-copy-commit" chapter="16-one-touch"> %}
`set_enabled`, `save` and `delete` each build a new list and end in `set_alarms`. One function validates, saves and applies every change.
{% </tip> %}

{% <pitfall title="Write the file before you change memory" id="state-file-before-memory" chapter="15-state-on-device"> %}
`set_alarms` writes `config.json` before it changes memory. If the write fails, `?` returns, and the clock and the card still agree.
{% </pitfall> %}

{% <pitfall title="FAT cannot rename onto an existing file" id="state-fat-rename" chapter="15-state-on-device"> %}
`sd::replace` must remove the old file before it renames the temporary one. A power failure in between leaves only `config.tmp`, so `config::load` looks for it and puts it back.
{% </pitfall> %}

{% <pitfall title="Do not measure a duration with wall-clock time" id="alarm-two-clocks" chapter="14-state-machine"> %}
Whether an alarm is due is asked in local time. A snooze is measured in Unix time, because local time jumps by an hour on two nights a year.
{% </pitfall> %}

## Errors

Three small idioms for the policies of chapter 20.

{% <tip title="`anyhow::bail!` and the format `{e:#}`" id="errors-anyhow-bail-alternate" chapter="20-errors"> %}
`bail!` builds an error from a message and returns it at once. `{e:#}` prints an `anyhow::Error` with its whole chain of causes on one line.
{% </tip> %}

{% <tip title="`inspect_err(..).ok()`: log the error, keep an `Option`" id="errors-inspect-err-ok" chapter="20-errors"> %}
`inspect_err` logs the error and passes the `Result` on, and `ok()` makes an `Option` of it. So the clock runs without NVS when it cannot be opened.
{% </tip> %}

{% <tip title="A poisoned lock is taken anyway" id="errors-poisoned-lock" chapter="20-errors"> %}
The error from a poisoned `Mutex` still contains the guard, and `into_inner` takes it out. So one thread's panic does not take down every other user.
{% </tip> %}

## `unsafe`

Each entry names the facts that make one `unsafe` site sound.

{% <unsafe title="Borrowing two pins before the driver takes them" id="unsafe-i2c-recover" chapter="07-sharing-hardware"> %}
`SysI2c::new` reborrows its two pins to free a stuck bus by hand. The pin drivers made from them are dropped before the I2C driver takes the pins.
{% </unsafe> %}

{% <unsafe title="One modem, one driver at a time" id="unsafe-modem-reborrow" chapter="07-sharing-hardware"> %}
Each Wi-Fi session reborrows the one modem for its driver. The promise is that this driver is dropped before the next session reborrows.
{% </unsafe> %}

{% <unsafe title="A slice over the driver's frame buffer" id="unsafe-display-back-buffer" chapter="13-pixels-out"> %}
The driver allocated `PIXELS` values per buffer, so the slice's length is right. The DMA only reads the front buffer, and `&mut self` rules out a second slice.
{% </unsafe> %}

{% <unsafe title="`mem::zeroed()` is only sound for some types" id="unsafe-zeroed-config" chapter="19-unsafe"> %}
Zero must be a valid value for every field. The C structs here hold integers, floats, booleans, raw pointers and optional function pointers, and for all of these it is.
{% </unsafe> %}

{% <unsafe title="A callback's context pointer must outlive the callback" id="unsafe-callback-context" chapter="19-unsafe"> %}
`on_done` casts its context pointer to the `AtomicU32` in a box that `FbCopy` owns. The `Drop` of `FbCopy` waits for the last copy, so the counter outlives the interrupt.
{% </unsafe> %}

{% <unsafe title="A register block as a `static mut`" id="unsafe-static-mut-register" chapter="19-unsafe"> %}
The code never makes a reference to the register block. `&raw mut` gives a raw pointer, the accesses are volatile, and the comment's other claims are about timing.
{% </unsafe> %}

## Project structure

This group is about the repository and the build.

{% <tip title="An empty `[workspace]` table keeps a crate to itself" id="own-workspace" chapter="01-clock-board-crates"> %}
It makes the crate a workspace of its own. The firmware's target, nightly compiler and `build-std` then do not reach crates built for the host.
{% </tip> %}

{% <tip title="`unsafe_code = \"forbid\"` for a whole crate" id="forbid-unsafe" chapter="01-clock-board-crates"> %}
This lint makes every `unsafe` block in `wakey-core` a compile error. Unlike a `deny`, a `forbid` cannot be switched off with `#[allow]`.
{% </tip> %}

{% <pitfall title="The compiler does not check the layers" id="layers-not-checked" chapter="02-layers"> %}
Inside one crate any module can use any other, so the layer rule is a convention. The code keeps it by passing a module what it needs as an argument.
{% </pitfall> %}

{% <tip title="`include!`: generated code in a module of its own" id="slint-include-generated" chapter="10-slint"> %}
`include!` pastes the Rust file generated from the `.slint` sources into `mod screens`, away from `app`'s own names. Slint's own macro knows only the last of the two files compiled.
{% </tip> %}

## The two questions

How do the parts work together? The firmware calls the pure logic in `wakey-core`, never the other way round. There, `app` connects four layers by passing small things downwards. At run time, threads send a message where something must happen and share a value where something must be known.

Where does the state live? The `app` thread holds `Ctx` and the `AlarmClock`, with the alarm machine inside. The services keep theirs in statics and behind handles. What must survive a reboot is in `config.json` and in NVS. The Slint properties only show it, except for the current page.
