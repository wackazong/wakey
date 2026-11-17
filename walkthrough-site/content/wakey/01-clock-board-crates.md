+++
title = "A clock, a board and two crates"
description = "What the device does, what it is built on, and why its logic is split into a crate that never sees hardware."
weight = 1

[extra]
map = ["core"]
map_note = "`wakey-core` is a crate of its own beside the firmware's layers. The layers are the subject of the next chapter."
goals = [
  "what the clock does, seen from the bedside table",
  "which pieces of hardware the firmware has to drive",
  "how the repository is divided into two crates, and what decides where code goes",
  "the two questions that run through every chapter",
]
takeaways = [
  "Wakey runs on an M5Stack Tab5. Its main processor, the ESP32-P4, has two cores and no radio. A second chip, the ESP32-C6, does the radio work.",
  "`wakey-core` holds everything that can be computed without hardware. It builds and is tested on your computer, and it forbids `unsafe`.",
  "`firmware` holds everything that touches the board. It uses `wakey-core` as a path dependency, and each crate is a Cargo workspace of its own.",
]
+++

This walkthrough reads the source code of Wakey, a bedside alarm clock written in Rust. Before we open the first function, this chapter shows what the clock does, which hardware it runs on, and how the repository is divided.

It also introduces the two questions that we will ask in every chapter. The first is **how the parts work together**. The second is **where the application's state lives**. State means everything the program has to remember: the list of alarms, whether one is ringing, which page is on the screen.

## What the clock does

We start with the device as its owner sees it. The pictures below are not photographs. A tool in the repository, `tools/ui-preview`, renders them from the firmware's own user interface code.

{% <gallery caption="The clock page with a night sky, and while an alarm rings."> %}
{{ <figure src="img/ui-clock.jpg" alt="Clock page: large digits on a dark purple sky, the date and the next alarm." /> }}
{{ <figure src="img/ui-clock-ringing.jpg" alt="Clock page while ringing: Tap to snooze, and a button Hold: alarm off." /> }}
{% </gallery> %}

The clock page shows the time, the date and the next alarm, on a sky that follows the sun. When an alarm rings, a tap anywhere on the screen snoozes it. To stop it, you have to hold the button at the bottom for one and a half seconds.

{% <gallery caption="The settings pages: the alarm list, one alarm, and its sound."> %}
{{ <figure src="img/ui-alarm-list.png" alt="Alarm list: rows with a time, a name, weekdays and a switch." /> }}
{{ <figure src="img/ui-alarm-page.png" alt="Alarm page: wheels for hour and minute, a name, weekday chips and a sound." /> }}
{{ <figure src="img/ui-sound-tidal.png" alt="Sound page: a list of TIDAL tracks, a volume slider, fade-in choices and Preview." /> }}
{% </gallery> %}

A tap on the alarm line opens the alarm list. Each alarm has its own time, weekdays, name and sound. The sound is a file on the memory card, or a track from a playlist on the music service TIDAL that the clock downloads to the card, or a file sent from an iPhone by AirDrop.

Two more things happen without any touch. The clock measures the light in the room and dims the screen in the dark. And it switches its Wi-Fi chip on only when it needs the network.

## The board

Now we look at what the firmware has to drive. Wakey runs on a finished board, the M5Stack Tab5, and the repository's `docs/hardware.md` lists what is on it.

{{ <svg src="img/hardware.svg" caption="The Tab5 as the firmware uses it. Six parts have a connection of their own to the main processor, and the label on each line names its kind. The chips in the bottom row share one bus." /> }}

In the middle is the **ESP32-P4**, the processor that runs our Rust code. It has two cores, which matters from [chapter 5](@/wakey/05-threads.md) on, where we see which thread runs on which core. It also has 32 MB of PSRAM, which is external memory, much larger than the processor's internal RAM.

The P4 has no radio. Wi-Fi comes from a second processor on the board, the **ESP32-C6**, which runs firmware of its own. [Chapter 18](@/wakey/18-radio.md) is about this neighbour.

The bottom row is different. These chips share the I2C bus, two wires on which the processor addresses one chip at a time. Among them are two IO expanders, which give the processor extra pins for power switches and reset lines, and the RTC, the real-time clock that keeps the time while the device is off. Several threads want to talk to these chips, so the bus must be shared, and [chapter 7](@/wakey/07-sharing-hardware.md) shows how.

{% <concept title="ESP-IDF, and Rust with `std` on a microcontroller"> %}
Many embedded Rust projects are `no_std`: they run without an operating system and without the standard library. Wakey is not one of them.

The firmware is built on ESP-IDF, the software framework of Espressif, the company that makes both chips. ESP-IDF brings drivers, a network stack, a file system and a small operating system called FreeRTOS, which provides threads. Rust's standard library runs on top of it.

So `std::thread`, `std::sync::Mutex` and `std::fs` all work on this board.
{% </concept> %}

## Two crates

Now we open the repository. Two of its folders hold the code of the clock. `wakey-core/` is a library crate with the logic that needs no hardware. `firmware/` is the crate that runs on the board. It builds a library named `wakey` and two programs: the clock, and a test bench for the user interface.

The documentation at the top of `wakey-core/src/lib.rs` states how the two are divided. Read the highlighted paragraph first, then the list.

{% <code file="wakey-core/src/lib.rs" lines="1-25" hl="4-6"> %}{% raw %}
//! Hardware-independent logic of Wakey, an alarm clock for the M5Stack
//! Tab5.
//!
//! Everything here builds and is tested on the host (`cargo test` in this
//! directory); the firmware links it for the ESP32-P4. Nothing here knows
//! about ESP-IDF, chips or threads.
//!
//! - [`airdrop`]: files received by AirDrop (the C6's frames, names, part
//!   files).
//! - [`alarm`]: alarm settings and the ringing state machine.
//! - [`audio_file`]: audio file readers (WAV, AIFF, FLAC, MP3, AAC or ALAC
//!   in M4A, ADTS AAC) → stereo i16.
//! - [`config`]: the `config.json` model.
//! - [`datetime`]: calendar dates ↔ Unix time.
//! - [`lang`]: the screen's texts per language (`lang/*.json`).
//! - [`mix`]: sample operations for the built-in speaker (mono, mixing,
//!   keep-alive).
//! - [`resample`]: sample-rate conversion to the speaker's 48 kHz.
//! - [`scroll`]: touch scrolling physics (drag, fling, rubber band, spring).
//! - [`sky`]: the sun's elevation and the clock face's sky gradient.
//! - [`tidal`]: TIDAL login, playlist sync and track downloads (requests
//!   and responses only, no I/O).
//! - [`tone`]: the built-in alarm tone, volume curve, fade-in and wheel
//!   click.
//! - [`uac`]: USB Audio Class 1.0 descriptor parsing for USB speakers.
{% endraw %}{% </code> %}

Lines 4 to 6 make two promises. The crate builds on the host, which is the computer you develop on, and nothing in it knows about chips or threads.

The list below shows how far that goes. The crate holds the decision when an alarm rings (`alarm`), the physics of a scrolling list (`scroll`), the colours of the sky (`sky`) and even the TIDAL protocol. Look at the words in brackets on lines 21 and 22: "requests and responses only, no I/O". The crate describes which request to send and reads the answer. Sending it over Wi-Fi is the firmware's job.

The project's architecture document gives the rule in one sentence: anything that can be computed without hardware goes into `wakey-core` and gets host tests. The reason is practical. Code in `firmware` can only be tried on the board. Code in `wakey-core` is tried with `cargo test`, with no board on the desk.

## Two manifests, two workspaces

The two `Cargo.toml` files show how the crates are connected. We begin with the firmware's manifest. Watch the two highlighted lines, an empty `[workspace]` table and the dependency on `wakey-core`.

{% <code file="firmware/Cargo.toml" lines="9-36" hl="11 35" lang="toml"> %}{% raw %}
# The firmware is its own Cargo workspace: its target, build-std and
# toolchain must not leak into host crates.
[workspace]
# ... @32
[dependencies]
# Hardware-independent logic (dates, audio files, resampling, descriptors,
# config, scroll physics), tested on the host.
wakey-core = { path = "../wakey-core" }
esp-idf-svc = { version = "0.53", features = ["std", "binstart", "alloc", "embassy-time-driver", "embassy-sync", "critical-section"] }
{% endraw %}{% </code> %}

Line 11 makes the firmware a Cargo workspace of its own. The comment above it says why: the settings of its build must not reach crates that are built for the host.

Line 35 is the whole connection between the two crates. It is a path dependency, so Cargo compiles the source in `../wakey-core` again as part of the firmware, this time for the P4. The same code therefore exists in two builds: one for your computer, where the tests run, and one for the device.

Line 36 names `esp-idf-svc`, the Rust crate through which the code reaches ESP-IDF.

{% <tip title="An empty `[workspace]` table keeps a crate to itself" id="own-workspace"> %}
Crates in one repository often share one workspace. A `[workspace]` table with nothing in it does the opposite. It makes `firmware` the root of a workspace that contains only itself.

The firmware's build is unusual. It is built for a RISC-V target, with a pinned nightly compiler, and it compiles the standard library itself (`build-std`). None of that should apply to a crate that is meant to build with the default toolchain.
{% </tip> %}

The manifest of `wakey-core` has the same empty `[workspace]` table, and its list of dependencies names no ESP crate. It ends with two lines worth seeing.

{% <code file="wakey-core/Cargo.toml" lines="38-39" hl="39" lang="toml"> %}{% raw %}
[lints.rust]
unsafe_code = "forbid"
{% endraw %}{% </code> %}

This is a lint setting, which the tip below explains. For our tour it means that we will find no `unsafe` code in `wakey-core`.

{% <tip title="`unsafe_code = \"forbid\"` for a whole crate" id="forbid-unsafe"> %}
A `[lints]` table in a manifest applies to the whole crate. Line 39 sets the lint `unsafe_code` to `forbid`, so any `unsafe` block in `wakey-core` is a compile error.

`forbid` is stricter than `deny`. A `deny` can be switched off again for one function with `#[allow(unsafe_code)]`. A `forbid` cannot.

So every `unsafe` of the clock is in `firmware`, where the hardware is, and [chapter 19](@/wakey/19-unsafe.md) goes through it.
{% </tip> %}

## A test that runs on your computer

To see what the split buys, we look at one test from the module `datetime`, which converts between Unix time and calendar dates.

{% <code file="wakey-core/src/datetime.rs" lines="133-142"> %}{% raw %}
    #[test]
    fn known_date() {
        let d = DateTime::from_unix(1_794_265_200); // Monday 2026-11-09 23:00 UTC
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.weekday),
            (2026, 11, 9, 23, 0, 1)
        );
        assert!(d.plausible());
        assert_eq!(d.hms(), "23:00:00");
    }
{% endraw %}{% </code> %}

Line 135 turns a number of seconds into a `DateTime`, and lines 136 to 139 compare its fields with the date in the comment. The last value, `1`, is the weekday, counted from Sunday as 0. Line 140 calls `plausible`, which checks that every field is in its valid range.

On the device, the same `plausible` helps to decide whether the time read from the real-time clock chip can be trusted. Here it is checked without that chip. `wakey-core` has more than a hundred tests of this kind.

## The two questions, and the tour ahead

We can now give a first answer to each question. At the coarsest level, the parts work together as two crates with a one-way dependency: the firmware calls the pure logic and never the other way round. As for the state, `wakey-core` only describes it. It defines what an alarm is, what `config.json` contains and how a ringing alarm changes when you tap. Where those values are kept on the running device is decided in the firmware.

The tour goes from the top down. Chapters 2 to 5 show the firmware's layers, how it starts, and everything that runs at the same time. Chapters 6 to 9 follow the hardware to its owners and show how threads talk to each other. Chapters 10 to 13 cover the user interface. Chapters 14 to 16 answer the question about state and follow one touch through the whole system. Sound and the radio come next, and the last chapters collect every `unsafe`, the error handling and the Rust idioms we met.

The [next chapter](@/wakey/02-layers.md) opens the `firmware` crate and looks at its layers.
