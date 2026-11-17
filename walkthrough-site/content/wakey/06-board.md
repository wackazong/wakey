+++
title = "The board: one `take`, many owners"
description = "From a single `Peripherals::take` to the final owner of every pin and chip."
weight = 6

[extra]
map = ["board", "display"]
map_note = "`board` and `display` are in the hardware layer. `board` knows the pins and chips of the circuit board and brings them up; `display` is the screen driver that `board` creates on the way."
goals = [
  "how Rust makes sure that each piece of hardware has exactly one user",
  "what `Board::init` does, step by step, and what it returns",
  "which chips stop the start-up when they are missing, and which one may be absent",
  "where each handle ends up, and why `sd: _sd` is not the same as `sd: _`",
]
takeaways = [
  "`Peripherals::take()` succeeds once per program. Every pin and every built-in device of the processor is a value that can be moved, so the compiler can check that it has one user.",
  "`Board::init` brings the hardware up in seven steps and returns a `Board` with nine owned handles. Some are finished drivers, others are untouched peripherals for a service to use later.",
  "A missing IO expander, clock chip or touch controller panics with a message that names it. A missing memory card becomes `None`, and the start-up goes on.",
  "Each handle that is not shared moves to exactly one thread or to the render loop, and from then on it needs no lock.",
  "A pattern `_name` binds a value and keeps it alive. A bare `_` binds nothing, so the value is dropped where it stands.",
]
+++

[Chapter 4](@/wakey/04-startup.md) read the start-up function `app::app`. Its first statement called `Board::init()` and took the result apart into nine variables, and we followed each variable to the service that received it. In this chapter we stand on the other side of that call. We ask where the nine handles come from, what type each one has, and what it means that Rust lets each of them have one owner only. The file to read is `firmware/src/board/mod.rs`.

## Hardware as values

The processor on this board, the ESP32-P4, has many built-in devices: an I2C controller, timers, serial ports, and its pins. Embedded programmers call these devices *peripherals*. The question for any firmware is how to keep two parts of the program from driving the same pin.

The firmware uses the crate `esp-idf-hal`, a Rust wrapper around the drivers of ESP-IDF, the manufacturer's software framework. The code reaches it as `esp_idf_svc::hal`, because the crate `esp-idf-svc` passes it on under that name. The crate gives every peripheral a type of its own and one value of that type. To use a pin you must hold its value. The value can be moved but not copied, so the ordinary ownership rules now apply to hardware.

{% <tip title="A singleton you can take apart" id="peripherals-take"> %}
All those values start out in one struct, `Peripherals`, and there is one way to get it: `Peripherals::take()`. The function keeps a flag in a `static`. The first call sets the flag and returns the struct. Every later call finds the flag set and returns an error.

So there can be only one `Peripherals` in the program. Its fields can be moved out one by one, `p.i2c0` to one driver and `p.pins.gpio22` to another, and each field can be moved once. A second user of the same pin is a compile error, "use of moved value".
{% </tip> %}

## What `Board::init` returns

Before we read the function, we look at its result. The struct `Board` has nine fields, and their types tell us a good deal.

{% <code file="firmware/src/board/mod.rs" lines="44-62"> %}{% raw %}
pub struct Board {
    /// The system I2C bus (touch polling uses it directly).
    pub i2c: SharedI2c,
    /// Power switches and resets on the two IO expanders.
    pub ioexp: IoExpanders,
    /// The RX8130 real-time clock (UTC).
    pub rtc: rtc::Rx8130,
    pub display: Display,
    pub backlight: Backlight,
    /// The microSD card at [`sd::MOUNT_POINT`], if one is inserted and
    /// readable. Unmounted when dropped.
    pub sd: Option<sd::SdCard>,
    /// Wi-Fi through the ESP32-C6, for [`crate::net::start`].
    pub modem: Modem<'static>,
    /// The C6's IO2 → GPIO14, read by UART1: the AirDrop receiver's frames.
    pub c6_link: C6Link,
    /// I2S to the ES8388 codec (built-in speaker).
    pub codec: codec::CodecPins,
}
{% endraw %}{% </code> %}

Four of the fields are finished drivers that the firmware defines itself. `display` is the screen, which is already streaming pictures to the panel. `backlight` controls the brightness of the lamp behind the screen. `rtc` is the real-time clock, a small chip named RX8130 that keeps the time while the device is off. `sd` is the mounted memory card.

Three fields are different. `modem`, `c6_link` and `codec` are peripherals that `Board::init` has not touched at all. `Modem<'static>` is the value from `esp-idf-hal` that stands for the radio, which on this board is a second chip, the ESP32-C6. `C6Link` and `CodecPins` are small structs, each of which bundles one device with the pins that are wired to it. The board module knows which pins belong together, because that is a fact about the circuit board. The driver itself is created later by the service that uses it.

The remaining two, `i2c` and `ioexp`, are the shared ones. I2C is a two-wire bus that connects many of the board's chips to the processor. The IO expanders are two chips on that bus that provide extra output pins, which the board uses as power switches and reset lines. Many parts of the firmware need both, so they cannot have a single owner, and [chapter 7](@/wakey/07-sharing-hardware.md) is about them.

## Seven steps, in a fixed order

Now the function itself. It is numbered by its own comments from 1 to 7, and the next excerpt shows steps 1 to 6. Watch the highlighted lines: the `take`, two pins that are converted, the clock chip that must answer, and the detection of the panel.

{% <code file="firmware/src/board/mod.rs" lines="66-96" hl="66 71-72 82-83 92"> %}{% raw %}
        let p = Peripherals::take()?;

        // 1. I2C with bus recovery.
        let i2c: SharedI2c = Arc::new(Mutex::new(SysI2c::new(
            p.i2c0,
            p.pins.gpio31.degrade_input_output(),
            p.pins.gpio32.degrade_input_output(),
        )?));

        // 2. Expanders: rails off, LCD and touch in reset. Then release the
        //    resets after the 10 ms minimum pulse width.
        let ioexp = IoExpanders::new(i2c.clone())?;
        FreeRtos::delay_ms(10);
        ioexp.release_display_resets()?;

        // 3. RTC: supercap charging on, interrupt sources off.
        let rtc = rtc::Rx8130::new(i2c.clone())
            .unwrap_or_else(|e| panic!("RX8130 RTC at {:#04x} does not answer: {e}", rtc::ADDR));

        // 4. Backlight PWM, dark until the first frame is on the panel.
        let mut backlight = Backlight::new(p.ledc.timer0, p.ledc.channel0, p.pins.gpio22)?;
        backlight.set_percent(0)?;

        // 5. Panel variant from the touch controller. The ST712x touch
        //    controller needs a moment after reset before it answers.
        FreeRtos::delay_ms(300);
        let variant = detect_panel(&mut i2c::lock(&i2c));
        log::info!("board: panel variant {variant:?}");

        // 6. Display: DSI PHY power, panel init, video mode streaming.
        let display = Display::new(p.dsi, variant)?;
{% endraw %}{% </code> %}

Line 66 is the one `Peripherals::take` of the whole firmware. Everything else in the function moves fields out of `p`.

Step 1 creates the driver for the I2C bus from the controller `p.i2c0` and two pins, and wraps it in `Arc<Mutex<…>>`. Step 2 creates the handle for the IO expanders and gives it a clone of the bus. According to the comment, this first switches the power rails off and holds the screen and the touch controller in reset. After 10 ms, the shortest reset pulse those chips accept, line 79 releases the resets.

Step 3 sets up the real-time clock, and here we meet the first of two policies for missing hardware. `Rx8130::new` returns a `Result`. If the chip does not answer, `unwrap_or_else` runs a closure that panics, and the message names the chip and its bus address. The module's comment calls such chips required. A panic that says "RX8130 RTC at 0x32 does not answer" tells whoever reads the log which part to look at.

Step 4 creates the backlight and sets it to 0 percent at once. The comment says why: the screen stays dark until the first frame is on the panel.

Step 5 solves a problem that only real products have. The module's comment says that three combinations of screen and touch controller exist across the production runs of this board. The firmware has to find out which one it runs on. After a pause of 300 ms, which the touch controller needs after its reset, `detect_panel` asks on the I2C bus which touch controller answers, and from that it concludes which panel is fitted. If none answers, it panics with the names and addresses of the candidates, in the same style as the clock chip. Step 6 then creates the `Display` for that variant.

The IO expanders in step 2 are treated the same way: `IoExpanders::new` checks that both answer and panics with the address of the one that does not. The remaining failures are not named. The I2C driver, the writes to the expanders, the backlight and the display end in `?`, so an error there travels up to `app::app`, which stops at its `expect("board init")`. [Chapter 20](@/wakey/20-errors.md) compares these policies.

{% <tip title="`degrade_input_output`: one type for any pin" id="degrade-pin"> %}
`p.pins.gpio31` has the type `Gpio31`. That is what makes the single-owner check work, but a function that should accept any pin cannot name one such type. Lines 71 and 72 call `degrade_input_output()`, which turns the pin into an `AnyIOPin`. Afterwards the pin number is no longer part of the type.

With the erased type, `SysI2c::new` and the fields of `CodecPins` do not have to name pin numbers in their signatures. `SdPins` in `board/sd.rs` goes the other way and keeps the exact types, `Gpio39` to `Gpio44`. Ownership is the same in both cases: the pin has been moved, and `p` no longer has it.
{% </tip> %}

The seventh step is the memory card, and it shows the second policy.

{% <code file="firmware/src/board/mod.rs" lines="98-118" hl="114-117"> %}{% raw %}
        // 7. microSD (optional).
        let sd = match sd::mount(
        // ... @109
        ) {
            Ok(card) => {
                log::info!("board: microSD mounted at {}", sd::MOUNT_POINT);
                Some(card)
            }
            Err(e) => {
                log::warn!("board: no microSD ({e})");
                None
            }
        };
{% endraw %}{% </code> %}

`sd::mount` also returns a `Result`, but this time the error arm does not panic. It writes a warning to the log and turns the error into `None`. That is why the field has the type `Option<sd::SdCard>`. The board comes up with or without a card, and the type forces everyone who wants the card to consider both cases.

Looking back, the order of the seven steps follows what each step needs. The bus comes first, because the expanders, the clock and the touch controller are all reached through it. The panel is detected before the display driver is created, because `Display::new` takes the variant as an argument.

## Where each handle ends up

`Board::init` returns, and `app::app` takes the struct apart. Chapter 4 showed those lines, so here we only collect the result in one picture.

{{ <svg src="img/ownership.svg" caption="Each field of `Board` and its final owner. Solid arrows are moves. The two dashed arrows are handles that are cloned, which is the subject of chapter 7." /> }}

Five of the moves cross into another thread. `backlight` goes to `ambient::start`, which moves it into the closure of the `ambient` thread. From then on that thread is the only code that sets the brightness. `modem` and `c6_link` travel inside `net::Setup` to the `wifi` thread. `rtc` is moved into the time-sync closure, and `net::start` stores that closure on the `wifi` thread too. `codec` goes to `BuiltinSpeaker::start`, which builds the audio driver from its pins and moves the driver into the `speaker` thread.

Two handles stay on the `app` thread. `display` is passed by value to the render loop. `sd` is bound to a local variable that nothing ever reads.

`Backlight::set_level` takes `&mut self`, and the `ambient` thread can call it without any lock, because the compiler knows that no one else holds the backlight. The same is true for each solid arrow in the picture. Where hardware has one owner, no synchronisation is needed.

{% <pitfall title="`_name` keeps a value, `_` drops it" id="underscore-binding"> %}
The pattern in `app::app` contains `sd: _sd` (line 176 of `app/mod.rs`, shown in chapter 4). The variable is never used, so one might write `sd: _` instead. The two are not the same.

`_sd` is a real variable. The card moves into it and lives until the task ends, which is never. A bare `_` is not a variable and moves nothing. The card would stay inside the temporary `Board` that `Board::init()` returned, and a temporary is dropped at the end of its `let` statement.

Dropping matters here. The comment on `SdCard` says "unmounted when dropped": its fields unmount the file system and release the power supply of the card's pins. With `sd: _`, the code three lines further down, which reads `config.json` from the card, would no longer find a mounted card.
{% </pitfall> %}

For our two questions, this chapter adds the hardware side of both. The parts work together without stepping on each other because each exclusive piece of hardware is a value with one owner, and the type system checks that from `Peripherals::take()` to the last thread. And the state of the hardware lives with those owners: the brightness in the `ambient` thread, the radio in the `wifi` thread, the screen's buffers in the render loop. What is left is the hardware that several threads really must share. The [next chapter](@/wakey/07-sharing-hardware.md) shows how the firmware shares the I2C bus and the expanders.
