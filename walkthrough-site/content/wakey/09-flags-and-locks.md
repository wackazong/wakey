+++
title = "How threads talk: flags, locks and wake-ups"
description = "State that several threads read and write: atomics in statics, a mutex behind a handle, and a condition variable."
weight = 9

[extra]
map = ["ui", "ambient", "audio"]
map_note = "The examples are the sleep flag and the dimming factor in `ui`, the light meter in `ambient`, and the player's ring in `audio`."
goals = [
  "how an atomic in a `static` lets one thread leave a value for another, and what `Ordering::Relaxed` promises",
  "how a counter beside a value tells every reader that it changed",
  "when the code uses a `Mutex` instead, and how it keeps the lock short",
  "how a `Condvar` lets a thread sleep until shared data changes, and how a wake-up can get lost",
]
takeaways = [
  "A value that fits in an integer and is complete on its own lives in a `static` atomic with `Ordering::Relaxed`. It wakes nobody.",
  "`swap` reads and replaces in one step, so `take_request` neither misses a request stored at that moment nor carries one out twice.",
  "Data that does not fit in an atomic sits in an `Arc<Mutex<…>>` behind a handle, and the lock is held only long enough to copy the value.",
  "The waiting thread checks its condition under the lock. `Player::stop` takes the same lock before it notifies, so its wake-up cannot fall between the check and the wait.",
]
+++

This chapter and the [previous one](@/wakey/08-channels.md) are a pair about how the firmware's threads tell each other things. Chapter 8 covered messages: one thread sends a value, another receives it, and from then on the value belongs to the receiver. This chapter covers the other way, shared state. Here a value stays in one place, and several threads read it or write it there.

The firmware uses three tools for shared state, and we take them in order of weight: atomics, a mutex, and a mutex with a condition variable. The figure shows our examples, writers on the left and readers on the right.

{{ <svg src="img/shared-state.svg" caption="The coloured arrow is a wake-up: the reader of the ring tells the writer that there is room again." /> }}

## A flag in a static

The display can be asleep, which means that the backlight is off and nothing is drawn. `ui/power.rs` keeps this in two statics declared at its top: `ASLEEP`, an `AtomicBool`, and `REQUEST`, an `AtomicU8`. Watch the highlighted `load` and `swap`.

{% <code file="firmware/src/ui/power.rs" lines="33-50" hl="34 49"> %}{% raw %}
pub fn asleep() -> bool {
    ASLEEP.load(Ordering::Relaxed)
}

/// Asks the render loop to put the display to sleep (any thread).
pub fn sleep() {
    REQUEST.store(Request::Sleep as u8, Ordering::Relaxed);
}

// ... @47
/// The request since the last call, if any.
pub(super) fn take_request() -> Option<Request> {
    Request::from_u8(REQUEST.swap(0, Ordering::Relaxed))
}
{% endraw %}{% </code> %}

A `static` exists once and every thread can reach it, but an ordinary `static` cannot be changed. An atomic type lifts that limit for one integer or boolean. Its methods take `&self`, and each of them happens as one indivisible step, so no thread ever sees a half-written value.

`asleep` on line 34 is the reading side. The Wi-Fi thread calls it because it starts no periodic session while the display sleeps, and the light meter's thread, `ambient`, calls it to switch the backlight off. Both received it at start-up as a plain `fn() -> bool`, as [chapter 2](@/wakey/02-layers.md) showed. The writing side is the render loop, the loop on the `app` thread that draws the screen. It reports through `set_asleep`.

`REQUEST` goes towards the render loop. `sleep` stores a number that stands for a wish, and so does `wake`, which the excerpt leaves out. 0 means that there is no wish, and a newer wish replaces an older one. The render loop collects the wish with `take_request` on line 49. The comments allow `sleep` and `wake` from any thread. At this commit their callers are a timer in `app/power.rs` and the render loop itself, when a touch wakes the display, and both run on the `app` thread.

{% <tip title="`swap` takes a value and leaves an empty one behind" id="atomic-swap-take"> %}
`REQUEST.swap(0, …)` writes 0 and returns what was there before, in one indivisible step.

The obvious alternative is a `load` followed by a `store(0)`. Between those two calls another thread could store a new request, and the `store(0)` would erase it unseen. With `swap`, every request is either returned by this call or still there for the next one.
{% </tip> %}

Every one of these calls takes an `Ordering`, and in this file it is always `Relaxed`. The ordering does not decide whether the operation itself is safe, because a `load`, `store` or `swap` is indivisible with every ordering. What a stronger ordering adds is a promise about other memory: a reader may then rely on also seeing data that the writer wrote before it touched the atomic. `Relaxed` makes no such promise. For `ASLEEP` none is needed, because the flag is the whole message.

A flag has one cost: storing a request wakes nobody. Here that does no harm, because both callers run on the `app` thread, and the render loop looks at the flag at the start of each round. [Chapter 12](@/wakey/12-touch-in.md) shows that loop.

## A value, and a counter that says it changed

In a dark room the firmware darkens every pixel by a factor. The `ambient` thread sets that factor through `dim::set`, and the drawing code on the `app` thread reads it. But a new factor only affects what is drawn afterwards, so something has to notice the change and repaint the screen. A second static, `EPOCH`, makes that possible.

{% <code file="firmware/src/ui/dim.rs" lines="30-53" hl="33-34"> %}{% raw %}
/// Sets the brightness factor in 1/256 (any thread; [`FULL`] = undimmed).
pub fn set(factor: u16) {
    let factor = factor.min(FULL);
    if FACTOR.swap(factor, Ordering::Relaxed) != factor {
        EPOCH.fetch_add(1, Ordering::Relaxed);
        log::debug!("dim: {factor}/256");
    }
}
// ... @49
/// True once if the dimming changed since `seen` (updated).
pub fn changed_since(seen: &mut u32) -> bool {
    let now = epoch();
    core::mem::replace(seen, now) != now
}
{% endraw %}{% </code> %}

`set` swaps the new factor in on line 33 and gets the old one back. Only if the two differ does line 34 add one to `EPOCH`, so `EPOCH` counts the changes.

`changed_since` is the reader's half. The caller keeps a number of its own, `seen`, with the count it saw last. The function loads the current count through `epoch()`, puts it into `seen` with `mem::replace`, and returns whether the two differed. In `app/mod.rs`, a timer on the `app` thread calls it every 100 ms and repaints the whole window when the answer is `true`.

A counter is used, and not a flag that the reader clears, because there is more than one reader. The module's comment names a second one, the list layer, which draws the scrolling lists. Each reader compares the counter with its own remembered count, so neither takes the news away from the other.

{% <pitfall title="Two atomics are two messages" id="two-atomics"> %}
`FACTOR` and `EPOCH` belong together, but with `Relaxed` each travels on its own. A reader on the other core could see the new count with the old factor, and repaint nothing until the next change.

Fixed after this commit: `set` adds to `EPOCH` with `Release` and `epoch` loads it with `Acquire`, so a reader that sees the new count also sees the factor stored before it.
{% </pitfall> %}

## A lock behind a handle

Atomics end where a value no longer fits in an integer. The state of the Wi-Fi connection is an enum, `Status`, and some of its variants carry strings. For such a value the firmware uses a `Mutex`, a lock that lets one thread at a time reach the data inside it.

{% <code file="firmware/src/net.rs" lines="151-162" hl="152"> %}{% raw %}
pub struct Wifi {
    status: Arc<Mutex<Status>>,
    jobs: mpsc::Sender<Job>,
}

impl Wifi {
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
{% endraw %}{% </code> %}

`Wifi` is the rest of the program's handle on the Wi-Fi thread. Line 152 is the shared state. The `Mutex` guards the `Status`, and the `Arc`, a reference-counted pointer that threads may share, lets the handle and the thread own it together.

`status` holds the lock only briefly. It locks, clones the `Status`, and returns the clone. The lock guard is a temporary value, so the lock is released when `status` returns. The caller, the clock screen, then works with its own copy. The Wi-Fi thread writes in the same way, in its one-line method `set`.

The `unwrap_or_else` in the middle takes the data even if another thread panicked while it held the lock. [Chapter 7](@/wakey/07-sharing-hardware.md) explains why.

## Waiting for a change

A mutex protects data, but it cannot wait, and the player needs to wait. A thread called `decoder` pushes samples into a ring, a queue that holds one second of sound, and the speaker takes them out. When the ring is full, the decoder must sleep until there is room. [Chapter 17](@/wakey/17-sound.md) covers the player as a whole.

{% <concept title="A condition variable"> %}
A condition variable, `Condvar` in Rust, is a place where threads sleep until another thread notifies them. It always works together with a mutex.

`wait` takes the lock guard, then releases the lock and puts the thread to sleep as one step. When the thread is notified, `wait` takes the lock again and returns the guard. So the thread holds the lock whenever it looks at the shared data.
{% </concept> %}

The ring is a struct with a `Mutex<VecDeque<i16>>` named `samples`, a `Condvar` named `space`, and a few atomic flags, among them `stop`. This is the start of the decoder's `push`; the highlighted lines are the wait.

{% <code file="firmware/src/audio/player.rs" lines="303-311" hl="305-309"> %}{% raw %}
fn push(ring: &Ring, mut samples: &[i16]) {
    while !samples.is_empty() {
        let mut q = ring.samples.lock().unwrap();
        while q.len() >= RING_SAMPLES && !ring.stop.load(Ordering::Acquire) {
            q = ring.space.wait(q).unwrap();
        }
        if ring.stop.load(Ordering::Acquire) {
            return;
        }
{% endraw %}{% </code> %}

Line 305 takes the lock. The loop on line 306 asks two questions: is the ring full, and has nobody asked us to stop? While both answers are yes, line 307 waits on `space`. The speaker's side is in `Player::source`. After it has taken samples out, it drops its guard and calls `space.notify_one()`. That wakes the decoder, and the loop asks its two questions again. Line 309 looks at `stop` once more, because the wake-up may have come from there.

That other wake-up is sent by `Player::stop`, which the `app` thread calls when a sound should end. Watch line 131, which takes the lock and gives it back at once.

{% <code file="firmware/src/audio/player.rs" lines="125-133" hl="131"> %}{% raw %}
    /// Stops the current file (the ring drains within a second).
    pub fn stop(&self) {
        self.ring.stop.store(true, Ordering::Release);
        // The decoder checks `stop` and waits under the lock: taking it
        // here makes sure it is either before its check or already
        // waiting, so the notification is not lost.
        drop(self.ring.samples.lock().unwrap());
        self.ring.space.notify_all();
    }
{% endraw %}{% </code> %}

Line 127 sets the flag and line 132 wakes the decoder. The line between them seems to do nothing, and its comment says what it is for.

{% <pitfall title="A wake-up sent between the check and the wait is lost" id="lost-wakeup"> %}
A notification is not stored. If no thread is waiting when it is sent, it does nothing.

Imagine `stop` without line 131. The decoder runs line 306, reads `stop` as `false` and finds the ring full. At that moment the `app` thread stores `true` and calls `notify_all`. Nobody is waiting yet, so nothing happens. Then the decoder reaches line 307 and goes to sleep, although it was told to stop. Only a speaker that takes samples would wake it now.

The decoder holds the lock from its check until `wait` releases it. Because `stop` takes the same lock, it cannot run in that gap. Either it gets the lock first, and the decoder's check then reads `true`. Or it gets the lock when the decoder is already waiting, and the notification arrives.
{% </pitfall> %}

`audio/player.rs` is also a place where other orderings appear. The flags `stop` and `playing` are stored with `Release` and loaded with `Acquire`, and one `swap` uses `AcqRel`, which is both. This pair is the stronger promise from earlier: a thread whose `Acquire` load reads the value of a `Release` store also sees what the storing thread wrote before that store. The file's comments do not say which data this is for.

## Written once, and written by an interrupt

The screen's texts never change after start-up. `app/texts.rs` keeps them in a `static` of type `OnceLock<Texts>`, a cell that starts empty and can be filled exactly once. The start-up fills it with `set`, and afterwards every caller gets a `&'static Texts`.

An interrupt handler, the short function the processor runs when hardware signals an event, ends in the tools we have seen. The camera's handler in `ambient.rs` stores `true` in an `AtomicBool`, and the `ambient` thread, while it waits for a frame, looks at it every 5 ms. The display's handler adds one to an atomic counter and notifies the `app` task, the wake-up from [chapter 4](@/wakey/04-startup.md).

For our two questions, this chapter completes the answer to the first: the parts work together by messages where something must happen, and by shared values where something must be known. It also adds a place where state lives: a handful of statics and handles that belong to no single thread. The [next chapter](@/wakey/10-slint.md) turns to the screen.
