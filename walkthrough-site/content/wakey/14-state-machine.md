+++
title = "Where the state lives: the alarm machine"
description = "A map of all state by owner and lifetime, and the pure state machine at its centre."
weight = 14

[extra]
map = ["core"]
map_note = "The alarm machine is in `wakey-core`, the crate without hardware. The firmware's `app` layer owns one instance of it, which the next chapter shows."
goals = [
  "where each piece of the clock's state is kept, and which pieces survive a reboot",
  "how an enum whose variants carry data rules out meaningless states",
  "why the machine gets the time as an argument, and why it gets it twice",
  "what it means that this logic is tested on the developer's machine",
]
takeaways = [
  "State lives on the `app` thread, in the services' statics and handles, in `config.json` on the card, and in NVS in flash. The Slint properties show it; only the current page lives in one.",
  "`Phase` is an enum whose variants carry their own data, so a ring timeout exists only while an alarm rings.",
  "`Now` holds the time twice. Local time decides when an alarm is due, and Unix time measures how long a snooze or a ringing lasts.",
  "`tick` and `input` take the settings and the time as arguments and do no I/O, so a test on the host can replay a reboot or an hour of ticks in an instant.",
]
+++

"Where does the application's state live?" is one of the two questions of this walkthrough, and this chapter is where we answer it. We start with a map of the places that hold state. Then we spend most of the chapter on the piece in its middle: the state machine that decides whether an alarm rings.

## A map of the state

Earlier chapters showed pieces of state one at a time. The figure puts them together. It sorts them by who can reach a value, and by whether the value survives a reboot.

{{ <svg src="img/state-map.svg" caption="Where the state lives. The two columns on the left are in memory and are lost at a reboot. The column on the right survives one. The arrows show what is loaded at start-up and written back on a change." /> }}

The left column is the state from [chapter 9](@/wakey/09-flags-and-locks.md): atomics in statics, and mutexes behind handles. Several threads share these values, so each needs an atomic or a lock.

The middle column lives on the `app` thread alone, so it needs no lock. `Ctx`, the struct from [chapter 11](@/wakey/11-view-models.md) that the settings pages share, holds the *draft*, the alarm being edited on the screen. `AlarmClock` holds the list of alarms, the settings for ringing, and the state machine.

The right column is what survives a reboot. For the alarms there are two stores. `config.json` on the memory card holds the settings, and the list of alarms is part of them. NVS, the small key-value store that ESP-IDF keeps in flash, holds the state of the machine as JSON.

At the bottom are the properties of the Slint user interface. They are a projection: Rust code on the `app` thread copies state into them so that the screen can show it. One exception is the current page, which lives only in the property `page`.

[Chapter 15](@/wakey/15-state-on-device.md) explains `Ctx`, `AlarmClock` and the two stores in detail. Here we look at what the NVS entry contains: the type `State` in `wakey-core/src/alarm.rs`.

## The state is two fields

Here is everything the machine remembers. Look at the two fields in lines 263 and 264, and at the three variants of `Phase`.

{% <code file="wakey-core/src/alarm.rs" lines="257-297" hl="263-264 281"> %}{% raw %}
/// What survives a reboot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Per alarm id: the last day it finished; it does not ring again that
    /// day.
    pub done: BTreeMap<u32, Date>,
    pub phase: Phase,
}
// ... @278
/// What the state machine is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    /// Alarm `id` rings for `day` until `until` (Unix time), then silenced.
    Ringing {
        id: u32,
        day: Date,
        snoozes: u8,
        until: i64,
    },
    /// Alarm `id`, snoozed `snoozes` times; rings again at `until`.
    Snoozed {
    // ... @296
    },
}
{% endraw %}{% </code> %}

`State` has a map and a phase. The map `done` records, for each alarm id, the last day on which that alarm finished. This is how the machine keeps an alarm to one ringing per day.

`Phase` says what the machine is doing now: nothing, ringing, or snoozed. `id` names the alarm, `day` is the day it rings for, `snoozes` counts how often it has been snoozed, and `until` is a Unix time. While ringing, it is the moment the alarm gives up. While snoozed, it is the moment the alarm rings again. `Snoozed` has the same four fields, so the excerpt leaves them out.

{% <tip title="An enum variant carries the data of its state" id="alarm-phase-data"> %}
The alternative would be a struct with a `kind` field and four more fields beside it. Then the code could write "idle, with a ring timeout", which means nothing.

With the fields inside the variants, `Idle` has no `until` to misread. To get at `until`, the code must `match` on the phase first.
{% </tip> %}

Line 281 marks `Idle` with `#[default]`. That attribute is what allows `#[derive(Default)]` on an enum: it tells the derive which variant is the default one. `State` derives `Default` too, so `State::default()` is an empty map and an idle phase. Both types also derive `Serialize` and `Deserialize`, which is how the state becomes the JSON string in NVS.

## Time comes in from outside, twice

The machine never asks what time it is. The caller tells it, with a small struct named `Now` that has two fields. `local` is the calendar date and time of day in the clock's time zone. `unix` is the number of seconds since 1970, which knows nothing of time zones.

{% <pitfall title="Do not measure a duration with wall-clock time" id="alarm-two-clocks"> %}
An alarm set to 06:30 must ring at 06:30 in summer and in winter, so "is it due?" is asked in local time.

A snooze of nine minutes must last nine minutes. Local time cannot promise that, because on two nights a year it jumps by an hour. So every `until` in `Phase` is a Unix time.
{% </pitfall> %}

## `tick` moves the machine forward

The machine is the struct `Alarms`. It holds a `State` and the `Now` of the previous tick. The firmware calls its method `tick` about once a second. The first half of `tick` moves the current phase forward. Watch the guard at the top and the ring timeout below it.

{% <code file="wakey-core/src/alarm.rs" lines="352-379" hl="355-356 366-368"> %}{% raw %}
    pub fn tick(&mut self, alarms: &[AlarmConfig], ring: &RingConfig, now: Now) {
        let s = &mut self.state;
        s.phase = match s.phase {
            Phase::Ringing { id, day, .. } | Phase::Snoozed { id, day, .. }
                if s.done.get(&id) == Some(&day) =>
            {
                Phase::Idle
            }
            Phase::Ringing {
                id,
                day,
                snoozes,
                until,
            } => {
                if now.unix >= until {
                    s.done.insert(id, day);
                    Phase::Idle
                } else {
                    // A clock set back must not stretch the ringing.
                    let until = until.min(now.unix + ring.timeout_s());
                // ... @378
                }
            }
{% endraw %}{% </code> %}

Look at the signature in line 352 first. The alarms, the ringing settings and the time all arrive as arguments. The function reads no clock and no file, and it starts no sound. It only changes `self`.

Line 354 matches on `s.phase`. `Phase` is `Copy`, so the patterns copy its fields out, and the arms are free to change `s.done` while they decide. Each arm produces the new phase.

The first arm, in lines 355 and 356, covers two variants with one pattern and adds a guard with `if`. It applies when an alarm is ringing or snoozed and the `done` map already says that this alarm has finished for that day. The phase then becomes `Idle`.

The second arm is the ringing alarm. In line 366 the Unix time has reached `until`, so nobody answered for the whole timeout. The alarm is marked done and the phase becomes `Idle`. Otherwise it keeps ringing. Line 371 covers the case of a clock that was set back in the meantime: `until` is never allowed to be further away than one full timeout from now.

The arm for `Snoozed` has the same shape. When `until` has passed, the phase becomes `Ringing` with a fresh timeout. There is one exception: if the snooze ended longer ago than a whole ring timeout, which a comment explains with a late reboot, the alarm is marked done and does not ring.

The second half of `tick` looks for alarms that become due. A helper, `due`, says yes when an alarm's time was crossed between the previous tick and `now.local`. What happens then follows the rule that one alarm is active at a time. If nothing is ringing, the first due alarm starts to ring, and an alarm that was snoozed is marked done for its day. Every other due alarm is marked done for today without ringing. If an alarm is ringing already, all the due ones are marked done.

## `input` is a button press

The second method handles the two buttons of the ringing screen, `Snooze` and `Stop`. Watch what its `match` matches on.

{% <code file="wakey-core/src/alarm.rs" lines="437-459" hl="439 445 453"> %}{% raw %}
    pub fn input(&mut self, ring: &RingConfig, now: Now, input: Input) {
        let s = &mut self.state;
        match (s.phase, input) {
            (
                Phase::Ringing {
                    id, day, snoozes, ..
                },
                Input::Snooze,
            ) if snoozes < ring.max_snoozes => {
                s.phase = Phase::Snoozed {
                    id,
                    day,
                    snoozes: snoozes + 1,
                    until: now.unix + ring.snooze_s(),
                };
            }
            (Phase::Ringing { id, day, .. } | Phase::Snoozed { id, day, .. }, Input::Stop) => {
                s.done.insert(id, day);
                s.phase = Phase::Idle;
            }
            _ => {}
        }
    }
{% endraw %}{% </code> %}

Line 439 builds a tuple of the phase and the input and matches on both at once.

The first arm is "ringing, and Snooze". It has a guard in line 445, so it applies only while snoozes are left. The new phase is `Snoozed`, with the count raised by one and `until` set from `now.unix`. The second arm, in line 453, is "ringing or snoozed, and Stop". It marks the alarm done and returns to `Idle`. Line 457 catches everything else and does nothing. A press on Snooze after the last allowed snooze fails the guard and lands there, so it is ignored.

{% <tip title="Match on a tuple to write a transition table" id="alarm-tuple-match"> %}
When a decision depends on two values, a tuple `(a, b)` puts both into one pattern, so each arm reads as "in this state, on this input". A guard after the pattern adds a condition that a pattern cannot express, and if it fails, matching continues with the next arm.
{% </tip> %}

The diagram collects the transitions from both methods.

{% <diagram caption="The three phases and what moves the machine between them. Not drawn: the first arm of `tick`, and `reconfigured`, which sets `Idle` when the active alarm is deleted or switched off."> %}
stateDiagram-v2
    Idle --> Ringing: tick, an alarm is due
    Ringing --> Snoozed: input Snooze, while snoozes are left
    Snoozed --> Ringing: tick, the snooze is over or another alarm is due
    Ringing --> Idle: input Stop, or tick after the ring timeout
    Snoozed --> Idle: input Stop, or tick long after the snooze ended
{% </diagram> %}

## A reboot, tested without a board

The module's documentation promises that an alarm still rings after a boot if its time is less than the ring timeout ago. On the device, testing that means cutting the power before 06:30 and restoring it at 06:35. Here is that test in the `tests` module at the end of the file.

{% <code file="wakey-core/src/alarm.rs" lines="718-728"> %}{% raw %}
    #[test]
    fn boot_catches_up_within_the_ring_timeout() {
        let c = cfg();
        let mut a = Alarms::new(State::default());
        a.tick(&c, &r(), at_unix(ALARM + 5 * 60));
        assert_eq!(a.ringing(), Some(1));

        let mut a = Alarms::new(State::default());
        a.tick(&c, &r(), at_unix(ALARM + 11 * 60));
        assert_eq!(a.ringing(), None);
    }
{% endraw %}{% </code> %}

`cfg()` is one alarm at 06:30 on every day, `r()` is the default ringing settings with a timeout of ten minutes, and `ALARM` is the Unix time of 06:30 on a Monday. `at_unix` turns a Unix time into a `Now`.

Line 721 creates a new machine, and that is the reboot. It has no previous tick, so `due` accepts an alarm time up to one ring timeout back. Line 722 gives it a first tick five minutes after the alarm time, and the alarm rings. The second half does the same eleven minutes late, and the alarm stays silent.

The test can do this because the time is an argument. If `tick` read the clock itself, a test would have to wait for 06:30. Other tests in this file run an hour of ticks in a loop, jump to the next morning, and step across both daylight saving changes of 2026.

This is what the split into two crates from [chapter 1](@/wakey/01-clock-board-crates.md) buys. `wakey-core` has no dependency on ESP-IDF, so `cargo test` runs these tests on the developer's machine, with no board attached.

For our two questions, this chapter is the main answer to the second. The part that decides whether the clock rings is one value of type `State`, and `tick` and `input` change it without reading the clock, writing to flash or starting a sound. The [next chapter](@/wakey/15-state-on-device.md) shows `AlarmClock`, the type in the firmware that does those three things around the machine.
