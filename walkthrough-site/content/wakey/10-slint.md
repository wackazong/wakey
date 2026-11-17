+++
title = "Slint in one page, and the platform behind it"
description = "Just enough Slint to read the Rust that talks to it, and how the firmware gives Slint a screen and a clock."
weight = 10

[extra]
map = ["ui", "app"]
map_note = "The `.slint` files are turned into Rust types that live in `app`. The module `ui::platform`, one layer below, gives those types a window to draw into."
goals = [
  "what a `.slint` file declares, and which Rust methods each declaration becomes",
  "how `build.rs` and one `include!` bring the generated code into `app`",
  "what a Slint platform is, and how `TabPlatform` hands out windows",
  "why all user interface code has to run on the `app` thread",
]
takeaways = [
  "A `.slint` file declares components, properties, callbacks and globals. The Slint compiler turns them into Rust types with `get_`, `set_`, `on_` and `invoke_` methods.",
  "`build.rs` compiles `ui/wakey.slint` when the firmware is built, and `app/mod.rs` includes the result in a module called `screens`.",
  "`TabPlatform` gives Slint two things: the time since start, and a window for each new component. Windows are prepared first and handed out in the same order, because Slint does not say which component is asking.",
  "The feature `unsafe-single-threaded` is a promise by the firmware that only one thread touches Slint. That thread is `app`.",
]
+++

The last two chapters were about threads and how they talk to each other. This chapter turns to one thread, `app`, and to its main work, the user interface. The firmware draws it with Slint, a toolkit for user interfaces that [chapter 2](@/wakey/02-layers.md) mentioned in one sentence.

Slint brings its own small language, and this walkthrough is about Rust. So this chapter contains the only Slint you need to read: enough to understand the Rust code that talks to it. After that we look at the piece of Rust that Slint needs before it can draw anything on this board, which is called a platform.

## What a `.slint` file declares

The screens are not written in Rust. They are described in files that end in `.slint`, in the folder `firmware/ui`. The main file is `ui/wakey.slint`, and it imports one file per page. Here is the start of the one window the clock has. Look at the first line and at line 44.

{% <code file="firmware/ui/wakey.slint" lines="38-44" hl="38 44" lang="slint"> %}{% raw %}
export component AppWindow inherits Window {
    width: 1280px;
    height: 720px;
    background: Theme.background;
    default-font-family: "Inter 24pt Medium";

    in-out property <Page> page;
{% endraw %}{% </code> %}

Line 38 declares a *component*, a piece of user interface with a name. This one is called `AppWindow`, and `inherits Window` says that it is a whole window and not a part of one. The lines below it set the window's size, background and font.

Line 44 declares a *property*, a named value with a type that belongs to the component. The property `page` has the type `Page`, an enum that the same file declares, with an entry for each page and a few more. Further down, the file shows one page or another depending on this value. So the window shows one page at a time, and whoever sets `page` decides which one.

The words in front of `property` say who may do that. An `in` property is set from outside the component, which here means from Rust. An `in-out` property can be set from both sides.

A window is not the only place for properties. The pages keep theirs in *globals*. The next excerpt is the global of the alarm list page. Watch for one property and one callback.

{% <code file="firmware/ui/wakey/alarm_list.slint" lines="72-85" hl="73 83" lang="slint"> %}{% raw %}
export global AlarmList {
    in property <[AlarmRow]> rows;
    // ... @80
    callback back();
    callback add();
    callback open(int);
    callback toggle(int, bool);
    callback delete(int);
}
{% endraw %}{% </code> %}

A global is a group of properties and callbacks that exists once per window and has a name, here `AlarmList`. Every element of the page can read it, and so can Rust. Line 73 is the list's content: `rows` is an array of `AlarmRow`, a struct that the same file declares.

Line 83 is a *callback*. A callback is the opposite direction of a property. The Slint side calls it when something happens, and Rust supplies the code that runs. `toggle(int, bool)` is called when the user taps the switch of an alarm, with the alarm's id and the new position of the switch.

That is all the Slint we need. A page is a component, it reads its content from the properties of a global, and it reports what the user did through the callbacks of the same global.

## From `.slint` to Rust

Something has to turn these declarations into Rust types, and that happens while the firmware is built. Cargo runs a crate's `build.rs` before it compiles the crate itself. The firmware's `build.rs` calls the Slint compiler, the crate `slint_build`, once for each of two files: `ui/wakey.slint` for the clock, and a second file for the test bench that [chapter 3](@/wakey/03-executor.md) mentioned. A comment there says where the result goes: into a file named after the `.slint` file, in the directory that Cargo reserves for build output and names in the environment variable `OUT_DIR`. For us that file is `wakey.rs`.

One line in `app/mod.rs` brings it in.

{% <code file="firmware/src/app/mod.rs" lines="39-52" hl="41 49"> %}{% raw %}
/// Components generated from `ui/wakey.slint` (see `build.rs`).
mod screens {
    include!(concat!(env!("OUT_DIR"), "/wakey.rs"));
}

use screens::{AppWindow, Page};

/// A window with the language's texts: the screen's, or one that renders
/// rows offscreen.
fn new_window() -> AppWindow {
    let window = AppWindow::new().expect("UI window");
    texts::apply(&window);
    window
}
{% endraw %}{% </code> %}

Line 41 pastes the generated file into a module called `screens`. From then on `screens::AppWindow` is an ordinary Rust struct, and `screens::Page` is an ordinary Rust enum.

The generated types follow a pattern that you can read off the firmware's own calls. For a property there is a setter and a getter: `page` becomes `set_page` and `get_page`, and `error-text` becomes `set_error_text`, because Rust names cannot contain a hyphen. For a callback there is a method that installs a handler, so `toggle` becomes `on_toggle`, and one that calls it, such as `invoke_delete`. A global is reached through the window, as in `window.global::<AlarmList>()`.

{% <tip title="`include!`: generated code in a module of its own" id="slint-include-generated"> %}
Line 41 is built from three macros that run while the crate is compiled. `env!("OUT_DIR")` becomes the variable's value as a string literal. `concat!` joins it with `"/wakey.rs"`. `include!` reads the file with that name and puts its content in place of the macro call. Wrapping the call in `mod screens` keeps the many generated names out of `app`'s own namespace.

Slint has a macro of its own for this, `slint::include_modules!()`. The comment in `build.rs` says why the firmware does not use it: it only knows the last file that was compiled, and here there are two.
{% </tip> %}

The second half of the excerpt, `new_window`, is where a window comes into existence. Line 49 calls `AppWindow::new()`, and line 50 fills in the texts of the chosen language. `new` returns a `Result`, and the next section shows one way in which it can fail here.

## A platform for a board without a desktop

`AppWindow::new()` creates a component, and a component needs a window to appear in. On this board there is no operating system that could open one. Slint solves this with a trait called `Platform`. The program implements the trait and registers the implementation, and from then on Slint asks it for what it cannot find out by itself.

The firmware's implementation is `TabPlatform` in `ui/platform.rs`. In the next excerpt, look at the `static` at the top and at how the first method uses it.

{% <code file="firmware/src/ui/platform.rs" lines="23-43" hl="25 35-36"> %}{% raw %}
thread_local! {
    /// Windows handed out, in order, to the next components created.
    static PENDING: RefCell<VecDeque<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(VecDeque::new()) };
}

/// The Slint platform: hands out the prepared windows and tells the time.
struct TabPlatform {
    start: Instant,
}

impl Platform for TabPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        PENDING
            .with(|p| p.borrow_mut().pop_front())
            .map(|w| w as Rc<dyn WindowAdapter>)
            .ok_or_else(|| PlatformError::Other("no window prepared for this component".into()))
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros(self.start.elapsed().as_micros())
    }
{% endraw %}{% </code> %}

The struct has a single field, the moment at which the platform was created. The two methods are two questions that Slint asks.

The first question is "give me a window for the component I am creating". `create_window_adapter` answers it from `PENDING`, a queue of windows that were prepared earlier. `PENDING` is declared with `thread_local!`, so every thread has its own copy of it, and that is why a `RefCell` without any lock is enough. `pop_front` in line 36 takes the oldest window out. If the queue is empty, line 38 returns an error instead, and that error is what the `expect` in `new_window` would turn into a panic.

The second question is "how late is it". Slint has timers and animations, and `duration_since_start` gives it the clock for them: the time since `start`.

The windows in the queue come from two functions in the same file. Watch the order of events in `init`.

{% <code file="firmware/src/ui/platform.rs" lines="54-78" hl="55 59 76"> %}{% raw %}
pub fn init() -> Rc<MinimalSoftwareWindow> {
    slint::platform::set_platform(Box::new(TabPlatform {
        start: Instant::now(),
    }))
    .expect("Slint platform already set");
    prepare_window(RepaintBufferType::SwappedBuffers, UI_WIDTH, UI_HEIGHT)
}
// ... @69
fn prepare_window(
    repaint: RepaintBufferType,
    width: u32,
    height: u32,
) -> Rc<MinimalSoftwareWindow> {
    let window = MinimalSoftwareWindow::new(repaint);
    window.set_size(slint::PhysicalSize::new(width, height));
    PENDING.with(|p| p.borrow_mut().push_back(window.clone()));
    window
}
{% endraw %}{% </code> %}

`init` is called once. Normally `app::app` calls it, just before the first `new_window()`. If `config.json` cannot be read, `config_error` in the same file calls it instead. Lines 55 to 58 register the platform with Slint. Line 59 then prepares the main window, which is as large as the whole user interface, 1280 by 720 pixels.

`prepare_window` does three things. It creates a `MinimalSoftwareWindow`, the window type of Slint's software renderer, which computes every pixel on the processor. It is not a window on any screen: it has a size, and it draws when it is asked to. Then line 76 pushes one reference to it into `PENDING`, and line 77 returns another to the caller. Both are `Rc` clones of the same window.

Slint gets one through `create_window_adapter` and connects it to the new `AppWindow`. The firmware keeps the other, and `app::app` passes it to the render loop, which uses it to make Slint draw. [Chapter 12](@/wakey/12-touch-in.md) and [chapter 13](@/wakey/13-pixels-out.md) are about that loop.

The lines we skipped hold `offscreen_window`, which calls `prepare_window` with another size. The alarm list and the sound list each prepare one such window, as large as a single row, and then create a second and third `AppWindow` for it with `new_window()`. Those windows never reach the panel. The firmware uses them to draw pictures of single rows.

{{ <svg src="img/slint-bridge.svg" caption="At build time the `.slint` files become Rust types. At run time each new `AppWindow` gets the oldest window from the queue `PENDING`." /> }}

{% <pitfall title="Prepare the window, then create the component" id="slint-window-order"> %}
`create_window_adapter` has no parameters. Slint does not say which component it is creating, so the platform cannot choose a fitting window. The only link between a prepared window and its component is the order: the first window prepared goes to the first component created.

The firmware keeps the two steps next to each other. `platform::init()` is followed by `new_window()`, and so is each call to `offscreen_window`.

Nothing checks this. If two windows were prepared and their components then created the other way round, each component would get the other's window, and the queue would report no error.
{% </pitfall> %}

## One thread for all of Slint

One setting in `firmware/Cargo.toml` decides where all of this code may run. The `slint` dependency is built with the feature `unsafe-single-threaded`, and without Slint's `std` feature.

The copy of Slint's core in `firmware/vendor` shows what the feature does. Slint keeps internal state in thread-local variables. Built this way, it replaces them with plain statics, and it declares those statics safe to share between threads with an `unsafe impl`. The comment next to it gives the condition: the feature means that Slint is only ever called from a single thread.

{% <pitfall title="A feature flag can be a promise" id="slint-single-thread-promise"> %}
Most Cargo features switch code on. This one also moves a duty from the library to the program. The firmware promises that one thread alone creates windows, sets properties and runs callbacks.

This firmware has more than ten threads, and it keeps the promise by its structure. Everything that touches Slint is called from the one task on the `app` thread.
{% </pitfall> %}

The other threads still have things to tell the user interface. They use the mechanisms of the last two chapters: the touch thread sends events through a channel, and the light meter sets an atomic that a timer on the `app` thread reads. The Slint call itself happens on `app`.

For our two questions, this chapter adds a boundary. Rust and Slint work together through generated methods: Rust sets properties and installs callbacks, and Slint calls them. As for state, a property holds what a page shows. Where the state behind it lives, and how Rust fills these properties and answers these callbacks, is the subject of the [next chapter](@/wakey/11-view-models.md).
