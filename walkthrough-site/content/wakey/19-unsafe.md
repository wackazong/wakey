+++
title = "Every `unsafe`, and why it is there"
description = "All of the firmware's `unsafe`, by kind, with what makes each one sound."
weight = 19

[extra]
map = ["display", "audio", "c6", "psram"]
map_note = "`unsafe` sits where Rust meets ESP-IDF's C code and the chip itself. Most of it is in the display, the USB audio and the C6 modules; `app` and `wakey-core` have none."
goals = [
  "how many `unsafe` sites the firmware has, where they are, and how to count them yourself",
  "the few kinds they fall into, and what to check for each kind",
  "how safe types keep `unsafe` out of the rest of the firmware",
]
takeaways = [
  "The word `unsafe` appears on 135 lines in 25 files: 115 blocks and 20 other items. None of them is in `app/` or in `wakey-core`.",
  "More than half of the blocks are plain calls into ESP-IDF. The ones worth your time are those that turn a raw pointer into a slice or a reference.",
  "A good `SAFETY` comment names facts you can look up: how long the memory lives, how large it is, and who else can touch it.",
  "Safe types such as `PsramBuffer`, `Transport` and `Display` own the raw handles, so the layers above them never write `unsafe`.",
]
+++

Earlier chapters met `unsafe` a few times and promised that this chapter would cover all of it. We first count every site, then sort the sites into a few kinds, and then look at one example of each kind and ask what you would have to check to believe it.

An `unsafe` block does not turn the borrow checker off. It allows a handful of extra operations, such as calling a C function or reading through a raw pointer, whose rules the compiler cannot check. The rules still apply, and the firmware writes down how it keeps them in a comment that starts with `// SAFETY:`.

## Two lints frame all of it

Two lines of the firmware's manifest decide how every `unsafe` in the crate has to be written.

{% <code file="firmware/Cargo.toml" lines="21-27" hl="22 27" lang="toml"> %}{% raw %}
[lints.rust]
unsafe_op_in_unsafe_fn = "deny"

[lints.clippy]
mem_forget = "deny"
large_stack_frames = "deny"
undocumented_unsafe_blocks = "warn"
{% endraw %}{% </code> %}

Line 22 concerns functions that are themselves declared `unsafe fn`. With `unsafe_op_in_unsafe_fn` set to `deny`, the body of such a function does not count as one large `unsafe` block: each unsafe operation inside it needs a block of its own, or the build fails.

Line 27 switches on a Clippy lint. Clippy is Rust's additional linter, and this lint warns about every `unsafe` block that has no `// SAFETY:` comment above it. It is a warning and not an error, and `CONTRIBUTING.md` says that the firmware's Clippy runs on the developer's machine and not in CI.

## The inventory

You can repeat the count with `grep -rn unsafe firmware/src`. It prints 135 lines in 25 files, and every one of them is code, because the comments write `SAFETY` in capitals. We sort the lines by what follows the keyword.

- 115 lines open an `unsafe` block. The table's number columns sort them.
- 8 lines declare an `unsafe extern "C" fn`, a function that C code calls back.
- 5 lines declare an `unsafe fn` of the firmware's own.
- 3 lines open an `unsafe extern "C"` block, which declares C symbols that the generated bindings lack.
- 2 lines are the attribute `#[unsafe(no_mangle)]`.
- 1 line is an `unsafe impl Send`, and 1 line is a type alias for a callback pointer.

Many blocks do several things, so each block is counted once, under the first of these kinds that fits. "Zeroed" means it creates a C struct with `core::mem::zeroed()`. "Registers" means it accesses registers of the hardware or the CPU directly. "Raw memory" means it reads or writes other memory through a raw pointer, makes a slice or a reference from one, or reads a union field. "Reborrow" is the second handle to a peripheral from [chapter 7](@/wakey/07-sharing-hardware.md). "Own" means it calls one of the firmware's five `unsafe fn`. What remains is "FFI": a call into C and nothing else.

|File in `firmware/src`|Blocks|FFI|Zeroed|Raw memory|Registers|Reborrow|Own|Other items|
|---|---|---|---|---|---|---|---|---|
|`airdrop.rs`|2|||||2|||
|`ambient.rs`|8|4|1|1|2|||1 callback|
|`audio/uac/device.rs`|29|13|1|13|||2|3 callbacks, 1 `unsafe fn`|
|`audio/uac/host.rs`|5|3|1||1|||1 `extern` block|
|`audio/uac/mod.rs`|1|1|||||||
|`board/i2c.rs`|1|||||1|||
|`board/sd.rs`|3|2|1||||||
|`c6/mod.rs`|6|6|||||||
|`c6/ota.rs`|6|6|||||||
|`c6/peer.rs`|3|2||1||||1 callback|
|`clock.rs`|6|4|2||||||
|`display/cache.rs`|2|2||||||1 `extern` block, 2 `unsafe fn`|
|`display/dma2d.rs`|4|2||2||||1 `extern` block, 1 callback, 1 type alias, 1 `unsafe fn`|
|`display/mod.rs`|14|7|3|3|1|||1 callback, 2 `no_mangle`|
|`display/ppa.rs`|6|2|2|2||||1 callback|
|`net.rs`|3|2||||1|||
|`psram.rs`|2|2||||||1 `unsafe impl Send`|
|`taskstats.rs`|4|2||2|||||
|`tidal/card_writer.rs`|2|||2|||||
|`ui/list_layer/corners.rs`|2|||1|||1|1 `unsafe fn`|
|`ui/list_layer/mod.rs`|2||||||2||
|`ui/platform.rs`|1|||1|||||
|`ui/stats.rs`|1|1|||||||
|`ui/trace.rs`|1||||1||||
|`ui/transition.rs`|1||||||1||
|**Total**|**115**|**61**|**11**|**28**|**5**|**4**|**6**|**20**|

Two things stand out. The directory `app/`, which holds the alarm clock's behaviour, is not in the table, and `wakey-core` cannot be, because its manifest forbids `unsafe` ([chapter 1](@/wakey/01-clock-board-crates.md#forbid-unsafe)). And one file, the USB audio driver `audio/uac/device.rs`, has a quarter of all blocks.

## Calls into C, and the structs they want

ESP-IDF is written in C, and Rust treats every C function as `unsafe` to call, because it cannot see what the function does with its arguments. FFI, the foreign function interface, is the name for such calls, and they explain 61 blocks. Their comments are short ("plain call; `tv` lives for it"), and so is the check: every pointer that is passed must stay valid for as long as C uses it, which is usually the duration of the call.

C APIs often take a struct with many fields. The next excerpt shows how the firmware creates such structs. It is the start of `clock::local`, which turns a Unix time into a local date with the C library.

{% <code file="firmware/src/clock.rs" lines="108-115" hl="109-112"> %}{% raw %}
pub fn local(unix: i64) -> DateTime {
    // SAFETY: `tm` is plain data (all zeros is valid) and an out-parameter
    // of the reentrant `localtime_r`.
    let tm = unsafe {
        let mut tm: sys::tm = core::mem::zeroed();
        sys::localtime_r(&unix, &mut tm);
        tm
    };
{% endraw %}{% </code> %}

Line 112 creates a `sys::tm` in which every byte is zero, and line 113 passes it to `localtime_r`, which fills it in. The comment claims that all zeros is a valid `tm`. The display code creates its configuration structs the same way and then sets the fields it needs.

{% <unsafe title="`mem::zeroed()` is only sound for some types" id="unsafe-zeroed-config"> %}
`core::mem::zeroed()` returns a value whose bytes are all zero. That is undefined behaviour for any type in which zero is not a valid value, for example a reference, which may never be null.

So the thing to check is the struct's fields. The C structs here contain integers, floats, booleans, raw pointers and optional function pointers. Zero is valid for all of these: a zeroed `bool` is `false`, a raw pointer may be null, and a zeroed `Option` of a function pointer is `None`.
{% </unsafe> %}

## Functions that C calls

The firmware also hands functions to C. Eight of them are `unsafe extern "C" fn` callbacks, and several run inside an interrupt handler. Here is the smallest one that receives a pointer, from the 2D-DMA copy engine in `display/dma2d.rs`.

{% <code file="firmware/src/display/dma2d.rs" lines="50-55" hl="51-53"> %}{% raw %}
unsafe extern "C" fn on_done(_m: Handle, _e: *mut c_void, done: *mut c_void) -> bool {
    // SAFETY: `done` is the `AtomicU32` boxed in the `FbCopy`, which lives
    // as long as its copies (it waits for them before dropping).
    unsafe { &*done.cast::<AtomicU32>() }.fetch_add(1, Ordering::Release);
    false
}
{% endraw %}{% </code> %}

The driver passes back, as `done`, a pointer that the firmware gave it when the copy started. Line 53 casts it to an `AtomicU32` and adds one.

{% <unsafe title="A callback's context pointer must outlive the callback" id="unsafe-callback-context"> %}
C callbacks carry their state as an untyped pointer. Rust cannot know what it points to or whether the target still exists, so two things need checking in `on_done`.

The type: the field `done` of `FbCopy` is a `Box<AtomicU32>`, and `FbCopy::start` passes the address of the counter in that box. So the cast is right, and the box keeps the address fixed when the `FbCopy` moves.

The lifetime: `FbCopy` implements `Drop`, and its `drop` waits until the last copy has finished. So the counter cannot be freed while an interrupt may still write to it.
{% </unsafe> %}

Most other callbacks avoid the question. The display's and the one in `display/ppa.rs` touch only statics, and the USB driver's get a `CallbackState` that was leaked with `Box::leak` and so lives for good. [Chapter 13](@/wakey/13-pixels-out.md) covers the display's callback.

Two more functions are called by C by name. The display driver in `firmware/vendor` is ESP-IDF's own with one change: it allocates and frees its frame buffers through two functions that it defines as weak symbols. The firmware defines the same two names in Rust. Here is the start of the one that frees.

{% <code file="firmware/src/display/mod.rs" lines="177-178" hl="177"> %}{% raw %}
#[unsafe(no_mangle)]
extern "C" fn esp_lcd_dpi_panel_free_fb(fb: *mut c_void) {
{% endraw %}{% </code> %}

Line 177 is the attribute. Normally Rust changes function names in the binary; `no_mangle` keeps the name exactly as written. Since the 2024 edition the attribute must be wrapped in `unsafe(...)`, because a name that collides with another symbol can replace a function that other code relies on. Here replacing is the purpose. What you have to check is in the vendored C file: the names and signatures match, and the C definitions are weak.

## Raw memory and hardware registers

The 28 raw-memory blocks deserve the most care. A slice promises that the memory exists, holds that many initialised elements, and is not written by anyone else while the slice lives. With DMA, hardware that copies data on its own, "anyone else" includes the chip. This is how the light meter in `ambient.rs` looks at a camera frame.

{% <code file="firmware/src/ambient.rs" lines="377-382" hl="381-382"> %}{% raw %}
        // SAFETY: the buffer holds `PIXELS` RGB565 pixels and is cache-line
        // aligned; the frame is complete and the driver invalidated the
        // cache for the buffer before the callback: these reads see it.
        // No DMA writes until the next `receive`.
        let frame =
            unsafe { core::slice::from_raw_parts(self.buf.as_mut_ptr().cast::<u16>(), PIXELS) };
{% endraw %}{% </code> %}

`self.buf` is a `PsramBuffer`, a buffer in PSRAM, the board's 32 MB of external memory, and it only hands out a pointer. The comment covers each promise of a slice in turn. The buffer holds `PIXELS` pixels and is aligned. The frame is complete and visible to the CPU. And nothing writes while the slice lives, because the camera's DMA starts again only with the next `receive`, which this same thread calls. `Display::back_buffer` argues the same way for a frame buffer; [chapter 4](@/wakey/04-startup.md) showed that line, and chapter 13 checks it in [a callout](@/wakey/13-pixels-out.md#unsafe-display-back-buffer).

`PsramBuffer` also has the crate's only `unsafe impl`, in `psram.rs` line 17: `unsafe impl Send for PsramBuffer`. A struct with a raw pointer is not `Send`, so without this line no thread could be handed a buffer. The comment claims that the buffer owns its allocation and that nothing ties it to a thread. Who may touch the memory at a given moment is not part of the claim; each slice has to argue that for itself.

Five blocks access registers directly. The most unusual one is in `audio/uac/host.rs`.

{% <code file="firmware/src/audio/uac/host.rs" lines="7-29" hl="9 25-27"> %}{% raw %}
unsafe extern "C" {
    /// Register block of the high-speed USB-DWC controller (linker symbol).
    static mut USB_DWC_HS: u32;
}
// ... @20
fn set_full_speed_only() {
    // SAFETY: HCFG of the HS controller, whose registers exist and are
    // clocked once `usb_host_install` returned; nothing else writes it
    // before the port is powered.
    unsafe {
        let hcfg = (&raw mut USB_DWC_HS).cast::<u8>().add(0x400).cast::<u32>();
        let v = hcfg.read_volatile();
        hcfg.write_volatile(v | 1 << 2);
    }
}
{% endraw %}{% </code> %}

{% <unsafe title="A register block as a `static mut`" id="unsafe-static-mut-register"> %}
Line 9 declares a linker symbol: the first register of the USB controller. This is the firmware's only `static mut`.

Line 25 never makes a reference to it. `&raw mut` produces a raw pointer directly, and `read_volatile` and `write_volatile` tell the compiler that each access has an effect and must not be removed or merged.

The comment's claims are about timing: the registers are clocked once `usb_host_install` has returned, and nothing else writes this one before the port is powered. You check both against the order of the calls in `install`, a few lines further down.
{% </unsafe> %}

The four reborrows are the subject of two callouts in chapter 7, for the [I2C pins](@/wakey/07-sharing-hardware.md#unsafe-i2c-recover) and the [modem](@/wakey/07-sharing-hardware.md#unsafe-modem-reborrow); the two in `airdrop.rs` make the same promise for a UART. The six "own" blocks call the firmware's `unsafe fn`, each of which states its contract under a `# Safety` heading.

## What keeps the number small

The table has 8 blocks in `ui/`, 5 in `net.rs` and `tidal/`, and none in `app/`. That is the result of one pattern: a safe type owns the raw resource, its methods contain the `unsafe`, and its `Drop` ends the resource.

`PsramBuffer` allocates in `new` and frees in `drop`. `Transport` in `c6/mod.rs` brings the link to the C6, the radio chip, up in `up` and calls `esp_hosted_deinit` in `drop`; its only field is `()`, so holding one is nothing but proof that the link is up. `Meter` in `ambient.rs` deletes its camera handles in `drop`. `Display` owns the panel's handles and has no `Drop`, but its public methods are all safe.

This is also the chapter's answer to how the parts work together and where the state lives. The C drivers and the registers hold state that Rust cannot see. Each piece of it has one Rust owner, and that owner's module is the only place where the rules are kept by hand. [Chapter 20](@/wakey/20-errors.md) turns to errors.
