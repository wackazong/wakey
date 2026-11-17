+++
title = "State on the device: alarms, drafts and two stores"
description = "`AlarmClock` and its one `follow` function, the draft being edited, and what survives a reboot."
weight = 15

[extra]
map = ["app", "config", "core"]
map_note = "`AlarmClock` lives in `app`. It wraps the machine from `core` and saves the alarm list through the `config` service."
goals = [
  "how `AlarmClock` connects the pure alarm machine to time, storage and sound",
  "why one function, `follow`, is the only place where a change of state has an effect",
  "how an alarm is edited as a draft, and how the page knows whether the draft has changed",
  "why the device keeps two stores, and how each one is written",
]
takeaways = [
  "Every method of `AlarmClock` that can change the machine's state calls `follow`. It compares the state with the one it saved last, and the ringing alarm with the one that sounds, and it writes to NVS and commands the player only on a difference.",
  "`set_alarms` writes `config.json` first and changes memory afterwards, so a failed write leaves the clock exactly as it was.",
  "An alarm is edited as a copy, the draft in `Ctx`. A second copy, the original, tells `changed` whether there is anything to save.",
  "Settings a person may edit are in `config.json` on the memory card. The machine's own state is in NVS, in the board's flash.",
]
+++

The [previous chapter](@/wakey/14-state-machine.md) read `wakey_core::alarm::Alarms`, the state machine that decides when an alarm rings. That machine is pure. It does not know what time it is, it cannot make a sound, and it forgets everything when the power goes. In this chapter we read the code that gives it all three, a struct called `AlarmClock` in `firmware/src/app/alarm.rs`. We then look at the draft, a copy of one alarm that the settings pages edit, and at the two places where the device keeps what must survive a reboot.

## What `AlarmClock` owns

The struct's fields fall into three groups. The first is the configuration: `alarms`, the list of `AlarmConfig` values, and `ring`, the settings for ringing and snoozing. The second is the machine, `machine: Alarms`. The third connects it to the outside: `nvs` for storage, `player` and `output` for sound, and a few fields that remember what the struct did last.

There is one `AlarmClock`. It lives on the UI thread, in `Ctx`, the struct that the settings pages share ([chapter 11](@/wakey/11-view-models.md)). Three kinds of event reach it. The clock screen's timer calls `tick` once a second. The buttons on the ringing screen call `input`. The settings pages call `save`, `delete` and `set_enabled`, which all end in `set_alarms`. Each of these methods passes the event to the machine, with the current time from the `clock` module, and then calls the same private function.

## `follow`: where a change of state has an effect

That function is `follow`, and we read it in two halves. The first half is about storage. Watch what the code compares before it writes.

{% <code file="firmware/src/app/alarm.rs" lines="275-301" hl="277-278 282-285 292 298"> %}{% raw %}
    /// Saves a changed state and makes the speaker follow it.
    fn follow(&mut self) {
        let state = self.machine.state();
        if *state != self.saved {
            if !self.save_failed {
                log::info!("alarm: {:?}", state.phase);
            }
            let written = match &self.nvs {
                Some(nvs) => nvs.set_str(NVS_KEY, &state.to_json()),
                None => Ok(()),
            };
            match written {
                Ok(()) => {
                    if self.save_failed {
                        log::info!("alarm: state saved after all");
                    }
                    self.save_failed = false;
                    self.saved = state.clone();
                }
                Err(e) => {
                    if !self.save_failed {
                        log::error!("alarm: saving the state: {e}; trying again every tick");
                    }
                    self.save_failed = true;
                }
            }
        }
{% endraw %}{% </code> %}

Line 277 asks the machine for its `State`. That is the `Phase` and the map of finished days from chapter 14. Line 278 compares it with `self.saved`, the copy that `AlarmClock` keeps of the state that is in NVS. Only when the two differ do lines 282 to 285 write the state.

If the write succeeds, line 292 makes the new state the saved copy. If it fails, `self.saved` stays as it was, so on the next tick line 278 finds the same difference and the write is tried again. The flag `save_failed` (line 298) only keeps the log short.

NVS is a small key-value store that ESP-IDF, the manufacturer's framework, keeps in the board's flash memory. The firmware stores the `State` there as a JSON string under one key, `alarms`. `AlarmClock::new` reads that key when the clock starts and gives the result to `Alarms::new`. So a snoozed alarm survives a reboot.

The `match` on line 282 is there because `nvs` is an `Option`: if the store could not be opened at start-up, there is nothing to write.

The second half is about sound, and it has the same shape: compare, and act only on a difference.

{% <code file="firmware/src/app/alarm.rs" lines="302-328" hl="303-306 317-318 324"> %}{% raw %}
        let ringing = self.machine.ringing();
        if ringing == self.sounding {
            return;
        }
        self.sounding = ringing;
        // An alarm takes over from a preview.
        self.previewing = false;
        self.cancel_stream();
        match ringing.and_then(|id| self.get(id)).cloned() {
            Some(cfg) => {
                // ... @316
                self.output.set_streaming(true);
                self.player
                    .ring(sound, tone::volume_gain(cfg.volume), cfg.fade_in_s);
            // ... @322
            }
            None => {
                self.player.stop();
                self.output.set_streaming(false);
            }
        }
    }
{% endraw %}{% </code> %}

Line 302 asks the machine which alarm is ringing right now. The answer is an `Option<u32>`: the alarm's id, or `None`. The field `sounding` holds the answer from the last change, so it says what the speaker is doing now. If the two are equal, line 304 returns.

Otherwise line 306 records the new answer, and the next lines end a preview from the sound page, if one is playing. The `match` on line 310 then looks up the ringing alarm's configuration. If there is one, the lines left out pick its sound, a file from the card or the built-in tone, and lines 316 to 318 tell the output to stream and the player to ring. If no alarm is ringing, lines 324 and 325 stop both. [Chapter 17](@/wakey/17-sound.md) follows these calls into the player.

{% <tip title="Remember what you did last, and compare" id="state-follow-compare"> %}
`follow` does not ask what brought it here. It looks at where the machine is now and compares that with two remembered values, `saved` and `sounding`.

So the function can be called as often as the caller likes. `tick` calls it every second, and on almost every call both comparisons find no difference. Then there is no write to flash and no command to the player.
{% </tip> %}

## Changing the list: the file first

When a person saves, deletes or switches an alarm, the code builds a new list and hands it to `set_alarms`. The order of its lines is the point.

{% <code file="firmware/src/app/alarm.rs" lines="102-109" hl="104-106"> %}{% raw %}
    pub fn set_alarms(&mut self, mut alarms: Vec<AlarmConfig>) -> std::io::Result<()> {
        alarm::sanitize_list(&mut alarms);
        config::save_alarms(&alarms)?;
        self.machine.reconfigured(&alarms, clock::now());
        self.alarms = alarms;
        self.follow();
        Ok(())
    }
{% endraw %}{% </code> %}

Line 103 brings out-of-range values into range. Line 104 writes the list to `config.json` on the memory card. Only then does line 105 tell the machine about the new list, and line 106 replace the list in memory. Line 107 calls `follow`, because `reconfigured` may have changed the state. If the ringing alarm was deleted, the machine goes back to idle, and `follow` saves that and stops the sound.

{% <pitfall title="Write the file before you change memory" id="state-file-before-memory"> %}
The `?` on line 104 returns from the function when the write fails. At that point nothing in `self` has been touched, so the clock and the card still agree.

With the opposite order, a failed write would leave a list in memory that the card does not have. The alarm would ring as edited until the next reboot, and then come back as it was before.
{% </pitfall> %}

## What the screens may see

The screens show the alarm state, but they do not get the machine. `AlarmClock::view` gives them a small enum, `View`. Its variant `Idle` carries the time of the next alarm, `Ringing` the number of snoozes left, and `Snoozed` that number and the time the alarm rings again. A `Phase` holds more, such as the alarm's id and a Unix time. The screens have no use for those, and with `View` they cannot come to depend on them.

## The draft: editing a copy

The alarm page does not change an alarm in the list while its wheels turn. It works on a copy, and `Ctx` has two fields for that. `draft` is a `RefCell<AlarmConfig>`, the alarm being edited. `original` is a `RefCell<Option<AlarmConfig>>`, the alarm as it was when the page opened.

`alarm_page::open` fills both from the `AlarmConfig` it receives. It stores a clone in `original`, or `None` for a new alarm, which has the id 0, and moves the value itself into `draft`. From then on the alarm page, the sound page and the name page all write into `draft`.

{% <diagram caption="The life of a draft. Only the Save path reaches `AlarmClock` and the card."> %}
stateDiagram-v2
    [*] --> Opened: open copies the alarm
    Opened --> Edited: wheels, days, name, sound
    Edited --> Saved: Save
    Opened --> Saved: Save, for a new alarm
    Opened --> Dropped: Cancel
    Edited --> Dropped: Cancel
    Saved --> [*]
    Dropped --> [*]
{% </diagram> %}

The function `changed` tells the page whether there is anything to save. Look at how it compares the two copies.

{% <code file="firmware/src/app/alarm_page.rs" lines="59-70" hl="63-66"> %}{% raw %}
pub fn changed(ctx: &Ctx) -> bool {
    let draft = ctx.draft.borrow();
    match &*ctx.original.borrow() {
        Some(saved) => {
            AlarmConfig {
                enabled: saved.enabled,
                ..draft.clone()
            } != *saved
        }
        None => true,
    }
}
{% endraw %}{% </code> %}

A new alarm has no original, so line 68 answers `true`. For an existing alarm, a plain `*draft != *saved` would almost do. But the comment above the function says that switching the alarm on or off must not count as a change, because the list's switch does that. So lines 63 to 66 build a third value with struct update syntax: `enabled` comes from the saved alarm and every other field from a clone of the draft. Whatever differs now is an edit.

`alarm_page::save` uses the same syntax once more. It builds `AlarmConfig { enabled: true, ..draft.clone() }`, because saving an alarm means it should ring, and hands that to `AlarmClock::save`. That method puts it into a clone of the list, gives a new alarm a fresh id, and calls `set_alarms`. On an error the page stays open with an error text, and the draft is still there. Cancel only shows the list page again. The draft stays as it is until the next `open` overwrites it.

## Two stores

`config.json` is a text file in the root of the memory card. It holds the settings: language, time zone, Wi-Fi, the alarms. A person can edit this file on a computer, and the firmware expects that. NVS holds what the machine has done, the phase and the finished days. Nobody edits that by hand, and it changes without a touch, for example when an alarm starts to ring.

The firmware itself writes two parts of `config.json`: the alarm list, and the login for the music service TIDAL. Both go through one function in `firmware/src/config.rs`. (At start-up, `config::load` also adds settings that the file lacks.)

{% <code file="firmware/src/config.rs" lines="128-137" hl="129 136"> %}{% raw %}
fn rewrite(edit: impl FnOnce(Option<&str>) -> String) -> io::Result<()> {
    let _writer = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = sd::path(FILE);
    let old = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    sd::replace(&path, edit(old.as_deref()).as_bytes())
}
{% endraw %}{% </code> %}

`rewrite` reads the whole file, lets the closure `edit` produce the new text, and writes that. For the alarms, `edit` replaces the `alarms` list and keeps every other field.

Line 129 takes a lock first. `WRITE` is a `static Mutex<()>`, a mutex that protects no data, only this stretch of code. There are two writers: the UI thread saves alarms, and the `wifi` thread saves the TIDAL login. Without the lock both could read the same old text, and the second write would undo the first. The guard is bound to a name, `_writer`, so it lives until the function ends.

Line 136 does not write to `config.json` directly. `sd::replace` in `board/sd.rs` first writes the new text to a file beside it, `config.tmp`. If that fails, the old file is untouched.

{% <pitfall title="FAT cannot rename onto an existing file" id="state-fat-rename"> %}
Renaming the temporary file over the old one would finish the job in one step. The comment on `sd::replace` says that the card's file system, FAT, cannot do that. So the function removes the old file and then renames the temporary one.

That leaves a short moment with no `config.json` on the card. If the power fails exactly then, the new text exists only as `config.tmp`. Without help, the next start would find no `config.json`, take it for a first set-up and run on the defaults, and the next save would write those over everything.

So when `config.json` is missing, `config::load` looks for `config.tmp`, and if it parses, renames it into place and uses it. [Chapter 20](@/wakey/20-errors.md) shows that arm of `load`.
{% </pitfall> %}

## A broken `config.json`

A file that people edit by hand can be broken. For `config::load`, a missing file with no `config.tmp` to recover is a first setup, and the clock starts with the defaults. A file that exists but cannot be read or parsed makes `load` return `Err(String)`, the error in words. The comment gives the reason: the clock must not run on defaults and overwrite the file later. So, as [chapter 4](@/wakey/04-startup.md) showed, start-up calls `config_error`, which shows the text on a page of its own and goes no further: no alarms, no Wi-Fi, nothing written. [Chapter 20](@/wakey/20-errors.md) places this among the firmware's error policies.

For our two questions, this chapter is about the second, where the state lives. The alarm state is in the machine, inside one `AlarmClock` on the UI thread, and the draft is beside it in `Ctx`. What must outlast the power is on the card or in flash. The file is written only through `sd::replace`, and the alarm state only in `follow`. The [next chapter](@/wakey/16-one-touch.md) follows a single touch through all of this.
