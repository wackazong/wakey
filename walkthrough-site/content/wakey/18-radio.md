+++
title = "The radio next door: Wi-Fi sessions, jobs and the C6"
description = "A second chip does the radio work. The firmware switches it on for one session at a time, and `Drop` makes sure that nothing is left half done."
weight = 18

[extra]
map = ["net", "c6", "airdrop", "tidal"]
map_note = "`net`, `airdrop` and `tidal` are services, and `c6` is the hardware module below them. Most of their work is done on one thread, `wifi`."
goals = [
  "what the C6 is, and what one Wi-Fi session on the `wifi` thread does",
  "how a `Drop` implementation guarantees that every job gets an answer",
  "how `Transport` ties the start-up and teardown of a C library to the life of a value",
  "how the user interface and a long-running job share the TIDAL state",
]
takeaways = [
  "The P4 has no radio. The C6 next to it is on for one Wi-Fi session, or while AirDrop is wanted, and off otherwise.",
  "`JobGuard` wraps a job, and its `Drop` calls the job with an error if nobody else called it.",
  "`Outcome::Interrupted(Option<Job>)` carries a job that did not run back to the loop.",
  "`Transport` has a private field, so only `up()` can make one, and its `Drop` shuts the C library down. The driver is dropped before the transport.",
  "`Tidal` is a clone handle over an `Arc<Mutex<Inner>>` and a `Condvar`. The UI thread changes the state and notifies, and the job on the `wifi` thread waits.",
]
+++


The last chapter followed sound to the speakers. This chapter is about the one part of the board that reaches the outside world, the radio. Several ideas from earlier chapters meet here on one thread: the `Job` closures of [chapter 8](@/wakey/08-channels.md), the flags and condition variables of [chapter 9](@/wakey/09-flags-and-locks.md), and the modem of [chapter 7](@/wakey/07-sharing-hardware.md).

## A second chip with two programs

The main processor, the ESP32-P4, has no radio. The board has a second chip for that, the ESP32-C6. The C6 is a complete processor with its own flash memory, and it runs firmware of its own. That firmware is written in C and lives in `firmware/c6/`. We do not tour it, and this section says only what the Rust code needs.

The C6 holds two firmware images, and it runs one of them at a time. The first is esp_hosted, a firmware from the chip's maker with a small addition by this project. With it the C6 is a Wi-Fi adapter for the P4. C components on the P4 side make it look like the normal Wi-Fi driver, so the Rust type `EspWifi` works unchanged. The second image is an AirDrop receiver. With it the C6 receives sound files from iPhones.

Three connections lead from the P4 to the C6. The first is the power supply, which the firmware switches through the `Switch` closure from chapter 7. The second is SDIO, the bus that SD cards also use. esp_hosted talks to the P4 over it, and the code calls this link the *transport*. The third is a UART, a plain serial line. It carries data in one direction only, from the C6 to the P4, and the AirDrop receiver sends the received files over it.

## The `wifi` thread works in sessions

One thread, `wifi`, drives all of this, and its rule is that the C6 is off unless something needs it. A *session* is one period in which the C6 is on as a Wi-Fi adapter. The thread's main function is `Station::run`, a loop that never ends. Here is the top of that loop, with the bookkeeping left out. Watch the highlighted lines: the check for AirDrop, the call to `session`, and one arm of the `match` on its result.

{% <code file="firmware/src/net.rs" lines="267-295" hl="268-269 274-275 291-292"> %}{% raw %}
        loop {
            if airdrop::wanted() {
                airdrop::run(&mut self.c6_link, &self.c6);
            }
            if due || job.is_some() {
                // ... @274
                let result = self.session(&mut modem, JobGuard(job.take()));
                self.c6.off();
                // ... @279
                let wait = match result {
                    // ... @291
                    Ok(Outcome::Interrupted(unrun)) => {
                        job = unrun;
                        // ... @294
                        Duration::ZERO
                    }
{% endraw %}{% </code> %}

Two variables, declared above the loop, decide whether a session starts. `due` is true when the clock should be set from the network. `job` is an `Option<Job>` that holds a job waiting for a connection.

Line 271 starts a session if either is the case. Line 274 runs it, and line 275 switches the C6 off afterwards, whatever the result was. The `match` then decides how long to wait before the next session.

Below the excerpt, an inner loop waits until AirDrop is wanted, the next session is due, or a job arrives in the channel. It waits with `self.jobs.recv_timeout`, so a new job ends the wait at once.

The diagram shows what happens inside one session that was started for a job.

{% <diagram caption="One Wi-Fi session, started by a job. Everything between power on and power off happens on the `wifi` thread."> %}
sequenceDiagram
    participant U as UI thread
    participant W as wifi thread
    participant C as C6
    U->>W: Wifi::run(job)
    W->>C: power on
    W->>C: Transport::up
    Note over W: EspWifi::new, the driver
    W->>C: connect to the network
    Note over W: run the job
    Note over W: wait for the time from the network
    Note over W: stop and drop the driver
    W->>C: drop the Transport
    W->>C: power off
    Note over W: on_time_synced
{% </diagram> %}

## A job always gets an answer

Chapter 8 ended with a promise: a job hears back in every case, with `Ok` or with the reason why there is no connection. Many things can go wrong before a connection exists, and each of them ends `session` early. The firmware does not write "tell the job" at each of these exits. Line 274 above wraps the job in a `JobGuard`, and this is all of it.

{% <code file="firmware/src/net.rs" lines="125-147" hl="141-146"> %}{% raw %}
/// Makes sure a job hears back: dropped unrun, it gets an error.
struct JobGuard(Option<Job>);

impl JobGuard {
    fn run(mut self, result: Result<(), Unavailable>) {
        if let Some(job) = self.0.take() {
            job(result);
        }
    }

    /// The job back, unrun (for a later session).
    fn into_job(mut self) -> Option<Job> {
        self.0.take()
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            job(Err(Unavailable::Failed));
        }
    }
}
{% endraw %}{% </code> %}

Its method `run` takes the job out of the option with `take` and calls it. The highlighted `Drop` does the same, with one difference: it passes the error `Unavailable::Failed`.

Rust calls `drop` whenever a `JobGuard` goes out of scope, on every path. If `run` was called before, the option is empty and `drop` does nothing. Now look at `session`, which chapter 7 showed. Its second statement is `self.c6.connect()?`. When the C6 does not answer, the `?` returns from the function. The parameter `job` is dropped at that moment, and so the job is told `Failed`. No line at that exit mentions the job.

{% <tip title="A guard whose `Drop` finishes the work" id="net-job-guard"> %}
When something must happen on every way out of a function, put it in the `Drop` of a value the function owns. The compiler then adds the call to each exit, including exits that a later change adds.

The `Option` inside is part of the pattern. A `FnOnce` closure is used up when it is called, but `drop` only gets `&mut self`. `take()` moves the job out and leaves `None` behind.
{% </tip> %}

There is one case in which the job should not run at all, and for it `JobGuard` has `into_job`. To see why, we look at what `session` returns. It is an `Outcome`, an enum with three variants: `Synced`, `Scanned` for a session without credentials, and `Interrupted(Option<Job>)`.

The third variant is the interesting one. A session returns it when it ends early for AirDrop. If the job had not run by then, telling it `Failed` would be wrong, because nothing failed. So the code returns `Outcome::Interrupted(job.into_job())`. `into_job` empties the guard, which makes its `Drop` harmless, and the job travels back to the loop inside the result. Lines 291 and 292 of the first excerpt unpack it with `job = unrun`, and the job runs in the next session, after AirDrop. So the enum does more than name an ending. It carries what is left to do.

## `Transport`: C start-up and teardown as a value

The transport is the SDIO link to the C6, and it belongs to a C library. That library wants calls to start it and one call to stop it, and it wants the stop before the power goes off. The doc comment gives the reason: a C6 that is switched off while the transport is up is found "unresponsive" by the next session. The module `c6` turns this rule into a type. The excerpt skips the body of `up`, which makes the start-up calls.

{% <code file="firmware/src/c6/mod.rs" lines="208-245" hl="208 221 239"> %}{% raw %}
pub struct Transport(());

impl Transport {
    fn up() -> anyhow::Result<Self> {
        // ... @221
        Ok(Self(()))
    }
// ... @232
}

impl Drop for Transport {
    fn drop(&mut self) {
        // ... @237
        // SAFETY: the link is up (`up`), nothing uses the transport any
        // more (the driver is gone before its session's transport).
        let err = unsafe { sys::esp_hosted_deinit() };
    // ... @244
    }
}
{% endraw %}{% </code> %}

Line 208 declares a struct with one field of type `()`, the empty tuple. The field holds no data, but it is private. Code outside this module cannot write `Transport(())`, so the only way to get a `Transport` is `C6::connect`, which calls `up`.

`Drop` is the other half. Line 239 shuts the library down when the value goes away. If a start-up call fails, `up` returns an error before line 221 creates the value. No `Transport` exists then, so no `drop` runs. The doc comment says that this is intended: a half-started library must not be torn down.

The safety comment on `drop` claims that the Wi-Fi driver is already gone. The function `session`, whose first lines chapter 7 showed, makes that true.

`session` creates `transport` first and the driver `wifi` second. Its last statement moves `wifi` into `sync_clock` or `scan_once`, the two functions that do the session's work. A value that is moved into a function is dropped by the time that function returns. `transport` stays a local variable of `session` and is dropped when `session` itself returns. Only then does the loop call `self.c6.off()`.

{% <pitfall title="Drop order is part of the program" id="net-drop-order"> %}
With a C library behind them, two values may have to go in a fixed order. Local variables are dropped in the reverse order of their declaration, and a value moved into a function is dropped inside that function.

Both rules give the right order in `session`, but nothing in the types enforces it. That is why `session` has a comment at this place: "The driver (dropped by these, `esp_wifi_deinit`) goes before the transport."
{% </pitfall> %}

## Handing the C6 to AirDrop

The C6 runs one image at a time, so it is either a Wi-Fi adapter or an AirDrop receiver. While the Files tab of the sound page is shown, the flag `airdrop::WANTED`, a `static` atomic like those of chapter 9, is true. The `wifi` thread reads it with `airdrop::wanted()` in many places, among them the top of its loop and the wait for the time.

When the flag turns true, a running session ends with `Outcome::Interrupted` and the C6 is switched off. The loop comes round to line 268 and calls `airdrop::run`. That function has the C6 start the receiver, then reads the UART until the flag turns false, and returns with the C6 off. Jobs that arrive meanwhile stay in the channel.

## TIDAL: a job and its shared state

The heaviest user of sessions is `tidal`, the service that downloads tracks from the TIDAL music service to the card. It also has the firmware's largest piece of shared state. Its handle, `Tidal`, has three fields: `inner`, an `Arc<Mutex<Inner>>`, then `kick`, an `Arc<Condvar>`, and a `Wifi` handle.

`inner` has the shape that chapter 9 showed for `Wifi::status`, a mutex behind a handle that is cheap to clone. The difference is size. `Inner` has twenty fields, among them the playlist, the tracks on the card and the progress of the download under way.

Two threads work on it. The UI thread calls methods such as `set_selected` when the selection of tracks changes. The method locks `inner`, changes the selection and, if tracks are missing, a login exists and no job is under way, queues one through `Wifi::run`. That job runs on the `wifi` thread for a long time. It downloads one missing track after the other and locks `inner` only for a moment at a time.

The third field, `kick`, connects the two. When the job has nothing to download while the settings pages are open, it waits on this condition variable, with a timeout, instead of asking again and again. `set_selected` calls `self.kick.notify_all()` while it still holds the lock, and the job wakes and looks again. The job checks for missing tracks under that same lock before it waits, so the lost wake-up from chapter 9 cannot happen.

For our two questions: the parts work together through one thread that owns the radio: others send it closures and set a flag, and it never calls upwards. And some state lives in values that stand for something under way: dropping a `JobGuard`, a `Transport` or a driver ends it. The [next chapter](@/wakey/19-unsafe.md) collects every `unsafe` block.
