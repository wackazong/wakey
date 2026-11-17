+++
title = "Sharing hardware: one bus, many chips"
description = "What cannot have a single owner: the I2C bus, the IO expanders, single switches, the modem and buffers in PSRAM."
weight = 7

[extra]
map = ["board", "psram"]
map_note = "We stay in `board`, in the hardware layer, and end in `psram`, one of the two support modules."
goals = [
  "how seven chips and several threads use one I2C bus through `Arc<Mutex<..>>`",
  "why the IO expanders keep a copy of their registers, and which lock protects it",
  "how a boxed closure gives a service one switch and nothing else",
  "what the two `unsafe` reborrows in this chapter promise",
]
takeaways = [
  "There is one I2C driver. Whoever needs the bus holds a clone of `Arc<Mutex<SysI2c>>` and locks it for one transfer or one short sequence.",
  "All clones of `IoExpanders` share the bus and one cached image of the chips' registers. The image's lock is held until the changed byte is on the chip.",
  "A `Switch` is a boxed closure. Its receiver can turn one supply on and off and cannot see the expander behind it.",
  "Each Wi-Fi session reborrows the one modem, and `unsafe` marks the promise that the previous session's driver is gone.",
]
+++

[Chapter 6](@/wakey/06-board.md) followed hardware that has exactly one owner. This chapter is about hardware where that does not work, because several parts of the firmware need the same thing.

There are four such cases, each solved differently: the I2C bus, the IO expanders, a single pin of an expander, and the modem. We end with a buffer that shares memory with the hardware itself.

## One driver for the whole bus

Most small chips on the board hang on the same two processor pins, GPIO31 and GPIO32.

{% <concept title="I2C in short"> %}
I2C is a bus made of two wires, a data line called SDA and a clock line called SCL. Every chip on the bus has an address of seven bits. The processor starts each transfer by sending the address of the chip it wants, and only that chip answers. Only one transfer can be on the wires at a time.

Here seven chips share the wires, and the code that talks to them runs on several threads.
{% </concept> %}

The firmware installs one driver for these two pins. Here are the type that wraps it and the function everybody reaches it through.

{% <code file="firmware/src/board/i2c.rs" lines="17-27" hl="18 26"> %}{% raw %}
/// The bus, shared between chip drivers and tasks.
pub type SharedI2c = Arc<Mutex<SysI2c>>;

pub struct SysI2c {
    driver: I2cDriver<'static>,
}

/// Locks a [`SharedI2c`] (a poisoned lock is still a usable bus).
pub fn lock(bus: &SharedI2c) -> MutexGuard<'_, SysI2c> {
    bus.lock().unwrap_or_else(|e| e.into_inner())
}
{% endraw %}{% </code> %}

`SysI2c` wraps `I2cDriver`, the bus driver from the crate `esp-idf-hal`. Line 18 names the shared form of it. The `Mutex` makes sure that only one thread uses the driver at a time, and the `Arc` lets many parts of the program hold the same mutex. `Board::init` creates this value once. Everybody else receives a clone, and a clone of an `Arc` is a second reference to the same driver, not a second driver.

{{ <svg src="img/i2c-bus.svg" caption="The one bus driver in the middle. On the left is everything that holds a clone of the `Arc`, on the right the chip it talks to, with the address from the code." /> }}

The function `lock` exists because of line 26. `Mutex::lock` returns an `Err` when another thread panicked while it held the lock. Rust calls such a mutex *poisoned*. The usual reaction is `unwrap`, which passes the panic on. This code does the opposite. The error value still contains the guard, and `into_inner` takes it out, so the caller gets the bus in both cases. The doc comment gives the reasoning: a poisoned lock is still a usable bus. [Chapter 20](@/wakey/20-errors.md) comes back to this pattern.

## A lock for one transfer

The module comment says that holders "lock it per transaction". The shortest example is the INA226, the chip that measures the battery current. Look at the highlighted line.

{% <code file="firmware/src/board/ina226.rs" lines="13-17" hl="15"> %}{% raw %}
fn current_ma(bus: &SharedI2c) -> Option<f32> {
    let mut b = [0u8; 2];
    lock(bus).write_read(ADDR, &[SHUNT_VOLTAGE], &mut b).ok()?;
    Some(f32::from(i16::from_be_bytes(b)) * 0.5)
}
{% endraw %}{% </code> %}

On line 15, `lock(bus)` takes the bus, `write_read` sends the register number and reads two bytes back, and at the end of the statement the bus is free again. Nothing in the line says "unlock".

{% <tip title="A guard without a name lives for one statement" id="temporary-guard"> %}
`lock(bus)` returns a `MutexGuard`, and the mutex stays locked for as long as that guard exists. Line 15 never binds the guard to a variable. That makes it a temporary value, and Rust drops a temporary at the end of the statement that created it. So the lock is held for exactly one transfer.

When several transfers belong together, the code gives the guard a name. `codec::init` binds it with `let` and then sends a whole table of register values, and no other thread gets onto the bus in between.
{% </tip> %}

One thing happens before the driver exists. The comment on `recover` explains that a reset in the middle of a read can leave a chip holding the data line low. So `SysI2c::new` first calls `recover`, which drives the two pins by hand until the bus is free.

{% <unsafe title="Borrowing two pins before the driver takes them" id="unsafe-i2c-recover"> %}
`SysI2c::new` owns the two pins and must give them to `I2cDriver::new` afterwards. For the recovery it first calls `reborrow()` on each, which creates a second handle to the same pin. `reborrow` is an `unsafe` function.

The `SAFETY` comment claims one thing: the pin drivers made from the reborrowed handles are dropped before the I2C driver takes the pins. They are local variables of `recover`, and `recover` has returned by then. If the claim were false, two drivers would configure the same two pins at once.
{% </unsafe> %}

## The IO expanders: a handle that remembers

Two chips on the bus are *IO expanders*. An IO expander gives the processor eight more pins, which the firmware sets by writing a register over I2C. These pins switch power supplies and reset lines, for example the 3.3 V of the Wi-Fi chip and the 5 V of the USB port. Several services need one, so the expanders are shared as well. The next excerpt shows two types. We start with the second.

{% <code file="firmware/src/board/ioexp.rs" lines="72-91" hl="72 89-90"> %}{% raw %}
pub type Switch = Box<dyn Fn(bool) -> Result<(), EspError> + Send>;
// ... @87
#[derive(Clone)]
pub struct IoExpanders {
    bus: SharedI2c,
    images: Arc<Mutex<(Image, Image)>>,
}
{% endraw %}{% </code> %}

`IoExpanders` has two fields. `bus` is a clone of the shared bus. `images` is a pair of `Image` values, one per chip, behind a second `Arc<Mutex<..>>`. An `Image` is the firmware's copy of one chip's registers. The comment on `Image` gives the reason for the copy: the firmware is the only writer, so the copy stays in sync with the chip, and every change is a single register write.

`#[derive(Clone)]` on line 87 makes the handle cheap to copy. A clone copies two `Arc`s, so all clones share one bus and one pair of images. Next we see how a pin is set through such a handle.

{% <code file="firmware/src/board/ioexp.rs" lines="134-171" hl="136 142 169-170"> %}{% raw %}
    /// Runs `f` with the bus and the register image of expander `addr`.
    fn with<R>(&self, addr: u8, f: impl FnOnce(&mut SysI2c, u8, &mut Image) -> R) -> R {
        let mut images = self.images.lock().unwrap_or_else(|e| e.into_inner());
        let img = if addr == IOE_A {
            &mut images.0
        } else {
            &mut images.1
        };
        f(&mut i2c::lock(&self.bus), addr, img)
    }
    // ... @154
    /// Drives an output pin.
    pub fn set(&self, pin: Pin, level: bool) -> Result<(), EspError> {
        let (addr, bit) = pin.location();
        self.with(addr, |bus, addr, img| {
            if level {
                img.out |= 1 << bit;
            } else {
                img.out &= !(1 << bit);
            }
            bus.write_reg(addr, reg::OUTPUT, img.out)
        })
    }

    /// A [`Switch`] driving `pin`.
    pub fn switch(&self, pin: Pin) -> Switch {
        let this = self.clone();
        Box::new(move |on| this.set(pin, on))
    }
{% endraw %}{% </code> %}

Start with `set` on line 155. It asks the pin for its chip's address and its bit number. Then it calls `with` and passes a closure. The closure changes one bit in the cached output byte and writes the whole byte to the chip.

`with`, on line 135, does the locking. Line 136 locks the images, and the lines after it pick the image of the right chip. Line 142 then locks the bus and calls the closure with both.

{% <pitfall title="Change the copy and the chip under one lock" id="cached-register"> %}
An output register holds eight pins in one byte. Imagine two threads that each change a different pin of the same chip. If each took the byte, changed its bit and wrote it back with no lock around all three steps, the later write could undo the earlier one.

`with` prevents this. The guard `images` from line 136 lives until the function returns, so it is still held while the byte is written. A second thread waits there and then starts from the new byte.
{% </pitfall> %}

## A switch is a closure

Now back to `Switch`, on line 72 of the earlier excerpt. It is a boxed closure that takes `true` or `false` and may fail with an `EspError`. The method `switch`, on lines 168 to 171, makes one: it clones the handle and moves the clone and the pin into a closure.

Two services get a `Switch`: Wi-Fi the one for `Pin::WlanPwrEn`, the USB speaker the one for `Pin::Usb5vEn`. Each turns its own supply on and off by calling the closure, as in `(self.power)(true)`. Neither can reach the other pins or the bus, because the type gives them no way to name these. The `+ Send` lets the closure move to the service's thread. [Chapter 2](@/wakey/02-layers.md) gave the rule behind this: a service gets what it needs as an argument.

## The modem: shared between sessions

The modem is the handle that stands for the radio, and only the `wifi` thread owns it. The sharing happens inside that thread, over time. Each Wi-Fi session builds a new driver with `EspWifi::new`, which takes a modem by value. The thread's loop needs the modem again for the next session.

{% <code file="firmware/src/net.rs" lines="350-359" hl="353-355"> %}{% raw %}
    fn session(&self, modem: &mut Modem<'static>, job: JobGuard) -> anyhow::Result<Outcome> {
        let t = Instant::now();
        let transport = self.c6.connect()?;
        // SAFETY: the reborrowed modem lives only as long as this session's
        // driver, which is dropped before the next session reborrows it.
        let modem = unsafe { modem.reborrow() };
        let wifi = BlockingWifi::wrap(
            EspWifi::new(modem, self.sysloop.clone(), Some(self.nvs.clone()))?,
            self.sysloop.clone(),
        )?;
{% endraw %}{% </code> %}

`session` receives `&mut Modem` from the loop. Line 355 turns that reference into a modem value with a shorter lifetime, and line 357 gives it to the driver. Further down, the driver is moved into the code that does the session's work and is dropped there.

{% <unsafe title="One modem, one driver at a time" id="unsafe-modem-reborrow"> %}
`modem.reborrow()` creates a second handle to the same modem, as the pin reborrow did.

The `SAFETY` comment claims that the reborrowed modem lives only as long as this session's driver, and that this driver is dropped before the next session reborrows. If an earlier driver were still alive, two Wi-Fi drivers would use one radio. [Chapter 19](@/wakey/19-unsafe.md) lists this site with the other reborrows.
{% </unsafe> %}

## Memory shared with the hardware

PSRAM is the board's 32 MB of external memory. The firmware puts large buffers there, such as the camera frame. Some are filled by DMA, which is hardware that copies data on its own.

{% <code file="firmware/src/psram.rs" lines="12-41" hl="36-41"> %}{% raw %}
pub struct PsramBuffer {
    ptr: *mut u8,
}
// ... @36
impl Drop for PsramBuffer {
    fn drop(&mut self) {
        // SAFETY: allocated by `heap_caps_aligned_alloc`, freed once.
        unsafe { sys::heap_caps_free(self.ptr.cast()) };
    }
}
{% endraw %}{% </code> %}

`PsramBuffer` holds one raw pointer. It has no method that returns a slice, only `as_mut_ptr`, and its doc comment says why: the memory is shared with DMA engines and other threads, so the owner decides who touches what. The buffer itself guarantees one thing, in lines 36 to 41: when the value is dropped, the memory is freed, once.

For our two questions: the parts work together on shared hardware through small handles that are cheap to clone and carry their own lock. The state of that hardware lives in one place per thing: one driver for the bus, one register image for each expander, one modem. The [next chapter](@/wakey/08-channels.md) turns to messages between threads.
