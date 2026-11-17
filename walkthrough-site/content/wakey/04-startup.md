+++
title = "Start-up: one task brings the device to life"
description = "The order in which `app::app` starts every service, and how ownership of the hardware forces that order."
weight = 4

[extra]
map = ["app"]
map_note = "`app` is the top layer. Its start-up function touches almost every other box, because it is the one place that creates them all."
goals = [
  "how the `app` task takes the board's hardware apart and hands it out",
  "why the start-up steps come in exactly this order",
  "how a `match` arm that never returns works, for the type checker and for the borrow checker",
  "where the task waits with `await` for the first time, and what wakes it",
]
takeaways = [
  "`app::app` performs the whole start-up as ordinary blocking code. That is harmless here, because no other task exists that it could hold up.",
  "The start-up order follows ownership. A handle is created, lent to whatever needs a look at it, and then moved to the one part of the program that keeps it.",
  "The time zone is set before the first service thread is spawned, because it changes a process-wide environment variable.",
  "An expression of type `!` fits wherever any type is expected, and the borrow checker ignores moves on a path that never comes back.",
  "The first real `await` is in the render loop, where the task waits for the screen's frame interrupt.",
]
+++

The [previous chapter](@/wakey/03-executor.md) ended with an executor running on a thread called `app`, and with exactly one task handed to it. In this chapter we read that task from its first line to its last. It is the function that turns a board full of switched-off chips into a running alarm clock.

It is worth reading closely for a second reason. The start-up is the one place in the firmware where every piece of hardware and every service appears together, so it shows very clearly who ends up owning what.

## The one task

We begin with the first half of the function. Watch for two highlighted places: a field of the board that is renamed, and a `match` with an unusual second arm.

{% <code file="firmware/src/app/mod.rs" lines="163-187" hl="176 181-184"> %}{% raw %}
#[embassy_executor::task]
async fn app(
    // Only the UI bench spawns a task here (its scripted gestures).
    #[allow(unused_variables)] spawner: embassy_executor::Spawner,
) {
    let Board {
        display,
        backlight,
        i2c,
        ioexp,
        rtc,
        modem,
        c6_link,
        sd: _sd, // stays mounted
        codec,
    } = Board::init().expect("board init");

    clock::init(&rtc);
    let config = match config::load() {
        Ok(c) => c,
        Err(e) => config_error(display, backlight, e).await,
    };
    texts::init(config.language);
    clock::set_time_zone(&config.time_zone);
    let nvs = EspDefaultNvsPartition::take().expect("NVS");
{% endraw %}{% </code> %}

The task's first statement is `Board::init()`. It switches on and sets up the board's hardware, and it returns a struct with one field per piece of hardware. [Chapter 6](@/wakey/06-board.md) follows that function. Here we only see its result being taken apart again straight away, into nine separate variables. The reason is that each handle is about to be given to a different part of the program, and a struct cannot be handed out in pieces while it is still whole. The pattern names all nine fields and ends without `..`, the pattern for "and the rest". So if someone adds a field to `Board` later, this line stops compiling until the new handle gets a place here, and it cannot be dropped, and its hardware switched off, unnoticed.

Line 176 renames one field while taking it out: `sd: _sd`. This is the handle for the memory card. Nothing in the task needs it, but the card is unmounted when the handle is dropped. Binding it to a variable keeps it alive for as long as the task runs, and the underscore at the front of the name tells the compiler not to warn about a variable that is never used.

The lines that follow read like a script, and their order is not a matter of taste.

- `clock::init(&rtc)` sets the system time from the real-time clock, a small chip with its own power reserve that keeps the time while the device is off. The call only *borrows* the chip's handle. We will see in a moment why that matters.
- `config::load` reads the file `config.json` from the memory card. This is why the card had to be mounted first, inside `Board::init`.
- `texts::init` and `clock::set_time_zone` use two settings from that file, the language and the time zone.

The time zone line has a second, hidden reason to stand exactly here. It sets an environment variable called `TZ`. Environment variables belong to the whole program and not to one thread, and the C library does not protect a thread that reads them while another one changes them. The code avoids the problem by its position. At line 186 the firmware has not started any thread of its own besides the `app` thread that is running this code. The first service thread is started two lines later.

{% <tip title="A branch that never returns fits any `match`" id="never-arm"> %}
Look at lines 181 to 184. The first arm of the `match` produces a `Config`. The second arm calls `config_error`, which shows an error page and then runs forever. Its return type is `!`.

An expression of type `!` never produces a value, so Rust lets it stand wherever a value of any type is expected. That is why the two arms are allowed to "disagree".

The borrow checker reasons the same way. The second arm moves `display` and `backlight` into `config_error`, and yet both are used again further down in the function. This compiles because no path leads from that arm to the code below it.
{% </tip> %}

## Starting the services

The second half of the start-up creates the firmware's services. A service here means a part such as the network or the sound output that does its work on a thread of its own. As you read the next excerpt, look at the arguments of each call. They show which piece of hardware goes where.

{% <code file="firmware/src/app/mod.rs" lines="188-212" hl="198 202-205"> %}{% raw %}
    let wifi = net::start(
        net::Setup {
            modem,
            c6_power: ioexp.switch(Pin::WlanPwrEn),
            c6_link,
            nvs: nvs.clone(),
            cfg: config.wifi.clone(),
            interval: Duration::from_secs(u64::from(config.sync_interval_h) * 3600),
            display_asleep: crate::ui::power::asleep,
        },
        move || clock::store_system_time(&rtc),
    );
    // Speakers: set up now; the player's sound goes to the USB speaker,
    // or the built-in one if none is connected, while an alarm rings.
    let player = Player::new();
    let builtin = BuiltinSpeaker::start(codec, i2c.clone(), ioexp.clone(), player.source());
    input::start(i2c.clone(), ioexp.clone());
    let speaker = UsbAudio::start(ioexp.switch(Pin::Usb5vEn), player.source()).expect("USB host");
    let alarm = Rc::new(RefCell::new(AlarmClock::new(
        config.alarms,
        config.ringing,
        nvs,
        player,
        Output::new(speaker.clone(), builtin.clone()),
    )));
{% endraw %}{% </code> %}

We go through the calls in order.

`net::start` creates the thread that looks after Wi-Fi. Its first argument is a struct named `Setup`, and three of its fields are hardware we just took out of the board: the modem, a power switch, and a serial link. The Wi-Fi radio is a second chip on the board, the ESP32-C6, and the power switch and the serial link both lead to it. From this line on, those three handles belong to the Wi-Fi thread and nothing else can use them.

The second argument, on line 198, is a closure that the Wi-Fi thread will call whenever it has fetched the exact time from the internet. The closure writes that time into the real-time clock. The keyword `move` gives the closure ownership of `rtc`. Now we can see why `clock::init` only borrowed it earlier: the task needed the clock once, for a moment, and here it gives the clock away for good.

Lines 202 to 205 create the sound and touch parts. The `Player` decodes sound files, and it is created first because both speakers ask it for a *source*, a closure that hands out audio samples. `BuiltinSpeaker::start` and `input::start` each spawn a thread, and `UsbAudio::start` spawns two, `usb-lib` and `uac`.

Several of these calls receive `i2c.clone()` or `ioexp.clone()`. Cloning here does not copy any hardware. I2C is a two-wire bus that connects many of the board's chips, and the handle `i2c` is an `Arc<Mutex<…>>` around its one driver. Cloning it creates another reference to the same bus. Whoever wants to talk to a chip locks the mutex for a moment.

The last call builds the `AlarmClock`. It holds the list of alarms and decides when one rings, and we will meet it properly in [chapter 15](@/wakey/15-state-on-device.md). It receives the alarms from the configuration, the player, and an `Output` that knows both speakers. It is wrapped in `Rc<RefCell<…>>` because several parts of the user interface will share it, all on this one thread.

{% <diagram caption="The start-up in order. Each arrow is a call that creates the thread it points to. The notes are work that the `app` thread does itself."> %}
sequenceDiagram
    participant M as main
    participant A as app
    participant W as wifi
    participant S as speaker
    participant T as touch
    participant U as uac
    participant L as ambient
    M->>A: spawn the thread, then wait
    Note over A: Board::init
    Note over A: clock, config, time zone
    A->>W: net::start
    A->>S: BuiltinSpeaker::start
    A->>T: input::start
    A->>U: UsbAudio::start
    Note over A: AlarmClock, window, pages
    A->>L: ambient::start
    Note over A: render loop, forever
{% </diagram> %}

After this, about fifty lines create the window and connect the pages of the user interface. They are the subject of [chapter 10](@/wakey/10-slint.md) and [chapter 11](@/wakey/11-view-models.md), and we skip them here. The task ends like this.

{% <code file="firmware/src/app/mod.rs" lines="266-272" hl="271"> %}{% raw %}
    watch_render_loop();
    #[cfg(feature = "task-stats")]
    crate::taskstats::start();
    #[cfg(feature = "ui-bench")]
    spawner.spawn(bench::run().unwrap());
    render_loop::run(window, display, &mut rot, Some(&mut list)).await
}
{% endraw %}{% </code> %}

Line 271 is the last line the task ever reaches. `render_loop::run` draws the user interface, frame after frame, and never returns. It takes `display` by value, so the render loop is the final owner of the screen.

## Where the task first waits

Here is something that may surprise you. Everything we have read so far runs inside an `async fn`, and yet none of it waits with `await`. `Board::init` waits at least 300 ms for the touch controller to start up, and `config::load` reads a file. Both simply block the thread until they are done.

In most `async` programs that would be a mistake. While one task blocks the thread, the executor cannot run any other task. Here it does no harm, for a plain reason: there is no other task.

The first `await` that really waits is inside the render loop. Before it draws a frame, the loop asks the display for a buffer to draw into.

{% <code file="firmware/src/display/mod.rs" lines="544-555" hl="547"> %}{% raw %}
        if let Some(requested_at) = self.pending_flip {
            // The flip is done once one full frame ended after the request.
            while Self::frames_scanned().wrapping_sub(requested_at) as i32 <= 0 {
                FRAME_DONE.wait().await;
            }
            self.pending_flip = None;
        }
        let back = 1 - self.front;
        // SAFETY: the driver allocated PIXELS u16 per buffer; the DMA only
        // reads the front buffer, and `&mut self` prevents aliasing.
        unsafe { core::slice::from_raw_parts_mut(self.fbs[back], PIXELS) }
    }
{% endraw %}{% </code> %}

The display has two buffers, one that is being shown and one that can be drawn into, and they swap after every frame. The swap is only complete when the panel has finished sending the current frame. The panel reports that with an *interrupt*, a signal from the hardware that makes the processor run a short function at once, whatever it was doing.

`FRAME_DONE` is the link between that interrupt and our task. On line 547 the task waits for it. If no frame has finished, `wait().await` cannot continue, the task hands control back to the executor, and the executor puts the thread to sleep. When the interrupt arrives, it wakes the task and the loop continues. This is the place where `async` earns its keep in this firmware. The render loop keeps exact step with the screen without checking again and again in a busy loop, and [chapter 13](@/wakey/13-pixels-out.md) shows the whole of it.

For our two questions, this chapter gives a first answer to each. The parts work together because one task creates them all, in an order that ownership dictates, and hands every piece of hardware to exactly one of them. And the state so far lives in two kinds of places: in the `AlarmClock` on the `app` thread, and in the services that each own their hardware. The [next chapter](@/wakey/05-threads.md) lists the threads that these services run on.
