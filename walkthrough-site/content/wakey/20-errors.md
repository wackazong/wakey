+++
title = "When things go wrong: error handling"
description = "Panic, `Result`, typed reports or a log line: four policies and where each applies."
weight = 20

[extra]
map = ["app", "board", "net"]
map_note = "A failure starts low, in `board` or in a service such as `net`, and ends in `app`, which either stops, logs it or shows it in words."
goals = [
  "the four ways the firmware treats a failure, and how it chooses between them",
  "which error type is used where: `EspError`, `io::Error`, `anyhow::Error` and a plain `String`",
  "how a service reports a failure to `app` as a type, and where that type becomes a sentence",
  "small idioms for failures that are logged or ignored on purpose",
]
takeaways = [
  "What failed decides the treatment. Hardware the clock cannot run without stops the program, and everything optional is logged and left out.",
  "Inside a layer a failure travels as a `Result` with `?`. Drivers use `EspError`, file code uses `io::Error`, and `anyhow::Result` appears where several kinds meet.",
  "Across a layer boundary a service reports a small enum such as `net::Unavailable` or `tidal::Problem`. Only `app/texts.rs` turns it into words.",
  "Three kinds of chip panic with their name when they do not answer: the IO expanders, the RTC and the touch controller. The other failures of `Board::init` end in `expect(\"board init\")`.",
]
+++

Every chapter so far has passed a line that deals with a failure: an `expect`, a `?`, a log line. In this chapter we collect them. There is a system behind them, and it fits in a sentence: what failed decides how the failure is treated.

The firmware has four treatments. It stops, it passes the failure up as a `Result`, it reports a type to the layer above, or it writes a log line and carries on. The picture sets the four layers from [chapter 2](@/wakey/02-layers.md) against these four.

{{ <svg src="img/error-policy.svg" caption="The layers from top to bottom and the four policies, with one example from the code in each cell. The highlighted cell is the policy that is typical of the layer." /> }}

## Stop: hardware the clock cannot run without

A panic ends the program, and the firmware keeps it for parts without which nothing useful is left. The place to look is `Board::init`, which [chapter 6](@/wakey/06-board.md) read step by step. Here are a few of its lines again. Watch how the highlighted lines end.

{% <code file="firmware/src/board/mod.rs" lines="77-87" hl="77 82-83 86-87"> %}{% raw %}
        let ioexp = IoExpanders::new(i2c.clone())?;
        FreeRtos::delay_ms(10);
        ioexp.release_display_resets()?;

        // 3. RTC: supercap charging on, interrupt sources off.
        let rtc = rtc::Rx8130::new(i2c.clone())
            .unwrap_or_else(|e| panic!("RX8130 RTC at {:#04x} does not answer: {e}", rtc::ADDR));

        // 4. Backlight PWM, dark until the first frame is on the panel.
        let mut backlight = Backlight::new(p.ledc.timer0, p.ledc.channel0, p.pins.gpio22)?;
        backlight.set_percent(0)?;
{% endraw %}{% </code> %}

The RTC is the real-time clock, the chip that keeps the time while the device is off. If it does not answer, the closure panics, and the message names the chip and its bus address. Two more places do the same. `IoExpanders::new`, called on line 77, checks with an `assert!` that both IO expanders answer, and its message is "IO expander 0x.. does not answer". (The expanders are two chips that switch power rails and reset lines.) The third place is `detect_panel`, which panics when no touch controller answers.

Now look at how lines 77, 86 and 87 end: with `?`. An error from the expanders' register writes or from the backlight does not panic here. It leaves `Board::init` as an `anyhow::Error`, and the caller in `app::app` ends it with `Board::init().expect("board init")`. The same is true for the I2C driver and for the display.

The module's comment states both halves: the expanders, the RTC and the touch controller panic with their name, and other failures come back as the error of `Board::init`. So a display that does not start stops the program as well, but its panic carries the text "board init" and whatever the error says about itself.

Outside the board, `app::app` stops in the same way with `expect("NVS")`, `expect("USB host")` and `expect("PPA")`, `net::start` with `expect("wifi thread")`, and `input::start` with `expect("touch thread")`: without that thread the clock could not be operated at all. The PPA's rotator in `display/ppa.rs` stops the program on a timeout instead: a PPA that has made no progress for 2 s cannot be reset, so the rotator logs its last jobs and panics, and the reboot gives back a working clock.

## Pass it up: `Result`, and which error type where

Most failures are not fatal at the place where they happen, because that place does not know enough to decide. So the second policy is the ordinary Rust one: return a `Result`, with `?` on the way, and let the caller decide. What is specific to this firmware is which error type is used where.

- **`EspError` in drivers.** This type wraps a numeric error code of ESP-IDF, the C framework below the firmware. `Rx8130::new`, the read and write methods of `SysI2c` and `IoExpanders::set` return `Result<_, EspError>`.
- **`std::io::Error` for files.** `config::save_alarms` and `sd::replace` return `io::Result<()>`, and so does `AlarmClock::set_alarms`, which calls the first.
- **`anyhow::Result` where several kinds meet.** `Board::init` is one example and a Wi-Fi session in `net.rs` another. `anyhow::Error` can hold any error, so `?` works on all of them.
- **A `String` where the error goes to the screen.** `config::load` is the one example. (`play_sound` in `audio/player.rs` also returns a `String`, but that one only goes to the log.)

Here is the start of that function, which reads `config.json` from the memory card. Look at the return type on line 25 and at the error arms.

{% <code file="firmware/src/config.rs" lines="25-54" hl="25 40 43-49"> %}{% raw %}
pub fn load() -> Result<Config, String> {
    let path = sd::path(FILE);
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => match Config::load(&text) {
            // ... @38
            Err(e) => {
                log::error!("config: {FILE}: {e}");
                return Err(e.to_string());
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => match recover(&path) {
            Some(cfg) => cfg,
            None => {
                log::info!("config: no {FILE} ({e})");
                Config::default()
            }
        },
        Err(e) => {
            log::error!("config: {FILE}: {e}");
            return Err(e.to_string());
        }
    };
{% endraw %}{% </code> %}

The function meets two kinds of error: an `io::Error` from reading the file and a parse error from `Config::load`. On lines 40 and 52 it treats both alike: it logs the error and returns its text with `e.to_string()`. The doc comment says why a text is enough: the error is "in words, for the screen". The only caller shows it on an error page and never returns. [Chapter 4](@/wakey/04-startup.md#never-arm) has that call in its `never-arm` tip, and [chapter 15](@/wakey/15-state-on-device.md) explains the page.

Lines 43 to 49 are a different case. A match guard picks out one kind of `io::Error`, `NotFound`. A missing file usually means the first set-up, and then the inner `match` produces the defaults. Before that, `recover`, the function below `load`, looks for `config.tmp`. A power cut while the clock saves can leave that file behind instead of `config.json`, as [chapter 15](@/wakey/15-state-on-device.md#state-fat-rename) explains. If it parses, `recover` renames it into place, and `load` uses it.

{% <tip title="`anyhow::bail!` and the format `{e:#}`" id="errors-anyhow-bail-alternate"> %}
`net.rs` uses two small things from `anyhow`. The first is `anyhow::bail!("no NTP sync")` on line 525. It builds an error from a message and returns it at once.

The second is `{e:#}` in the log call on line 297. An `anyhow::Error` can carry a chain of causes. The plain `{e}` prints the outermost message only, and the alternate form `{e:#}` prints the whole chain on one line.
{% </tip> %}

## Report a type across a layer

When a failure in a service has to reach the user, somebody must write the sentence. The service cannot, because it does not know the language of the screen, and a layer may not use a layer above it. So the service reports a small type of its own, and `app` turns it into words.

`net.rs` has two such types. `Unavailable` tells a job why it runs without a connection. `Failure` is part of the Wi-Fi status and has three variants: `Driver`, `Timeout` and `Other(i32)`. Here is how a driver error becomes one of them.

{% <code file="firmware/src/net.rs" lines="90-97" hl="92-95"> %}{% raw %}
impl From<&EspError> for Failure {
    fn from(e: &EspError) -> Self {
        match e.code() {
            ESP_ERR_TIMEOUT => Failure::Timeout,
            c => Failure::Other(c),
        }
    }
}
{% endraw %}{% </code> %}

The `match` on lines 92 to 95 picks out the one code with a meaning for the user, `ESP_ERR_TIMEOUT`, which the enum's comment explains as a wrong password or a network out of range. Every other code is kept as a number in `Other`. The loop that connects to the network, in `sync_clock`, then writes `Failure::from(&e)` into the status. The `From` is implemented for `&EspError`, so the conversion borrows the error and does not consume it.

TIDAL, the music service the clock can download tracks from, does the same in two steps. Inside the module, `tidal::session::Error` has eleven variants with all the detail. Towards `app` it offers a shorter enum, `Problem`, and the method `Error::problem` maps one onto the other. Three internal errors (`SessionInvalid`, `Unbound`, `NotLoggedIn`) all become `Problem::Session`. The detail is not lost: the caller logs the full `Error` and stores only `e.problem()` in TIDAL's shared state.

The words come last, and in one file. The function `tidal_problem` in `app/texts.rs` has one `match` arm per `Problem`, and each arm picks a sentence in the configured language. Its module comment states the rule: "The services report types; details are in the log." The `match` has no catch-all arm, so a new variant does not compile until it has a sentence.

A report only helps if it always arrives. In `net.rs` that is the job of `JobGuard`, which [chapter 18](@/wakey/18-radio.md) explains.

## Log and carry on

The fourth policy is for everything optional. Chapter 6 showed it for the memory card. The built-in speaker has the same shape.

{% <code file="firmware/src/audio/builtin.rs" lines="65-73" hl="68-71"> %}{% raw %}
    pub fn start(pins: CodecPins, bus: SharedI2c, ioexp: IoExpanders, source: Source) -> Self {
        match Self::try_start(pins, bus, ioexp, source) {
            Ok(s) => s,
            Err(e) => {
                log::error!("speaker: built-in speaker unavailable: {e}");
                Self::default()
            }
        }
    }
{% endraw %}{% </code> %}

`try_start` is the real constructor. It returns `anyhow::Result<Self>`, which is policy two. `start` is the wrapper that `app` calls, and it cannot fail. On an error it writes one log line and returns `Self::default()`, a handle that goes nowhere. The caller needs no special case.

Where a failure is not even worth a line, the code writes `let _ = wifi.stop();`, as at the end of a Wi-Fi session in `net.rs`. `Result` is marked `#[must_use]`, and `let _ =` tells the compiler and the reader that the result is ignored on purpose.

The same policy covers the camera that serves as a light meter (`ambient::start` logs and sets the backlight to full brightness) and two threads: `ambient` and `ui-watch` log when they cannot be started. The touch queue in [chapter 8](@/wakey/08-channels.md#full-queue-drops) applies the policy to single events: a lost move is counted in the log, but a press or a release is not optional and waits for room.

NVS, the small key-value store ESP-IDF keeps in flash, shows two policies side by side. Taking the partition is required, as we saw. Opening the alarm's namespace in it, at the top of `AlarmClock::new`, is optional.

{% <code file="firmware/src/app/alarm.rs" lines="69-72" hl="70-71"> %}{% raw %}
        let nvs = EspNvs::new(nvs, NVS_NAMESPACE, true)
            .inspect_err(|e| log::error!("alarm: NVS: {e}; the state will not survive a reboot"))
            .ok();
        let saved = nvs.as_ref().map(load_state).unwrap_or_default();
{% endraw %}{% </code> %}

{% <tip title="`inspect_err(..).ok()`: log the error, keep an `Option`" id="errors-inspect-err-ok"> %}
Lines 70 and 71 turn a `Result` into an `Option` without losing the error. `inspect_err` runs a closure on the error and passes the `Result` on unchanged. `ok()` then drops the error and gives `Some(nvs)` or `None`.

The field has the type `Option<EspNvs<..>>`, and `follow` saves the state only `if let Some(nvs)`. The log line names the consequence: the state will not survive a reboot.
{% </tip> %}

{% <tip title="A poisoned lock is taken anyway" id="errors-poisoned-lock"> %}
[Chapter 7](@/wakey/07-sharing-hardware.md) explained poisoning: `Mutex::lock` returns `Err` when another thread panicked while it held the lock. The error still contains the guard, and `unwrap_or_else(|e| e.into_inner())` takes it out.

The firmware writes this line for the I2C bus, the expanders' register image, the Wi-Fi status, TIDAL's state and the `WRITE` lock in `config.rs`, among others. So one thread's panic does not take down every other user of the lock. The player's ring in `audio/player.rs` is the exception. It calls `lock().unwrap()`, so there a poisoned lock passes the panic on.
{% </tip> %}

## Two lints for small devices

Two lines in `firmware/Cargo.toml` set lints of Clippy, Rust's linter, to `deny`. The repository's `docs/architecture.md` names them in its section on memory, in one sentence and without a reason: "Lints `large_stack_frames` and `mem_forget` are denied." So we only say what they check.

`clippy::mem_forget` rejects calls to `std::mem::forget`, which throws a value away without running its `Drop`. This firmware does real work in `Drop`: the memory card is unmounted when its handle is dropped. `clippy::large_stack_frames` rejects functions whose local variables need a very large stack frame. The stacks here are small and fixed: `ui-watch` has 4,096 bytes and `touch` has 6 KiB.

For our two questions, the answers are short. The parts work together in failure as they do in success: a lower layer hands up a value, and only `app` knows what it means for the product. A failure that lasts is state like any other. It lives in `Wifi::status` as `Status::Failed`, in TIDAL's state as a `Problem`, or in the `None` of an `Option`. The [last chapter](@/wakey/21-recap.md) collects the tips of all twenty.
