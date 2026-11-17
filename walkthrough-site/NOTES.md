# Notes from reading the code

Bugs, oddities and rough edges found while writing the walkthroughs. The project's source is not changed here; this list is for its maintainer. Each entry names the commit it was seen at and gives file and line.

## Wakey, at `0fdbf78`

Each entry was assessed afterwards by reading the code. The ones that are real issues, and the comments that were wrong, were fixed at `888a2a1`, the commit the walkthrough now describes; those entries say so, and [docs/roadmap.md](../docs/roadmap.md#to-check-on-a-device) lists the fixes for a test on the device. The line numbers in the entries are those of `0fdbf78`. The others were judged intended or harmless (for example: the slide discarding queued touches, the blocking `set_streaming`, the expander lock, the `Relaxed` load of `armed`, the polling loops, the bounded card-writer channels, `Rx8130` being `Clone`).

None of these was tried on the device. They are places where the code can lose something, relies on something that is not checked, differs from its comment, or is surprising. The first group and about half of the rest were read in the source by the editor; the others are reported by the chapter writers and fact-checkers with the lines they read.

### Worth a look first

These three could cost a user something. All three are fixed now.

- **`firmware/src/board/sd.rs:33-39` with `firmware/src/config.rs:43-46`: a power cut while `config.json` is replaced can lose the settings.** `sd::replace` writes `config.tmp`, removes `config.json`, then renames. FAT cannot rename onto an existing file, as the comment says, so for a moment there is no `config.json`. If power fails then, the next boot finds no file, takes that for a first setup and runs on defaults; nothing looks for a leftover `config.tmp`. The next alarm save then writes a new file from the defaults, without the Wi-Fi and TIDAL settings. **Fixed at `888a2a1`.**
- **`firmware/src/audio/player.rs:263-267`: a sound file of unknown length is not repeated.** At the end of a file `decode_from` rewinds, except when `format.frames == 0`, where it returns `Ok`. `wakey-core/src/audio_file.rs:44` documents 0 as "unknown, e.g. MP3". `Player::ring` plays the built-in tone only on an error, so an alarm with such a file would play it once and then be silent while it still rings. **Fixed at `888a2a1`.**
- **`firmware/src/net.rs:327-338`: one narrow path drops a waiting job without an answer.** After a session ends as `Interrupted(Some(job))`, the job waits in the local `job`. The inner wait loop does not look at it. If AirDrop is no longer wanted and the display is asleep, the loop sits in `recv_timeout`, and a newly arrived job overwrites `job` on line 336. The old one is a bare `Job` there, not a `JobGuard`, so it is dropped unrun and uncalled. **Fixed at `888a2a1`.**

### Could misbehave under load

- **`firmware/src/ui/input.rs:83-89` and `:146-153`: a full touch queue drops events silently, and the thread's own state moves on.** `EVENTS` has 32 places. `try_send` fails when they are all taken, the result is discarded with `let _ =`, and nothing is logged. `release` sets `*down = false` whether or not the `Up` was queued, so a dropped `Up` is never sent again. Slint is told about a release only when an `Up` arrives (`ui/render_loop.rs:75-78`), so it would keep a pressed pointer until the next touch. The queue fills only if the render loop takes nothing out for 32 events, which is at least 320 ms of finger movement; the comments speak of frames of up to 90 ms. **Fixed at `888a2a1`.**
- **`firmware/src/ui/transition.rs:191`: after a slide every queued touch event is discarded.** `while input::EVENTS.try_receive().is_ok() {}` also removes a `Down` or an `Up` that arrived during the slide. Whether the events that follow can reach Slint without their `Down` was not traced to the end.
- **`firmware/src/audio/player.rs:167-171`: `Player::start` waits for the previous decoder in a 10 ms polling loop on the calling thread.** The caller is the UI thread (`AlarmClock::follow` and `preview`). The wait ends when the old decoder has seen `stop` and the ring has drained or been cleared. **Fixed at `888a2a1`.**
- **`firmware/src/audio/builtin.rs:143`: `set_streaming` uses a blocking `send` on the 8-place channel.** Clicks use `try_send` and are dropped when the channel is full; the stream command must not be dropped, so it waits. If the mixer thread is busy and the channel is full, the UI thread waits with it.
- **`firmware/src/board/ioexp.rs:135-142`: `with` holds the lock on the register images while it waits for the I2C bus and for the transfer.** Every other user of the expanders waits behind a slow bus (the transfer timeout is 50 ms).

- **`firmware/src/audio/player.rs:94-98`: the fade-in counter is per source, the restart flag is shared.** Each of the two `Source` closures has its own `frames`; `restart_fade` is one flag, taken by whichever source is called first after a sound starts. If the output changes during a sound (the USB speaker is unplugged), the other source goes on with its old count, so the fade may start again or be skipped. **Fixed at `888a2a1`.**
- **`firmware/src/app/alarm.rs:275-278` with `:324-328`: a failed write of the alarm state to NVS is not tried again.** `save_state` only logs, and `saved` is updated all the same, so the next attempt comes with the next change of state. **Fixed at `888a2a1`.**
- **`firmware/src/display/ppa.rs:395-403`: `wait_idle` spins until the PPA is done and never gives up.** A stall is logged after 20,000,000 spins; a PPA that hangs would hang the UI thread for good. **Fixed at `888a2a1`.**
- **`firmware/src/ui/render_loop.rs:234-249`: a wake request made while the display sleeps is taken on the next pass**, after the idle wait of up to 100 ms. An alarm that starts to ring lights the screen that much later.
- **`firmware/src/ui/render_loop.rs:236-242`: while asleep, only a `Down` wakes.** A finger that is already down when the display goes to sleep cannot wake it until it is lifted and put down again.

### Comment and code differ

- **`firmware/src/board/mod.rs:5`: "Required chips panic with their name" holds for three kinds of chip only.** The IO expanders (`board/ioexp.rs:100-105`), the RTC (`board/mod.rs:80-81`) and the touch controller (`board/mod.rs:154`) panic with a message that names them. Failures of the I2C driver, of the writes to the expanders, of the backlight PWM and of the display go up through `?` and end at the unnamed `expect("board init")` in `app/mod.rs:179`. **Fixed at `888a2a1`.**
- **`firmware/src/ui/power.rs:37` and `:42`: `sleep` and `wake` are documented as "(any thread)", but every caller is on the `app` thread** (`app/power.rs`, `ui/render_loop.rs:239`). Only `ASLEEP` is read from other threads. **Fixed at `888a2a1`.**
- **`firmware/src/board/i2c.rs:3`: the module comment lists "codecs and IMU" on the bus.** The firmware has one codec driver and none for an IMU.
- **`firmware/Cargo.toml:7` and `firmware/src/lib.rs:1-2`: "ESP-IDF (std) + Embassy".** True, but a reader who expects an Embassy-style design will be misled: the executor runs one task (two in the UI bench), and all other concurrent work is on `std` threads.
- **`firmware/Cargo.toml:33-35` and `firmware/src/lib.rs:19-21`: the comments that list what `wakey-core` holds are behind `wakey-core/src/lib.rs:8-25`.** They leave out the alarm state machine, the sky, the texts and AirDrop. **Fixed at `888a2a1`.**

- **`firmware/src/ui/render_loop.rs:86-88`: `MAX_SLEEP` is explained by "requests from other threads"**, but no other thread makes one at this commit (see `ui/power.rs` above). **Fixed at `888a2a1`.**
- **`firmware/src/display/mod.rs:581-583`: `present`'s comment says the interrupt handler "cannot interleave" on the same core.** It can still run between the buffer switch and the read of the counter; lines 603-604 handle that case. What one core rules out is the handler being half done while `present` reads. **Fixed at `888a2a1`.**
- **`firmware/src/app/clock_screen.rs:5-6` and `:129`: the comments credit `set_changed!` with keeping repaints small.** The vendored Slint already skips the dirty mark when a property is set to an equal value (`vendor/i-slint-core/properties.rs:1047-1054`), so for these properties the macro's comparison looks redundant. (For model rows it is not: `VecModel::set_row_data` reports a change regardless.)
- **`firmware/src/app/alarm_page.rs:5` and `:117`: "cancelling drops" the draft**, but Cancel only shows the list; the draft stays in `Ctx` until the next `open` overwrites it. Harmless.
- **`docs/hardware.md:20`: the pin table's note on the I2C bus omits the camera and names an IMU**, while the device table further down lists the camera. **Fixed at `888a2a1`.**

### Relies on something that is not checked

- **`firmware/src/lib.rs:4`: "none uses a layer above it" is a rule in a comment.** All modules are `pub mod` in one crate, so the compiler does not enforce it. It holds at this commit.
- **`firmware/src/app/texts.rs:25`: `get()` before `init()` fixes the default language for good.** `get_or_init` fills the `OnceLock`, and a later `init` only logs an error. The start-up order prevents this today.
- **`firmware/src/audio/player.rs:181-185`: the comment relies on the decoder's priority being 5, but the call sets none.** It is ESP-IDF's default for `std` threads, which `sdkconfig.defaults` does not pin.
- **`firmware/src/runtime.rs:117-119`: if the spawn configuration cannot be set, the thread is spawned anyway**, with whatever configuration is in force, so it can end up on another core or priority with only a warning in the log. **Fixed at `888a2a1`.**
- **`firmware/src/board/ioexp.rs:137-141`: any address other than `IOE_A` selects the image of expander B.** Harmless today: `with` is private and is called with the two constants only.
- **`firmware/src/board/rtc.rs:36-40`: `Rx8130` is `Clone`.** That the RTC has one owner (the time-sync closure) is how `app` uses it, not something the type enforces.

- **`firmware/src/ui/platform.rs:34-39` and `:69-78`: a window and its component are paired only by the order of calls.** Preparing two windows and creating their components in the other order would swap them without an error.
- **`firmware/src/net.rs:362-363` and `firmware/src/c6/mod.rs:237-238`: the Wi-Fi driver must be dropped before the transport**, and that order rests on which function each value is moved into, plus a comment.
- **`firmware/src/audio/player.rs:90-91`: "only one may take from it at a time" is a comment.** Both speakers hold a source over the same ring; `Output` keeps them apart by sending "off" before "on", but does not wait for the first speaker's thread to stop.
- **`firmware/src/tidal/playlist.rs:226` (and `speaker`, `uac`, `usb-lib`, `ambient`): threads without a core or priority run at priority 5 on any core**, so they can run on core 0 above the UI's priority 1, the situation the decoder is pinned to core 1 to avoid (`audio/player.rs:180-182`). **At `888a2a1` the `cover` thread is pinned (core 1, priority 1); the others are unchanged.**
- **`firmware/src/ui/input.rs:113-119`: a `touch` thread that cannot be started is only logged**, which leaves a clock without input, while the diagnostic `task-stats` thread panics in the same case (`taskstats.rs:90`). **Fixed at `888a2a1`.**
- **`firmware/Cargo.toml:27`: `undocumented_unsafe_blocks` is a warning, and the firmware's Clippy does not run in CI** (`CONTRIBUTING.md:22`, `.github/workflows/ci.yml:54-65`), so nothing enforces the `SAFETY` comments automatically.
- **`firmware/src/display/mod.rs:213`: `Display` has no `Drop`.** Its LDO channel, bus and panel handles are not released if it is ever dropped; in the product it never is.

### Small things

- **`firmware/src/app/mod.rs:168-179`: the `..` in the `Board` pattern matches nothing.** The pattern names all nine fields. It would silently drop a field added to `Board` later, and dropping a handle can switch hardware off, as with `sd`. **Fixed at `888a2a1`.**
- **`firmware/src/runtime.rs:76`, `:93` and `:107`: the stack size is set twice per spawn**, in the `ThreadSpawnConfiguration` and on `std::thread::Builder`.
- **`firmware/src/audio/player.rs:115`: `armed` is loaded with `Relaxed`** and stored with `Release` everywhere else (lines 178, 192, 317); the file's other flags pair `Acquire` with `Release`.
- **`firmware/src/audio/player.rs:193-196`: the decoder waits for the ring to drain in a 20 ms polling loop**, although the ring has a `Condvar`.
- **`firmware/src/audio/uac/mod.rs:98` and `:108`: a failed `send` to the `uac` thread is ignored**, while `Wifi::run` (`net.rs:167-169`) answers the same case with an error for the job.
- **`firmware/src/tidal/card_writer.rs:56-57`: both channels are bounded at 16, and only 16 blocks exist**, so neither bound can be reached. The back-pressure comes from waiting for a free block (`:131`).
- **`firmware/src/board/ioexp.rs:22-49`: `Pin` has two inputs, `HpDet` and `ChgStat`, and no function reads a pin.** **At `888a2a1` the enum's comment says that the two inputs are not read.**
- **`firmware/src/ui/dim.rs:33-34`: the factor and the epoch change in two steps**, so a reader can see the new factor with the old epoch for a moment. The next check, 100 ms later, catches it. **Fixed at `888a2a1`.**
- **`firmware/src/app/alarm_list.rs:80-83` and `:87-90`: a failed save from the list's switch or swipe is only logged**, while the alarm page shows "save failed" for the same error (`app/alarm_page.rs:89-95`). **Fixed at `888a2a1`.**
- **`firmware/src/ui/render_loop.rs:104-112`: `Stage::ALL` repeats the enum's variants by hand.** A variant added to the enum and not to the array would be reported as `Start`. **Fixed at `888a2a1`.**
- **`firmware/src/ui/render_loop.rs:231-250`: the asleep branch never calls `enter`**, so a stall report during sleep names the stage of the last pass awake.
- **`firmware/src/audio/builtin.rs:134-136`: `wake()` uses the dropping `try_send`.** Harmless: the next click switches the amplifier on itself.
- **`wakey-core/src/alarm.rs:413-431`: when two alarms become due in the same tick, the one earlier in the list rings** and the other is marked done; the rule is tested (`same_time_rings_once`) but not stated.
- **`wakey-core/src/alarm.rs:355-359`: the first arm of `tick` handles an active alarm that is already marked done for its day.** No path in `tick`, `input` or `reconfigured` was found that produces that state, so it looks like a guard against an inconsistent saved state; no comment says so. **Fixed at `888a2a1`.**
