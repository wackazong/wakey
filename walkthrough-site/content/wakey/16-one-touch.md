+++
title = "One touch, end to end"
description = "A tap on an alarm's switch, followed from the touch controller to the pixels and the memory card."
weight = 16

[extra]
map = ["ui", "app", "config", "core", "display", "board"]
map_note = "The tap enters through `board` and `ui`, changes state in `app` and `core`, is saved by `config`, and leaves through `ui` and `display`."
goals = [
  "the complete path of one tap, hop by hop, and the thread that does each hop",
  "how the alarm list decides that a tap was meant for the switch, and when it acts",
  "which mechanism carries the tap across each border, and where the state is at each moment",
]
takeaways = [
  "Only the first hop runs on the `touch` thread. From the moment the event leaves `EVENTS`, everything runs on the `app` thread.",
  "The list layer lets a tap pass to Slint, and the page's own touch area fires `toggle` when the finger lifts. The switch in the row only draws.",
  "`set_alarms` writes `config.json` inside the callback, so the render loop stands still until the card has answered. A failed write leaves the list in memory as it was, and the page says “save failed” in its header.",
  "One changed row of the model is all that Slint hears. The switch then slides over several frames, and each frame goes through the PPA and a flip.",
]
+++

Every part has now been read on its own. In this chapter we watch the parts move together, by following one event through the whole firmware.

The alarm list is on screen and still, no alarm is ringing, and the user taps the on/off switch of one alarm. We follow that tap from the finger to the pixels and to the memory card, in the order in which things happen, and we name the thread at every hop.

{% <diagram caption="The whole journey. Everything to the right of `EVENTS` runs on the `app` thread." wide={true}> %}
sequenceDiagram
    participant F as touch controller
    participant T as thread touch
    participant Q as EVENTS
    participant L as render loop
    participant H as list layer
    participant S as Slint
    participant V as view model
    participant A as AlarmClock
    participant C as memory card
    participant D as display
    Note over F: the finger comes down
    T->>F: I2C read, every 10 ms
    T->>Q: queue(Down)
    Q-->>L: the select ends
    L->>H: pointer(Down)
    H-->>L: Route Slint
    L->>S: dispatch, pointer pressed
    Note over F: the finger lifts
    T->>F: three empty reads
    T->>Q: queue(Up)
    Q-->>L: the select ends
    L->>H: pointer(Up)
    H-->>L: Route Slint
    L->>S: dispatch, pointer released
    S->>V: toggle(id, on)
    V->>A: set_enabled
    A->>C: save_alarms rewrites config.json
    Note over A: reconfigured, then follow
    V->>S: set_row_data for one row
    L->>D: back_buffer, waits for the last flip
    L->>S: render what changed
    S->>D: strips through the PPA
    L->>D: present
    Note over L,D: again while the switch slides
{% </diagram> %}

## The finger comes down: the `touch` thread

The journey starts on the `touch` thread. Every 10 ms its function `poll` in `ui/input.rs` locks the I2C bus, the two-wire bus that the board's small chips share, and calls `touch::read_point`. The lock is held for that one statement, as [chapter 7](@/wakey/07-sharing-hardware.md#temporary-guard) showed. No interrupt announces the finger. The module's comment says that the interrupt pin is not used, so the thread asks.

This time the answer is a point. Watch what the thread remembers in line 177.

{% <code file="firmware/src/ui/input.rs" lines="167-178" hl="168 170 177"> %}{% raw %}
                let (x, y) = to_landscape(px, py);
                let phase = if down { Phase::Move } else { Phase::Down };
                if phase == Phase::Down || (x, y) != last {
                    queue(TouchEvent {
                        phase,
                        x,
                        y,
                        at: Instant::now(),
                    });
                }
                down = true;
                last = (x, y);
{% endraw %}{% </code> %}

Line 168 decides what kind of event this is. `down` is a local variable of `poll`, and it is still `false`, so the phase is `Down`. Line 170 hands the event to `queue`, which puts it into `EVENTS`, the static channel from [chapter 8](@/wakey/08-channels.md). There is room, so nothing waits. Line 177 then sets `down`. That `bool` is the first place where our touch is state.

## Across the thread border

The `app` thread is asleep at this moment. Its render loop waits in the `select` at the end of `render_loop::run`, for a timer or for `EVENTS.receive()` ([chapter 12](@/wakey/12-touch-in.md)). The `try_send` on the `touch` thread makes `receive` ready, the executor wakes its thread, and the loop stores the event in `woken_by`.

This is the only thread border on the journey. From here to the last pixel, every hop is a function call on the `app` thread.

On its next pass, the loop offers the event to its hook first. The hook is the list layer, the code that scrolls the long lists by itself. Its `pointer` method is one `match` on the event's phase and on `self.touch`, the layer's own note of who has the current touch. These are the two arms our tap reaches.

{% <code file="firmware/src/ui/list_layer/mod.rs" lines="628-695" hl="629-630 692-694"> %}{% raw %}
            (Phase::Down, _) => {
                self.touch = Touch::Pending(ev.x, ev.y);
                Route::Slint
            }
            // ... @692
            (Phase::Up, _) => {
                self.touch = Touch::None;
                Route::Slint
            }
{% endraw %}{% </code> %}

For the `Down`, the arms above line 628 do not apply: the page is shown, the finger is inside the list, no row is being swiped and the list is not moving. So line 629 notes where the finger came down, as `Touch::Pending`, and line 630 answers `Route::Slint`. A later `Move` more than 12 pixels away from that point can make the layer take the touch over, as a scroll or a swipe. Our finger does not move that far.

{% <tip title="Match arms are tried in order, so the general one goes last" id="one-touch-arm-order"> %}
`pointer` matches on a tuple, like the transition table in [chapter 14](@/wakey/14-state-machine.md#alarm-tuple-match). `(Phase::Down, _)` appears four times, three of them with an `if` guard.

Rust takes the first arm that matches, from top to bottom. So the special cases come first, and line 628 is what remains. Line 692 does the same for a release.
{% </tip> %}

Back in the loop, `dispatch` turns the event into Slint's `PointerPressed` and hands it to the window. Slint is the UI toolkit, and the page is described in `firmware/ui/wakey/alarm_list.slint`. There, a `TouchArea` named `ta` lies over the list. On the press it mostly takes notes: which row is under the finger (`touched`), and that the gesture is still `undecided`. It also starts a 40 ms timer for the row's highlight, but that timer lights nothing for a press on the switch side.

## The finger lifts: Slint decides

When the finger leaves, the controller reports no point. As chapter 12 explained, the `touch` thread waits for the third empty reading in a row. Then `release` sends `Up` and sets `down` back to `false`.

The loop wakes as before. This time the list layer reaches line 692, forgets the touch and answers `Route::Slint`. `dispatch` sends `PointerReleased`, and now the page acts. This is the part of `ta`'s handler for the release.

{% <code file="firmware/ui/wakey/alarm_list.slint" lines="310-323" hl="312-313 319" lang="slint"> %}{% raw %}
                } else if (e.kind == PointerEventKind.up) {
                    let i = view.touched;
                    let toggles = view.gesture == Gesture.undecided && i >= 0
                        && self.mouse-x > self.width - view.switch-area;
                    // ... @317
                    if (view.gesture == Gesture.undecided && i >= 0) {
                        if (toggles) {
                            AlarmList.toggle(AlarmList.rows[i].id, !AlarmList.rows[i].enabled);
                        } else {
                            AlarmList.open(AlarmList.rows[i].id);
                        }
                    }
{% endraw %}{% </code> %}

Lines 312 and 313 are the decision. The tap toggles if the gesture is still undecided, if a row was under the finger when it came down, and if the finger is within `switch-area`, 220 pixels, of the list's right edge. Then line 319 calls the callback `toggle` of the global `AlarmList` with the row's id and the opposite of its `enabled`.

So the callback fires on the release, and the page fires it. The switch itself does nothing with the touch.

{% <code file="firmware/ui/wakey/alarm_list.slint" lines="169-175" hl="172-173" lang="slint"> %}{% raw %}
        Switch {
            x: parent.width - self.width - 40px;
            y: (parent.height - self.height) / 2;
            on: root.alarm.enabled;
            touchable: false;
            animated: root.animated && root.switch-animated;
        }
{% endraw %}{% </code> %}

This is the switch inside a row. Line 173 turns its own touch handling off. Line 172 binds what it shows to the row's data, so it moves only when Rust changes that data.

## The handler changes the state

Slint now calls the closure that `Ctx::with_args` built, and the closure runs the handler in `app/alarm_list.rs`, which [chapter 11](@/wakey/11-view-models.md#ctx-refcell-two-statements) showed. Its first statement borrows the `AlarmClock` and calls this method.

{% <code file="firmware/src/app/alarm.rs" lines="131-137" hl="132 136"> %}{% raw %}
    pub fn set_enabled(&mut self, id: u32, enabled: bool) -> std::io::Result<()> {
        let mut list = self.alarms.clone();
        if let Some(a) = list.iter_mut().find(|a| a.id == id) {
            a.enabled = enabled;
        }
        self.set_alarms(list)
    }
{% endraw %}{% </code> %}

Line 132 clones the list of alarms. The `if let` finds our alarm in the clone and changes its `enabled`. Line 136 hands the clone to `set_alarms`. The `AlarmClock` itself is still unchanged.

{% <tip title="Change a copy, then commit it in one place" id="one-touch-copy-commit"> %}
`set_enabled`, `save` and `delete` have the same shape. Each builds a new list and ends in `set_alarms`.

So one function validates, saves and applies every change. And a failed write cannot leave a half-changed list in memory.
{% </tip> %}

`set_alarms` is the function from [chapter 15](@/wakey/15-state-on-device.md#state-file-before-memory), and it writes the file first. `config::save_alarms` takes the static lock `WRITE`, reads `config.json` from the memory card, replaces the alarm list in the text and writes the file back through `config.tmp`. Only then does `reconfigured` tell the machine from `wakey-core` about the list, and the new list replaces the old one in memory. Last comes `follow`. With no alarm ringing it usually finds no difference, and then it writes nothing to NVS, the store in flash, and sends nothing to the player.

{% <pitfall title="A callback runs inside the loop that called it" id="one-touch-callback-blocks"> %}
The render loop called `dispatch`, Slint called the closure, and the closure called down to `std::fs`. All of it is one call stack on the `app` thread, and the file calls block.

Until the card has answered, the loop draws nothing and fires no timer. The `touch` thread keeps running, and its events wait in the 32 places of `EVENTS`; after about a third of a second, moves would be dropped and a press or release would wait ([chapter 8](@/wakey/08-channels.md#full-queue-drops)). The write is small, but it is a rough edge of the journey.
{% </pitfall> %}

If the write fails, the list in memory is as before, so the switch stays where it was, and `show_result` puts "save failed" in the page's header, as the alarm page does.

## Back to the screen

The handler's last statement is `refresh`. It builds the rows again from the alarms and compares them with the model, the list of rows that Slint draws from. Our row differs, so `refresh` calls `model.set_row_data` for it. Slint marks what depends on a changed row for repainting. Then the handler returns, and the render loop continues with its pass.

In `theme.slint` the switch's knob has an `animate` of 200 ms on its position. So the change is not one frame. While `window.has_active_animations()` is true, the loop does not sleep. Each pass is the output half from [chapter 13](@/wakey/13-pixels-out.md). It waits in `display.back_buffer().await` until the panel's interrupt has reported the last flip. Slint renders what changed in strips, and the PPA, the chip's block for rotating pictures, turns each strip into the back buffer. Then `present` flips.

When the animation has ended, the flag `other_buffer_stale` can ask for one more render, so that both frame buffers hold the final picture. Then the list layer gets its idle turn. For scrolling it keeps pictures of the rows in PSRAM, the slow external memory. Our row's stamp has changed, so the layer renders that picture again. After that the `app` thread sleeps in its `select`.

## The two questions, answered once

*Which parts worked together, and how?* The `touch` thread polled the controller over I2C, under a mutex. It reached the `app` thread through a channel. The render loop asked the list layer through a trait and got an enum back. Slint reached Rust through a callback, and Rust reached Slint through a model. `AlarmClock`, the machine and `config` are plain function calls, with a static mutex around the file. The panel answered with an interrupt that ends in an atomic counter and a notification.

*Where was the state?* First in `down` on the `touch` thread, then in the queue, then in the list layer's `touch` field and in the page's `touched` and `gesture`. For the length of one function it was a cloned list. Then it was in `config.json`, then in `AlarmClock`, then in one row of the model. In the end it was in two frame buffers and in the list layer's picture of the row. Of all these, only the queue and the file are reached by more than one thread.

The [next chapter](@/wakey/17-sound.md) follows a different output, from `follow` to the speaker.
