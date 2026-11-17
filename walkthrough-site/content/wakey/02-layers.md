+++
title = "Layers, and the rules that keep them"
description = "The module map, and how a service gets what it needs without knowing where it comes from."
weight = 2

[extra]
map = ["app", "net", "ambient"]
map_note = "The map shows the four layers of this chapter: `app`, the services, the hardware modules and the support modules. We look most closely at `app`, `net` and `ambient`."
goals = [
  "which modules the firmware crate has, and which layer each belongs to",
  "how a service gets what it needs as arguments instead of importing it",
  "when the code passes a `fn` pointer, when a closure and when a boxed closure",
  "what a cheap `Clone` handle over an `Arc` is, and why services return one",
]
takeaways = [
  "The firmware crate has four layers: `app`, services, hardware and support. No module uses a layer above its own.",
  "The compiler does not check this rule inside one crate. The code keeps it by passing things downwards as arguments.",
  "A service asks for the smallest thing that does the job: a `fn() -> bool` for one question, a boxed closure for one power switch.",
  "A service that runs a thread returns a handle that is cheap to clone, and every clone talks to the same thread.",
]
+++

The [previous chapter](@/wakey/01-clock-board-crates.md) split the project into two crates. This chapter looks inside the larger one, `firmware`, which holds everything that touches hardware. It has seventeen modules, and before we read any of them we need a map that says which module may use which.

First we read the map, which is a doc comment of fourteen lines. Then we look at what keeps the modules apart.

## Four layers in one doc comment

The map is at the top of `lib.rs`, the crate's root file. The line to watch is the first one, because it states the rule.

{% <code file="firmware/src/lib.rs" lines="4-17" hl="4"> %}{% raw %}
//! Layers, top to bottom; none uses a layer above it:
//!
//! - [`app`]: the alarm clock application (screens, wiring, UI texts).
//! - Services: [`ui`] (Slint, touch, list layer, page slides), [`audio`]
//!   (USB speaker, built-in speaker, file playback), [`net`] (Wi-Fi
//!   sessions, NTP), [`tidal`] (login, playlist, downloads) over [`http`],
//!   [`airdrop`] (sound files from iPhones), [`sounds`] (the card's sound
//!   files), [`clock`] (local time, RTC), [`config`] (`config.json`),
//!   [`ambient`] (backlight from the room's light, camera as light meter).
//! - Hardware: [`board`] (pins, chips, power switches, bring-up),
//!   [`display`] (panel, frame buffers, PPA, 2D-DMA) and [`c6`] (the Wi-Fi
//!   co-processor: power, esp_hosted's transport, its images).
//! - Support: [`runtime`] (threads, the Embassy executor), [`psram`]
//!   (DMA buffers), [`taskstats`] (CPU time per task).
{% endraw %}{% </code> %}

Line 4 says that no layer uses one above it. The rest of the comment lists the layers from top to bottom.

The top layer is a single module, `app` (line 6). It holds the screens, the texts, and the "wiring", which is the code that connects all the other parts.

The second layer, in lines 7 to 12, is the *services*. There are ten. For example, `ui` draws the screen with Slint, a toolkit for user interfaces, and `net` brings up Wi-Fi and fetches the time with NTP, the network time protocol. In the mini-map at the top of this page, `http` has no box of its own.

The third layer, in lines 13 to 15, is the hardware. One of its modules is `c6`, named after the C6, a second chip on the board that does the radio work. The fourth layer is support, for example `runtime`, which starts threads and the executor.

{{ <svg src="img/layers.svg" caption="The four layers of the `firmware` crate with their modules. Each arrow points from a module to one that it uses; only a few of the real dependencies are drawn. The crate `wakey-core` stands beside the layers." /> }}

In the figure, every arrow points downwards, or sideways to `wakey-core`. None points up, and that is the whole rule.

{% <pitfall title="The compiler does not check the layers" id="layers-not-checked"> %}
All seventeen modules are declared in `lib.rs` with `pub mod`, in one crate. Inside a crate, any module can use the public items of any other. Nothing stops `board` from writing `use crate::app`, so the rule in line 4 is a convention. Only the border to `wakey-core` is enforced, because that is a separate crate without any ESP-IDF dependency.

Inside the firmware, the code keeps the rule by a habit instead. A module that needs something from elsewhere receives it as an argument.
{% </pitfall> %}

## A service receives what it needs as arguments

The project's documentation states the habit in one sentence: services get what they need as arguments. The service `net` shows three variants of it.

`net` runs a thread called `wifi`. From time to time this thread switches the C6 on, sets the clock by NTP and switches the C6 off again. For that it needs three things from elsewhere. It must switch the C6's power. It must know whether the display is asleep, because it starts no periodic session then. And it must tell somebody that the time has been set. Here is the struct that `net` asks for. Two of the three are in the highlighted lines.

{% <code file="firmware/src/net.rs" lines="173-187" hl="177 186"> %}{% raw %}
/// What the `wifi` thread needs.
pub struct Setup {
    pub modem: Modem<'static>,
    /// The C6's 3.3 V (expander pin WLAN_PWR_EN).
    pub c6_power: Switch,
    // ... @185
    /// The display sleeps: no periodic session then.
    pub display_asleep: fn() -> bool,
}
{% endraw %}{% </code> %}

Look at the types in lines 177 and 186. `c6_power` is a `Switch`, which we come to in a moment. `display_asleep` is a `fn() -> bool`, a pointer to a function that takes nothing and answers yes or no. Neither field says where its value comes from. `net` can ask "is the display asleep?", and it knows nothing about who answers.

The answers are filled in by `app`, in the start-up function.

{% <code file="firmware/src/app/mod.rs" lines="189-200" hl="192 197 199"> %}{% raw %}
    let wifi = net::start(
        net::Setup {
            modem,
            c6_power: ioexp.switch(Pin::WlanPwrEn),
            c6_link,
            nvs: nvs.clone(),
            cfg: config.wifi.clone(),
            interval: Duration::from_secs(u64::from(config.sync_interval_h) * 3600),
            display_asleep: crate::ui::power::asleep,
        },
        move || clock::store_system_time(&rtc),
    );
{% endraw %}{% </code> %}

Line 197 gives `net` the function `asleep` from the module `ui::power`. It is an ordinary function that reads a flag, and [chapter 9](@/wakey/09-flags-and-locks.md) looks at the flag. The important point is that this line is in `app`, so `app` is the module that knows both `ui` and `net`. The file `net.rs` does not import `ui` at all. Note that `ui` is a service like `net`, so the layer rule alone would allow the import. The code is stricter here than its own rule.

Line 199 is the third thing. It is a closure, the second argument of `net::start`. The board has an RTC, a real-time clock chip that keeps the time for the next boot. After every successful NTP sync, the `wifi` thread calls this closure, and the closure writes the fresh time to the RTC. The closure owns the `rtc` handle, because `move` has moved it in. So `net` never sees the RTC. It only has something to call.

## A `Switch` instead of the expander

Now back to line 192 and the type `Switch`. First the hardware behind it. An IO expander is a small chip that gives the processor more output pins. This board has two of them, with eight pins each. Their pins switch, among others, the speaker amplifier, the 5 V of the USB port and the 3.3 V of the C6.

The handle for both expanders is called `IoExpanders`, and it can drive every one of those pins. The `wifi` thread needs exactly one. The next excerpt shows the type that makes this possible, in line 72, and the method that creates a value of it.

{% <code file="firmware/src/board/ioexp.rs" lines="70-171" hl="72 169-170"> %}{% raw %}
/// A power switch on an expander pin, for services that manage their own
/// supply (the USB-A 5 V, the C6's 3.3 V).
pub type Switch = Box<dyn Fn(bool) -> Result<(), EspError> + Send>;
    // ... @167
    /// A [`Switch`] driving `pin`.
    pub fn switch(&self, pin: Pin) -> Switch {
        let this = self.clone();
        Box::new(move |on| this.set(pin, on))
    }
{% endraw %}{% </code> %}

A `Switch` is a boxed closure. It takes `true` or `false` and reports whether the switching worked. The `+ Send` allows it to be moved to another thread, and the `wifi` thread will own this one.

The method `switch` clones the handle in line 169. Line 170 moves the clone into a new closure, together with one `pin`. The expander, the bus and the pin number are hidden inside the closure. So when `app` passes `ioexp.switch(Pin::WlanPwrEn)`, `net` receives the power switch of the C6 and nothing else. [Chapter 7](@/wakey/07-sharing-hardware.md) opens up `IoExpanders` and shows why cloning it is safe.

{% <tip title="Three ways to pass a function" id="passing-functions"> %}
`fn() -> bool` is a function pointer. It accepts only functions and closures that capture nothing. In exchange it is `Copy` and one pointer in size.

`impl Fn() + Send + 'static` as a parameter accepts any closure, including one that owns data, like the closure that owns `rtc`. But this type has no name, so a struct field cannot have it unless the struct becomes generic.

A `Box<dyn Fn>` such as `Switch` also accepts any closure, and it has a name, so it can be a field of `Setup`. The price is one heap allocation and an indirect call.
{% </tip> %}

## The same idea in `ambient`

The service `ambient` is built the same way. Its thread uses the camera as a light meter and sets the backlight to match the room. In the dark it also makes the pixels darker, and that scaling is done in `ui`. So `ambient` needs a way to reach the screen.

It asks for a small struct, `ambient::Screen`, which its comment calls the display "as the light meter sees it". The struct has three function pointers and a number: is the display asleep, how bright are the pixels, set them to this value, and the value that means full brightness. Again `app` fills it in, with `ui::power::asleep` and with two functions and a constant from `ui::dim`. The file `ambient.rs` does not import `ui` either.

## What a service gives back: a handle

So far, things went down into a service. Something also comes back: a handle. `net::start` returns a `Wifi`, and this type has a shape you will see many times.

{% <code file="firmware/src/net.rs" lines="149-154" hl="150"> %}{% raw %}
/// Handle on the `wifi` thread. Cheap to clone.
#[derive(Clone)]
pub struct Wifi {
    status: Arc<Mutex<Status>>,
    jobs: mpsc::Sender<Job>,
}
{% endraw %}{% </code> %}

The struct has two fields. `status` is an `Arc<Mutex<Status>>`, a shared value that says what the Wi-Fi is doing. `jobs` is the sending end of a channel that carries work to the `wifi` thread. The thread holds a second clone of the `Arc` and the receiving end of the channel.

Line 150 derives `Clone`, and the comment above it promises that cloning is cheap. Cloning an `Arc` increases a counter, and cloning a `Sender` makes one more sending end for the same channel. So every clone of `Wifi` talks to the same thread.

Both fields are private, so a holder of the handle can only call its two methods, `status` and `run`. [Chapter 8](@/wakey/08-channels.md) covers the channel, and chapter 9 the lock.

{% <tip title="A cheap `Clone` handle over an `Arc`" id="clone-handle"> %}
When several parts of a program need to reach one thread or one piece of shared state, give each a clone of a small struct that holds an `Arc` or a channel sender. Each part owns its clone, so no part needs a reference with a lifetime.

`Wifi`, `UsbAudio`, `Tidal` and `IoExpanders` all have this shape, and each says "Cheap to clone" in its doc comment.
{% </tip> %}

## What only `app` knows

We end with the top layer. The `use` lines at the top of `app/mod.rs` name eleven of the other sixteen modules, and no module imports `app`. The project's documentation says what belongs here: `app` is the only place that knows the whole product. That means the order of the start-up, which page shows what, and in which words. The `wifi` thread and the display's sleep flag meet in line 197 of `app/mod.rs`, and nowhere else.

For our two questions, this chapter gives a first answer to each. The parts work together because `app` connects them, by passing small things downwards: a function pointer, a closure, a `Switch`. The state of a service lives inside the service, and the rest of the program reaches it through a handle. The [next chapter](@/wakey/03-executor.md) starts the program.
