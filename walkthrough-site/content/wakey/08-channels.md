+++
title = "How threads talk: channels and commands"
description = "Messages from one thread to another: the touch queue, command channels, and closures passed downwards."
weight = 8

[extra]
map = ["ui", "audio", "net"]
map_note = "Three services receive messages: `ui` has the touch queue, `audio` the two speakers, and `net` the Wi-Fi thread with its jobs."
goals = [
  "how touch events get from the `touch` thread into the one `async` task",
  "what happens to a message when its queue is full, and how the code chooses between dropping and waiting",
  "how a handle with a few methods hides a command channel",
  "why a boxed closure can be a message, and why some closures are handed over only once",
]
takeaways = [
  "`input::EVENTS` is the only channel that leads into the `async` task. The `touch` thread fills it with `try_send`, which never waits. The task takes the events out with `try_receive` and, when it is idle, waits on `receive()`.",
  "A queue with a limit forces a decision when it is full. The firmware drops touch events and wheel clicks, and it waits for everything that must arrive.",
  "`UsbAudio`, `BuiltinSpeaker` and `Wifi` are handles. Each keeps its `Sender` private and offers methods instead. The two speakers keep their message type private as well.",
  "A `Job` is a boxed closure sent through a channel, so the `wifi` thread runs code it knows nothing about. `Source` and `on_time_synced` are closures that are handed over once and then called many times.",
]
+++

The earlier chapters showed up to thirteen threads and gave each one its own hardware. This chapter and the next are a pair about how those threads communicate. There are two ways to do it, and the firmware uses both.

The first way is to *send* something. A value leaves one thread and arrives in another, and afterwards only the receiver has it. That is message passing, and it is this chapter. Its main tool is the channel, a queue with two ends: one thread puts values in, and another takes them out in the same order. The second way is to *share* something. A value stays in one place and several threads look at it. That is the subject of [chapter 9](@/wakey/09-flags-and-locks.md).

The picture shows every message path in the firmware.

{{ <svg src="img/messages.svg" caption="Every message path between threads. Solid arrows are channels, with the type that travels on them and the room they have. Dashed arrows are closures that the `app` thread hands over at start-up." /> }}

## The one channel into the task

We begin with touch. The `touch` thread asks the touch controller for a finger position every 10 ms, and the user interface runs on the `app` thread inside the one `async` task. Something has to carry the events across. In the next excerpt, look at the declaration on line 69 and at the two highlighted lines of `release`, which reports a lifted finger.

{% <code file="firmware/src/ui/input.rs" lines="68-91" hl="69 83 89"> %}{% raw %}
/// The touch events, oldest first; the render loop takes them out.
pub static EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, 32> = Channel::new();
// ... @81
fn release(down: &mut bool, last: (f32, f32)) {
    if *down {
        let _ = EVENTS.try_send(TouchEvent {
            phase: Phase::Up,
            x: last.0,
            y: last.1,
            at: Instant::now(),
        });
        *down = false;
    }
}
{% endraw %}{% </code> %}

Line 69 is the whole channel. It comes from `embassy_sync`, one of the Embassy crates, and it has three parameters. The first names the lock that guards the queue inside, and this one may be used from any thread. The second is the type of the messages. The third is the room: 32 events. The channel is a `static`, so both sides reach it by its name and nobody has to pass a handle around.

Line 83 is the sending side. `try_send` puts the event in the queue if there is room and returns at once in either case. The thread never waits here, so nothing delays its next poll 10 ms later.

The receiving side is in the render loop, the loop on the `app` thread that draws the user interface. When it has nothing to draw, it waits on `EVENTS.receive()` and on a timer together, and whichever is ready first wakes the task. The Embassy channel makes this possible, because its `receive` is a future. [Chapter 12](@/wakey/12-touch-in.md) reads that loop.

{% <pitfall title="A full queue drops the event, even the last one" id="full-queue-drops"> %}
When all 32 places are taken, `try_send` returns an error. The `let _ =` on line 83 throws that result away, so the event is lost silently.

For a `Move` event this costs little, because the next one follows 10 ms later. For the `Up` event it is a rough edge. Line 89 sets `down` to `false` whether or not the event went out, so the thread never sends it again. The user interface would keep a pressed finger until the next touch.

Fixed after this commit: only `Move` events are dropped now, and a `Down` or `Up` waits for room.

The queue fills only if the render loop takes nothing out for at least 32 polls, about a third of a second, while the finger moves the whole time.
{% </pitfall> %}

So touch events travel through a small queue that never makes the sender wait. Commands to the other threads use a different kind of channel.

## Commands behind a handle

The remaining channels come from the standard library, `std::sync::mpsc`. Their receiving end is made for an ordinary thread, which blocks until a message comes. We look at the USB speaker first. In the excerpt, watch which items are `pub` and which are not.

{% <code file="firmware/src/audio/uac/mod.rs" lines="44-99" hl="68 98"> %}{% raw %}
enum Cmd {
    Stream(bool),
    // ... @48
    Power(bool),
}
// ... @64
/// The USB speaker. Cheap to clone.
#[derive(Clone)]
pub struct UsbAudio {
    shared: Arc<Shared>,
    cmds: mpsc::Sender<Cmd>,
}
    // ... @96
    pub fn set_streaming(&self, on: bool) {
        self.shared.streaming.store(on, Ordering::Release);
        let _ = self.cmds.send(Cmd::Stream(on));
    }
{% endraw %}{% </code> %}

`Cmd` at the top lists everything one can ask of the `uac` thread, the thread that talks to the USB speaker: start or stop the sound stream, and switch the port's power. The enum is private. So is the field `cmds` on line 68, the sending end of the channel. The only public things are the struct `UsbAudio` and its methods.

Line 98 shows what such a method does. It builds a `Cmd` and sends it. This channel was made with `mpsc::channel()`, which is unbounded, so `send` never waits. It fails in one case only, when the receiving thread no longer exists, and the `let _ =` ignores that case. Line 97 belongs to the next chapter.

{% <tip title="A handle with methods hides the channel" id="handle-hides-channel"> %}
The rest of the firmware calls `speaker.set_streaming(true)`. It cannot see that a channel exists, and it cannot build a `Cmd`, because both are private to the module. This keeps the protocol between the two threads in one module.

`mpsc::Sender` implements `Clone`, and every clone sends into the same queue. That is why `#[derive(Clone)]` is enough to make the handle cheap to pass around.
{% </tip> %}

The built-in speaker follows the same shape with one difference. Its channel is created with `mpsc::sync_channel::<Msg>(8)`, so it is bounded at eight messages. A bounded channel can be full, and the code has to say what happens then.

{% <code file="firmware/src/audio/builtin.rs" lines="140-151" hl="143 149"> %}{% raw %}
    pub fn set_streaming(&self, on: bool) {
        if let Some(tx) = &self.tx {
            // Must not be dropped, unlike a click.
            let _ = tx.send(Msg::Stream(on));
        }
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(msg);
        }
    }
{% endraw %}{% </code> %}

The two highlighted lines give two different answers. The private helper `send` at the bottom is the path for wheel clicks, the short tick the speaker plays while the user turns a time wheel. It calls `try_send` on line 149, and if eight messages are already waiting, the click is dropped. Line 143 sends the message that starts or stops the alarm's sound on this speaker. It uses `send`, which on a bounded channel waits until there is room. The comment on line 142 gives the reason: this message must arrive.

## A closure as a message

The `wifi` thread receives the most unusual messages. They are not values from an enum. They are pieces of code. Look at the type on line 101 first, and then at what `run` does when the send fails.

{% <code file="firmware/src/net.rs" lines="99-170" hl="101 167-169"> %}{% raw %}
/// Work for a session: called on the `wifi` thread with `Ok` while
/// connected, or with the reason there is no connection.
pub type Job = Box<dyn FnOnce(Result<(), Unavailable>) + Send>;
    // ... @166
    pub fn run(&self, job: Job) {
        if let Err(mpsc::SendError(job)) = self.jobs.send(job) {
            job(Err(Unavailable::Failed));
        }
    }
{% endraw %}{% </code> %}

A *session* is one period in which the `wifi` thread switches the radio on and connects. A `Job` is work for such a session. The part of the firmware that needs the network, the TIDAL music service, writes a closure and gives it to `Wifi::run`. The `wifi` thread takes it out of the channel and connects. Then it calls the job with `Ok`, or with the reason why there is no connection. The thread does not know what the job does.

Lines 167 to 169 handle the one way the send can fail, which is that the `wifi` thread has ended. `SendError` hands the unsent value back, so the pattern on line 167 takes the job out again and line 168 calls it right there with an error. The job hears back in every case. [Chapter 18](@/wakey/18-radio.md) shows how the thread keeps that promise on its own side.

{% <tip title="What each part of the `Job` type is for" id="boxed-closure-message"> %}
Every closure has its own anonymous type, and a channel carries one type only. `Box<dyn …>` turns all of them into that one type.

`FnOnce` says that the job is called a single time, which lets it move its captured values out when it runs. `Send` is what allows the box to cross to another thread. Without it, the compiler would refuse to move the receiving end of the channel into the `wifi` thread.
{% </tip> %}

Closures also travel in a second way, without any channel. The dashed arrows in the picture are closures that the `app` thread creates at start-up and gives to a service for good.

One is the `Source`, a `Box<dyn FnMut(&mut [i16]) + Send>` that fills a buffer with sound samples. It is `FnMut`, not `FnOnce`, because it is called again and again. Each speaker thread owns one and calls it whenever it needs the next block of sound. Here the direction is turned round: the receiver decides when data moves. [Chapter 17](@/wakey/17-sound.md) follows the samples. The other closure is `on_time_synced`, which [chapter 4](@/wakey/04-startup.md) saw being built. The `wifi` thread calls it after each clock synchronisation, and it writes the time into the real-time clock chip, which `net` itself never sees.

## Waiting on purpose: the card writer

The last pair of channels shows the opposite of dropping. During a download, the `wifi` thread receives data and a `card-writer` thread writes it to the memory card. If the card is slower than the network, the download has to slow down. The mechanism is 16 blocks of memory, 32 KiB each, that go round between the two threads. This is the function that the downloading side calls when a block is full.

{% <code file="firmware/src/tidal/card_writer.rs" lines="127-138" hl="129 131"> %}{% raw %}
    /// Hands the current block to the writer and takes a free one.
    fn pass(&mut self) -> std::io::Result<()> {
        self.send(Some((self.cur, self.len)))?;
        self.len = 0;
        match self.free.recv() {
            Ok(b) => {
                self.cur = b;
                Ok(())
            }
            Err(_) => Err(self.stopped()),
        }
    }
{% endraw %}{% </code> %}

Line 129 sends the full block to the writer through the first channel. The message is the block's address and its fill level, not the bytes, so nothing is copied. Line 131 then takes an empty block from the second channel, `free`, into which the writer puts every block it has written.

That `recv` is where the waiting happens. While the card keeps up, a free block is always there. When the card falls behind, all the other blocks are waiting for the writer, `free` is empty, and `recv` blocks until one comes back. Slowing the sender down like this is called back-pressure.

For our two questions, this chapter says that the parts work together along a few narrow, named paths, each with one kind of message. It also adds a rule about state: what is sent is not shared. An event, a job or a block has one owner before the send and another one after it. The [next chapter](@/wakey/09-flags-and-locks.md) looks at the state that stays in one place while several threads read it.
