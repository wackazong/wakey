+++
title = "Sound: a decoder, a ring and two speakers"
description = "The path from a ringing alarm to a speaker: a decoder thread, a ring of samples, a closure that hands them out, and a router."
weight = 17

[extra]
map = ["audio", "core"]
map_note = "Everything that makes sound is in `audio`. Reading sound files and converting sample rates is pure logic in `wakey-core`."
goals = [
  "how a ringing alarm starts a `decoder` thread, and how that thread ends",
  "how a `move` closure with a counter of its own becomes the `Source` that a speaker calls",
  "why a `Box<dyn …>` needs one trait that combines `Read` and `Seek`, and how a blanket impl provides it",
  "how `Output` chooses between the two speakers with a `match` on a pair of values",
]
takeaways = [
  "Sound is pulled, not pushed. A speaker's thread calls the source when its hardware needs the next block, and the decoder only keeps the ring full.",
  "`Player::source` returns a `move` closure that owns a clone of the `Arc<Ring>` and a counter, `frames`.",
  "A trait object can name one ordinary trait only. `ReadSeek` combines `Read` and `Seek`, and its blanket impl gives it to every type that has both.",
  "`Output` keeps what is wanted and the current route. Once a second it works out the route again, and it switches one speaker off before it switches the other on.",
]
+++

[Chapter 15](@/wakey/15-state-on-device.md) ended at `AlarmClock::follow`, the one place where a change in the alarm's state becomes sound. This chapter follows that sound from there to a speaker.

You have met the pieces already. [Chapter 8](@/wakey/08-channels.md) showed the command channels of the two speakers and the `Source` type. [Chapter 9](@/wakey/09-flags-and-locks.md) showed the ring's lock and its condition variable. Here we put them together as one path.

{% <diagram caption="The audio path. Solid arrows carry samples. Dashed arrows are commands."> %}
flowchart TD
    F[file on the card] --> D[decoder thread]
    D -->|push| R[ring]
    R --> S1[source] --> U[uac thread] --> P[USB speaker]
    R --> S2[source] --> M[speaker thread] --> B[built-in speaker]
    O[Output] -.->|stream on or off| U
    O -.->|stream on or off| M
{% </diagram> %}

A *sample* is one `i16` number, and a *frame* is two samples, one for the left channel and one for the right. Everything plays at 48,000 frames per second.

## The alarm starts to ring

We begin on the `app` thread, near the end of `follow`. An alarm has started to ring. `cfg` is its configuration, and `sound` is either `Sound::File` with the path of a file on the memory card, or `Sound::Tone`, a beep the firmware computes itself. These three lines start the sound.

{% <code file="firmware/src/app/alarm.rs" lines="294-296"> %}{% raw %}
                self.output.set_streaming(true);
                self.player
                    .ring(sound, tone::volume_gain(cfg.volume), cfg.fade_in_s);
{% endraw %}{% </code> %}

Line 294 tells `Output` that sound is wanted. Lines 295 and 296 give the sound to the `Player`, with two numbers. The first is the *gain*, the factor by which every sample is multiplied, and `tone::volume_gain` computes it from the alarm's volume in percent. The second is the length of the fade-in in seconds, the time over which the sound grows from silent to that gain.

Neither call makes a sample here. We follow `Player::ring` first and come back to `Output` at the end.

## One thread for each sound

`Player::ring` wraps the sound in a closure and passes it to the private method `start` as the parameter `produce`. The closure calls `play_sound`. If the file cannot be played, it logs the error and plays the tone instead.

`start` first makes room. If an earlier sound is still playing, it calls `stop` and waits, in sleeps of 10 ms, until that sound's thread has cleared the flag `playing`, its last step. Then it empties the ring, stores the gain and the fade length in the ring's atomics, resets its flags and spawns the new thread.

{% <code file="firmware/src/audio/player.rs" lines="185-198" hl="185 189 197"> %}{% raw %}
        Thread::new(c"decoder", 64 * 1024)
            .core(Core::Core1)
            .stack_in_psram()
            .spawn(move || {
                produce(&ring);
                // Let the ring drain before reporting the end; the tail
                // running dry is not an underrun.
                ring.armed.store(false, Ordering::Release);
                while !ring.samples.lock().unwrap().is_empty() && !ring.stop.load(Ordering::Acquire)
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                ring.playing.store(false, Ordering::Release);
            })
{% endraw %}{% </code> %}

Line 185 uses the `Thread` builder from [chapter 5](@/wakey/05-threads.md), which also gave the reasons for core 1 and the large stack. Line 189 runs `produce`, and for a file this is a long loop in `decode_from`. Each round reads up to 4,096 frames from the file, converts them if the file has another sample rate, and calls `push`. Chapter 9 showed that `push` sleeps while the ring is full. The ring holds one second of sound, so the decoder is never further ahead than that. At the end of the file the loop starts the file again. It ends when the `stop` flag is set, or with an error. One case is different: for a file whose number of frames is not known, the function returns at the end of the file. An alarm with such a sound rang once and then went on in silence; after this commit, it starts over too.

After `produce` returns, the loop from line 193 waits until the ring is empty or `stop` is set. Line 197 then clears the flag `playing`, and the thread ends. The next call of `start` waits for that flag. So a `decoder` thread lives as long as one sound.

## What can be played

`play_sound` takes a `Sound`. The skipped lines hold the variants `File` and `Tone`. The third variant needs a trait that exists only for this purpose.

{% <code file="firmware/src/audio/player.rs" lines="48-59" hl="54 58-59"> %}{% raw %}
pub enum Sound {
    // ... @54
    Stream(String, Box<dyn ReadSeek + Send>),
}

/// A source for [`Sound::Stream`].
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}
{% endraw %}{% </code> %}

`Stream` belongs to the preview on the sound page, where the user can listen to a track from the TIDAL music service while it is still downloading. Its second field, on line 54, is a reader over that growing file. The player needs to read from it and to jump to a position in it, which are the standard traits `Read` and `Seek`.

{% <tip title="A trait that combines two traits, for `dyn`" id="sound-readseek-blanket"> %}
`Box<dyn Read + Seek + Send>` does not compile. A trait object may name one ordinary trait, and beside it only auto traits such as `Send` and `Sync`.

Line 58 is the way out: a new trait, `ReadSeek`, that has no methods of its own and requires both. `dyn ReadSeek` is then one trait.

An empty trait still has to be implemented for every reader. Line 59 does that once for all of them. It is a blanket impl: every type `T` that has `Read` and `Seek` gets `ReadSeek` too.
{% </tip> %}

## The source: where samples leave the ring

The ring is emptied by the `Source`, the boxed closure from chapter 8 that fills a buffer with samples. `Player::source` builds it. Look at the two variables above the closure and at what the closure does with them.

{% <code file="firmware/src/audio/player.rs" lines="92-119" hl="94-95 97 99 111"> %}{% raw %}
    pub fn source(&self) -> Source {
        let ring = self.ring.clone();
        let mut frames: u64 = 0;
        Box::new(move |buf: &mut [i16]| {
            if ring.restart_fade.swap(false, Ordering::AcqRel) {
                frames = 0;
            }
            let full = f32::from_bits(ring.gain.load(Ordering::Relaxed));
            let fade = u64::from(ring.fade_frames.load(Ordering::Relaxed));
            let gain = tone::faded(full, frames, fade);
            let mut q = ring.samples.lock().unwrap();
            let n = buf.len().min(q.len());
            for (dst, s) in buf.iter_mut().zip(q.drain(..n)) {
                *dst = if gain == 1.0 {
                    s
                } else {
                    (f32::from(s) * gain) as i16
                };
            }
            frames += (n / 2) as u64;
            drop(q);
            ring.space.notify_one();
            buf[n..].fill(0);
            if n < buf.len() && ring.armed.load(Ordering::Relaxed) {
                UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            }
        })
    }
{% endraw %}{% </code> %}

Line 93 clones the `Arc`, so the closure gets a reference of its own to the ring. Line 94 creates a counter, `frames`. The keyword `move` on line 95 moves both into the closure, and `Box::new` turns the closure into the one type `Source`.

The body runs each time a speaker wants samples, and `buf` is the buffer to fill. Lines 96 to 98 set the counter back to zero when a new sound has started. `start` leaves that message in the flag `restart_fade`, and the `swap` takes it. Lines 99 to 101 compute the gain for this buffer. The ring keeps the gain in an `AtomicU32`, because the standard library has no atomic `f32`. `Player::set_gain` stores `gain.to_bits()`, the float's 32 bits as an integer, and line 99 turns them back with `f32::from_bits`. `tone::faded` then returns the full gain once `frames` has reached the fade length, and a smaller value before that.

Line 102 takes the ring's lock. The loop from line 104 moves `n` samples from the ring into the buffer and multiplies each by the gain, and line 111 adds the frames to the counter. Lines 112 and 113 release the lock and wake the decoder.

Line 114 handles a ring that had too little: the rest of the buffer becomes zeros, which is silence. If the flag `armed` is set, lines 115 to 117 count this as an *underrun*, a moment where the decoder did not keep up. `push` sets `armed` once the ring is half full, so the empty ring at the start of a sound does not count.

{% <tip title="A closure that keeps its own state" id="sound-closure-counter"> %}
`frames` is a local variable of `source`, and yet it outlives the call. The `move` closure owns it, so it lives as long as the box does.

The closure changes `frames` on lines 97 and 111. That is why `Source` is an `FnMut`: a closure that only read what it captured would be an `Fn`. No other code can reach the counter, so it needs neither a lock nor an atomic.
{% </tip> %}

## Two speakers, two sources

The start-up in [chapter 4](@/wakey/04-startup.md) calls `player.source()` twice and gives one closure to each speaker. Each speaker's thread calls its closure at the pace of its hardware.

For the built-in speaker that is the `speaker` thread, in `Mixer::run`. While the stream is on, each round of its loop calls the source for 96 frames, which is 2 ms of sound, and writes the block to the driver for I2S, the bus that carries digital audio to the audio chip. The comment on `write_block` says that this write waits while the driver's buffers are full, and that this paces the loop.

The USB speaker works through *transfers*, blocks of 8 ms of sound that the `uac` thread hands to ESP-IDF's USB library. Three are under way at a time. When the library returns a finished one, it calls the function `on_iso_done` on the `uac` thread, which fills the transfer from the source and submits it again. Its `unsafe` belongs to [chapter 19](@/wakey/19-unsafe.md).

{% <pitfall title="Two sources, one ring" id="sound-two-sources-one-ring"> %}
Both closures drain the same ring, and the types allow both to do so at once. The comment on `source` states the rule that the types do not: only one may take from it at a time. Each closure also has its own `frames`, so a change of speaker during an alarm continues the fade-in from a stale count. After this commit the count is in the ring, shared.

The code keeps the rule in `Output`.
{% </pitfall> %}

## `Output` chooses the speaker

`Output` owns the handles of both speakers. It has two fields of state: `wanted`, which `set_streaming` writes before it calls `update`, and `route`, an enum with the variants `Silent`, `Usb` and `Builtin`. This is `update`.

{% <code file="firmware/src/audio/output.rs" lines="52-74" hl="53-57 63-68"> %}{% raw %}
    pub fn update(&mut self) {
        let route = match (self.wanted, self.usb.connected()) {
            (false, _) => Route::Silent,
            (true, true) => Route::Usb,
            (true, false) => Route::Builtin,
        };
        // ... @62
        // Off before on: the two must not take from the player together.
        if self.route == Route::Usb {
            self.usb.set_streaming(false);
        }
        if self.route == Route::Builtin {
            self.builtin.set_streaming(false);
        }
        match route {
            Route::Usb => self.usb.set_streaming(true),
            Route::Builtin => self.builtin.set_streaming(true),
            Route::Silent => {}
        }
        self.route = route;
{% endraw %}{% </code> %}

Lines 53 to 57 compute the route that should apply now. The `match` looks at a pair: is sound wanted, and is a USB speaker connected? `usb.connected()` reads an atomic that the `uac` thread writes. If no sound is wanted, the second value does not matter, which is what the `_` says. So three arms cover all four combinations.

The skipped lines return early when the route has not changed. Otherwise lines 63 to 68 send the old speaker its command to stop, and only then does the `match` on line 69 send the new one its command to start.

Besides `set_streaming`, `AlarmClock::tick` calls `update` once a second, because the USB speaker may have come or gone. So if someone unplugs the speaker while the alarm rings, the built-in speaker takes over within a second.

For our two questions, this chapter shows parts that work together by pulling: the speaker's hardware sets the pace, and a full ring slows the decoder down. The state lives in three places: the ring, shared through an `Arc` by the player, the decoder and both sources, the counter in each source, and the route inside `Output` on the `app` thread. The [next chapter](@/wakey/18-radio.md) turns to the radio.
