+++
title = "From main to the executor"
description = "What Embassy's executor is, how the firmware starts it, and why it runs a single task."
weight = 3

[extra]
map = ["runtime"]
map_note = "`runtime` is one of the two support modules at the bottom of the map. It starts the executor on which the whole of `app` then runs."
goals = [
  "what happens between `main` and the first line of application code",
  "what an executor is, and how this one sleeps and wakes up again",
  "why the executor is stored in a `StaticCell`",
  "why a single task is enough here, and what Embassy is actually used for",
]
takeaways = [
  "`runtime::start` spawns one thread called `app` and runs Embassy's executor on it. It is a separate thread because the main thread cannot get a 64 KiB stack at the moment it is created.",
  "An executor polls the tasks that are ready and puts its thread to sleep when none is. A waker, called from an interrupt, a timer or another thread, makes a task ready again.",
  "`StaticCell` gives the executor a permanent address and hands out a `&'static mut` to it once, without any `unsafe`.",
  "There is exactly one task. Tasks on one executor cannot interrupt each other, so everything that must not wait behind a long frame runs on a thread instead.",
]
+++


The previous chapter showed the firmware's layers as a map. In this chapter we watch the program start. We follow the processor from `main` to the moment the screen shows its first picture, and we do it in two chapters. This one covers the first few milliseconds: how `main` reaches the module `runtime`, and what `runtime` starts. The next chapter covers the long start-up function that follows.

This is also the chapter about Embassy. Embassy is a family of crates for `async` programming on small devices, and the part this firmware uses is its *executor*, the piece that runs `async` functions. We will see what an executor does, and we will find that this firmware uses it in an unusual and very modest way.

## Three steps from `main` to the executor

We start with the binary. The whole file is one function.

{% <code file="firmware/src/bin/wakey.rs" lines="5-7"> %}{% raw %}
fn main() -> ! {
    wakey::app::main()
}
{% endraw %}{% </code> %}

There is nothing here but a call into the library crate. The firmware keeps its code in a library, which a second program, a test bench for the user interface, also uses. The return type `!` says that `main` never returns. A clock has no reason to exit.

The function it calls, `app::main`, is hardly longer.

{% <code file="firmware/src/app/mod.rs" lines="158-161"> %}{% raw %}
/// Entry point of the `wakey` binary.
pub fn main() -> ! {
    runtime::start(|spawner| spawner.spawn(app(spawner).unwrap()))
}
{% endraw %}{% </code> %}

Line 160 does two things at once, so we take it apart. `runtime::start` receives a closure. Later, when the executor is ready, it will call that closure and pass it a `Spawner`, which is the handle used to give the executor a task to run. Our closure uses the spawner for exactly one task, `app`.

Notice that the closure calls `app(spawner)` like a normal function, although we will see in a moment that `app` is an `async fn`. This call does not run any of `app`'s code yet. The `#[embassy_executor::task]` attribute has turned the `async fn` into an ordinary function, and that function only reserves the memory the task needs and returns a token for it. The reservation can fail in one case, which is when the task is already running. That cannot happen here, because this is the only place that starts it, so the code calls `unwrap`. Finally `spawner.spawn` hands the token to the executor.

## What `runtime::start` does

Now we look at the function that was called. In the next excerpt, the highlighted lines are the ones to watch: a `static` at the top, and the thread that is spawned in the middle.

{% <code file="firmware/src/runtime.rs" lines="134-163" hl="134 153-159"> %}{% raw %}
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Stack of the executor task (Slint rendering runs on it).
const EXECUTOR_STACK: usize = 64 * 1024;

/// Common start-up for every application binary: ESP-IDF runtime patches,
/// logger, then the Embassy executor on its own task, `app`.
///
/// Not on the main task: esp_hosted (Wi-Fi via the C6) allocates internal
/// RAM in a C constructor, before ESP-IDF creates the main task, and a
/// 64 KiB main stack then does not fit beside the start-up stack (boot
/// loop in `esp_startup_start_app`). Once `main` runs, the start-up stack is
/// free again. `app` gets 64 KiB of internal RAM, the main task's priority
/// (1) and core 0; the display ISRs are installed from it, so they run on
/// the same core as the render loop (see [`crate::display`]).
pub fn start(init: impl FnOnce(Spawner) + Send + 'static) -> ! {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let app = Thread::new(c"app", EXECUTOR_STACK)
        .core(Core::Core0)
        .priority(1)
        .spawn(move || {
            let executor = EXECUTOR.init(Executor::new());
            executor.run(init)
        })
        .expect("app task");
    let _ = app.join();
    unreachable!("the executor never returns");
}
{% endraw %}{% </code> %}

The function begins with two lines of platform set-up. `link_patches` makes sure that a few fixes the `esp-idf-sys` crate adds to ESP-IDF are not thrown away by the linker. ESP-IDF is the manufacturer's software framework that this firmware is built on, and it includes the operating system, FreeRTOS. The second line sends everything written with the `log` crate to the serial console.

Then, from line 153, the function spawns a thread named `app`. `Thread` is the firmware's own small builder for threads, and [chapter 5](@/wakey/05-threads.md) reads it closely. For now it is enough to see what the new thread does in lines 157 and 158. It creates the executor and calls `run` on it. `run` never returns, so the thread never ends, and that is why the last two lines of `start` can wait for it with `join` and then declare the rest `unreachable!`.

Why a new thread at all? We are already on a thread, the one that runs `main`. The comment above the function gives the reason, and it is a very typical one for embedded work: memory at one particular moment. The user interface is drawn on this thread, and the drawing code needs a large stack of 64 KiB. The stack of the main thread is reserved very early during boot. At that point a temporary start-up stack still exists and the Wi-Fi component has already taken its share, so 64 KiB in one piece is not available. A little later, once `main` is running, the start-up stack has been freed. A thread created at that point gets its 64 KiB without trouble.

{% <concept title="What an executor does"> %}
An `async fn` does not run by itself. The compiler turns it into a value, called a future, that has a `poll` method. Each call to `poll` runs the function up to the next `await` that cannot continue yet, and then returns.

So something has to call `poll`, and call it again when the thing the function was waiting for has happened. That something is the executor. Embassy's executor for `std` is a short loop. It polls every task that has been marked as ready, and when no task is ready it blocks its thread on a condition variable.

The other half is the *waker*. When a timer runs out, or an interrupt arrives, or another thread sends a message, a waker marks the waiting task as ready and signals the condition variable. The thread wakes up and the loop goes round again. While nothing happens, the thread sleeps and uses no processor time.
{% </concept> %}

{{ <svg src="img/executor.svg" caption="The executor's loop on the `app` thread, and the three events that wake it up in this firmware." /> }}

One detail in the excerpt deserves a closer look. Line 134 declares the executor's home as a `static`, and line 157 fills it. The executor is not an ordinary local variable, and there is a reason.

{% <tip title="`StaticCell`: a `&'static mut` without `static mut`" id="static-cell"> %}
`Executor::run` is declared with `&'static mut self`. Spawners and wakers keep a pointer to the executor, so it must stay at the same address for as long as the program runs. A local variable cannot promise that.

The classic answer is a `static mut`, but every access to one needs `unsafe`. `StaticCell<T>` is a safer tool for the same job. It is a `static` with room for one `T`. Its `init` method moves a value in and returns a `&'static mut T`.

It can do this safely because it only works once. A second call to `init` panics. The check happens while the program runs, and in exchange the code needs no `unsafe` at all.
{% </tip> %}

## One task, and why

If you have seen other Embassy projects, you may have expected a dozen tasks by now: one for the touch screen, one for the network, one for sound. This firmware has one, a function called `app::app` that the next chapter reads from top to bottom. With the feature `ui-bench`, the clock program adds a second, which plays scripted touches. Everything else that runs at the same time is a thread, and there are about a dozen of them.

The reason is in how tasks share a thread.

{% <pitfall title="A task cannot interrupt another task" id="cooperative"> %}
Tasks on one executor take turns. A turn ends only when the task reaches an `await` that has to wait. Until then, no other task on that executor runs.

In this firmware one turn can be long. Drawing a frame is a single stretch of code with no `await` in it, and changing the page takes 40 to 60 ms according to the project's documentation. A task that reads the touch screen would get no turn during that time, however urgent the touch.

A thread is different. The operating system stops a thread the moment another thread with a higher priority has work to do. So work that cannot wait as long as the longest frame needs a thread of its own, with a priority above the user interface.
{% </pitfall> %}

The second reason is more ordinary. Much of the work is done by libraries that block: the USB library, the Wi-Fi driver, the file system, the audio decoders. On an executor, each of them would hold up every other task. On a thread of its own, each one holds up only itself.

So what is Embassy used for? It gives the `app` thread three things to wait for with `await`. The first is an interrupt from the screen that says a frame has been shown. The second is a timer. The third is a channel that the touch thread fills with events. With these, "sleep until the next timer or the next touch, whichever comes first" is a single line of code. A thread with a hand-written wait could do the same job. The firmware pays for the convenience with a handful of extra crates and a patched copy of `esp-idf-svc`, the crate that holds the time driver.

For our two questions, this chapter adds one piece to the first. The parts of this firmware work together as threads, and one of those threads, `app`, also runs an executor so that the user interface can wait for several things at once. The [next chapter](@/wakey/04-startup.md) follows the one task through the start-up and shows how it creates all the other threads.
