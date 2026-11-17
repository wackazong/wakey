+++
title = "Everything that runs: the thread inventory"
description = "A small builder for threads, and the list of all threads with their cores and priorities."
weight = 5

[extra]
map = ["runtime"]
map_note = "`runtime` is a support module at the bottom of the map. Every module that needs a thread gets it here."
goals = [
  "how the `Thread` builder gives a `std` thread a core, a priority and a stack location",
  "why it sets a configuration before each spawn and restores the old one",
  "which threads exist, on which core and at which priority",
  "the reasons the code gives for where it puts `touch`, `decoder` and `wifi`",
]
takeaways = [
  "Every thread is created through `runtime::Thread`, so each call site states name, stack, core and priority in one place.",
  "ESP-IDF keeps the settings for new threads per calling thread, so `configured` sets them, spawns and restores the old ones.",
  "Heavy work is pinned to core 1, away from the user interface on core 0. On core 1 the priorities are an order of urgency: `touch` 6, `decoder` 5, `card-writer` 4, `wifi` 1.",
  "`ui-watch` reads a few atomics once a second and logs when the render loop has stopped.",
]
+++

The [previous chapter](@/wakey/04-startup.md) saw the `app` task create one service after another, and most of those services started a thread. This chapter collects all the threads in one list, so that you know everything that can run at the same moment.

Before the list, we read the tool that creates them. It is a small struct in `runtime.rs`, the file we met in [chapter 3](@/wakey/03-executor.md).

## A builder for threads

Rust's standard library can give a new thread a name and a stack size, and nothing more. This firmware needs three more settings. The processor has two cores, and a thread can be *pinned* to one of them, which means it never runs on the other. A thread also has a priority, a number that the operating system, FreeRTOS, uses to decide who runs first. And its stack can be placed in PSRAM, the 32 MB of slower external memory, instead of the scarce internal memory.

Here is the struct with its first two methods. Watch the highlighted lines: the constructor is a `const fn`, and `core` ends in `..self`.

{% <code file="firmware/src/runtime.rs" lines="23-49" hl="33 44-48"> %}{% raw %}
#[derive(Clone, Copy)]
pub struct Thread {
    name: &'static CStr,
    stack_size: usize,
    core: Option<Core>,
    priority: Option<u8>,
    stack_in_psram: bool,
}

impl Thread {
    pub const fn new(name: &'static CStr, stack_size: usize) -> Self {
        Self {
            name,
            stack_size,
            core: None,
            priority: None,
            stack_in_psram: false,
        }
    }

    /// Pinned to `core`.
    pub const fn core(self, core: Core) -> Self {
        Self {
            core: Some(core),
            ..self
        }
    }
{% endraw %}{% </code> %}

The struct on lines 24 to 30 describes a thread that does not exist yet. Every thread needs a name and a stack size, so these are plain fields. The core and the priority are `Option`s, and `None` means "leave the default of ESP-IDF in place". ESP-IDF is the manufacturer's framework that the firmware is built on, and FreeRTOS is a part of it.

`new` on line 33 takes the two required values and fills in "no preference" for the rest. `core` on line 44 takes the whole description by value and returns a new one that differs in one field. The methods `priority` and `stack_in_psram`, which the excerpt leaves out, have the same shape. Because each method returns a `Thread`, a call site can chain them, as chapter 3 did with `Thread::new(c"app", EXECUTOR_STACK).core(Core::Core0).priority(1)`.

{% <tip title="A builder in three lines per setting" id="struct-update-builder"> %}
The expression `Self { core: Some(core), ..self }` is struct update syntax. It builds a new value, takes `core` from here and takes every other field from `self`. A setter written like this does not have to change when the struct gains a field.

The constructor and the three setters are `const fn`, so a chain of them can be evaluated at compile time. The methods that start a thread are ordinary functions.
{% </tip> %}

So far no thread has been started. That happens in `spawn`.

## Spawning with a configuration, and putting the old one back

Every call site ends its chain with `spawn` and passes it the closure that the new thread will run. `spawn` wraps the standard library's own call for starting a thread in a closure, and hands that closure to a private method named `configured`.

`configured` is where the three extra settings reach the operating system. As you read it, follow the variable `previous` from line 103 to line 121.

{% <code file="firmware/src/runtime.rs" lines="101-125" hl="103 117 120-121"> %}{% raw %}
    fn configured<R>(self, spawn: impl FnOnce(&'static str) -> R) -> R {
        let name = self.name.to_str().unwrap_or("thread");
        let previous = ThreadSpawnConfiguration::get().unwrap_or_default();
        let defaults = ThreadSpawnConfiguration::default();
        let config = ThreadSpawnConfiguration {
            name: Some(self.name),
            stack_size: self.stack_size,
            priority: self.priority.unwrap_or(defaults.priority),
            pin_to_core: self.core,
            stack_alloc_caps: if self.stack_in_psram {
                MallocCap::Spiram | MallocCap::Cap8bit
            } else {
                defaults.stack_alloc_caps
            },
            ..defaults
        };
        if let Err(e) = config.set() {
            log::warn!("{name}: thread configuration not set: {e}");
        }
        let result = spawn(name);
        if let Err(e) = previous.set() {
            log::warn!("{name}: thread configuration not restored: {e}");
        }
        result
    }
{% endraw %}{% </code> %}

Line 103 asks ESP-IDF for the spawn configuration that is in force right now, and keeps it in `previous`. Lines 105 to 116 then build the configuration for the new thread. Where the builder holds `None` for the priority, the code takes ESP-IDF's default. Lines 110 to 114 choose the memory for the stack, and `MallocCap::Spiram` is ESP-IDF's name for PSRAM.

Line 117 puts this configuration in force, line 120 runs the closure that actually spawns the thread, and line 121 restores `previous`. If setting or restoring fails, the code writes a warning to the log and carries on.

Why save and restore at all? The comment above the function says that ESP-IDF keeps the configuration per thread.

{% <pitfall title="A setting that stays behind after the call" id="spawn-config-per-thread"> %}
The standard library's spawn call has no parameter for a core or a priority. ESP-IDF instead keeps a spawn configuration for each thread, and it applies to the threads that this thread creates from then on.

So `config.set()` on line 117 is not an argument to one spawn. It changes a setting of the calling thread, and the setting stays. Without line 121, a thread that the same caller creates later by another route, for example through a library that calls `std::thread::spawn`, would also get the name, the core and the priority that were meant for one thread only. That is why `configured` puts the old values back.
{% </pitfall> %}

The builder has a second method to start a thread, `spawn_scoped`. It goes through `configured` too, and one thread uses it.

{% <tip title="A scoped thread may borrow from its caller" id="scoped-thread"> %}
`spawn` demands a `'static` closure, because the new thread may outlive the function that created it. Inside `std::thread::scope`, every thread is joined before the scope ends, so its closure may borrow the caller's local variables.

The firmware uses this in `tidal/playlist.rs`. Decoding the JPEG picture of an album cover needs more stack than the calling thread has. So the caller starts a thread named `cover` with a 64 KiB stack, lends it the downloaded bytes as a plain `&[u8]`, and joins it at once.
{% </tip> %}

## The inventory

The table lists the twelve call sites of `Thread::new`, plus the main thread, which ESP-IDF creates. A higher number is a higher priority. Where a call site sets no priority, the thread gets ESP-IDF's default, which the project's architecture document gives as 5. Where it sets no core, the thread may run on either one. Tasks that ESP-IDF and its drivers run themselves are left out.

| Thread | Stack | Core | Priority | What it does |
| --- | --- | --- | --- | --- |
| `main` | 8 KiB | 0 | 1 | Starts `app` and then waits for it. |
| `app` | 64 KiB | 0 | 1 | Runs the executor and the user interface. |
| `touch` | 6 KiB | 1 | 6 | Reads the touch controller every 10 ms and queues the events. |
| `decoder` | 64 KiB, PSRAM | 1 | 5 | Decodes one sound. It exists while the sound plays. |
| `card-writer` | 6 KiB | 1 | 4 | Writes blocks to the memory card during a TIDAL download. |
| `wifi` | 16 KiB | 1 | 1 | Runs Wi-Fi sessions and the jobs that need the network. |
| `usb-lib` | 4 KiB | any | 5 | Runs the event loop of ESP-IDF's USB host library. |
| `uac` | 8 KiB | any | 5 | Drives the USB speaker. |
| `speaker` | 4 KiB | any | 5 | Mixes the sound for the built-in speaker. |
| `ambient` | 6 KiB | any | 5 | Measures the room's light every 10 s and sets the backlight. |
| `ui-watch` | 4 KiB | any | 5 | Reports when the render loop has stopped. |
| `cover` | 64 KiB, PSRAM | any | 5 | Decodes one album cover and is joined at once. |
| `task-stats` | 6 KiB | any | 5 | Logs processor time per thread every 5 s. Only with the feature `task-stats`. |

{{ <svg src="img/threads.svg" caption="The threads by core and by priority. A thread higher up runs first when two on the same core are ready." /> }}

## Three placements and their reasons

The picture shows four threads pinned to core 1, one above the other. The code explains each placement in a comment at its call site, and we read three of them.

**`touch`, core 1, priority 6.** [Chapter 3](@/wakey/03-executor.md) explained why touch is a thread and not a task: a task would wait behind long frames. The module comment in `ui/input.rs` adds the number. As a task, two reads of the touch controller could be up to 90 ms apart, and a quick swipe measured from so few points has no speed. The comment on `input::start` explains the priority. The decoder runs on the same core at priority 5 and could hold the touch thread off, so touch stands one step above it. One read takes well under a millisecond.

**`decoder`, core 1, default priority.** The comment in `audio/player.rs` says that decoding and resampling can take a good part of a core. Its priority of 5 is above the user interface's priority of 1, so on core 0 it would run first and the `app` thread would wait. The code therefore pins it to core 1. The 64 KiB stack has a reason too: the audio library overflowed 8 KiB.

**`wifi`, core 1, priority 1.** The comment in `net.rs` says that a download takes about half a core. At priority 1 the thread stands below the decoder and the card writer, so that neither of them starves. And it is kept off core 0, because there, at the priority of the user interface, it makes animations stutter.

Read together, the comments describe one rule: core 1 is shared in order of urgency: touch first, then sound, then the card, then the network.

## A watchdog that only reads

One thread in the table does no work for the clock itself: `ui-watch`. The render loop is the loop on the `app` thread that draws every frame, and [chapter 12](@/wakey/12-touch-in.md) reads it. In the excerpt, look at what the watching thread touches: the two highlighted loads.

{% <code file="firmware/src/app/mod.rs" lines="357-369" hl="358 362"> %}{% raw %}
    let watch = runtime::Thread::new(c"ui-watch", 4096).spawn(|| {
        let mut last = render_loop::ITERATIONS.load(Relaxed);
        let mut still = 0u32;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = render_loop::ITERATIONS.load(Relaxed);
            // ... @368
            still += 1;
            if still == 2 || still.is_multiple_of(10) {
{% endraw %}{% </code> %}

Once a second the thread reads `render_loop::ITERATIONS`. This is an atomic counter in a `static`, and the render loop adds one to it each time it goes round. If the number has changed, the skipped lines reset `still` to zero. If it has not, `still` counts the seconds. After two seconds, and then every ten, the thread writes an error to the log. The message names the stage the render loop is in, the number of frames the panel has shown, and whether the touch thread is in the middle of a read. All three come from atomics as well.

The thread does not restart or repair anything. It only reports, and it needs neither a channel nor a handle to the user interface for that. The render loop writes a number, and another thread reads it.

For our two questions: the parts work together as up to thirteen threads on two cores, and their priorities settle who waits for whom. The state that the watchdog reads lives in atomics in `static`s. [Chapter 8](@/wakey/08-channels.md) and [chapter 9](@/wakey/09-flags-and-locks.md) look at how these threads talk to each other. First, the [next chapter](@/wakey/06-board.md) returns to the hardware they share.
