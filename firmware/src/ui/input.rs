//! Touch input as landscape pointer events.
//!
//! A thread ([`start`]) polls the touch controller every 10 ms (the touch
//! interrupt pin is not used yet) and turns readings into Down/Move/Up
//! events in UI (landscape) coordinates, timestamped when read and queued
//! in [`EVENTS`]. The render loop drains the queue once per frame.
//!
//! Its own thread, not a task of the UI's executor: there a frame that
//! took long (a page change, the clock's minute) delayed the next poll, up
//! to 90 ms, and a fling measured from so few points had no speed.

use crate::board::i2c::{self, SharedI2c};
use crate::board::ioexp::{IoExpanders, Pin};
use crate::board::touch;
use crate::display;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant};
use esp_idf_svc::hal::cpu::Core;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

/// A pointer event in landscape UI coordinates (x 0..1280, y 0..720).
#[derive(Clone, Copy, Debug)]
pub struct TouchEvent {
    pub phase: Phase,
    pub x: f32,
    pub y: f32,
    pub at: Instant,
}

/// Empty readings in a row (10 ms apart) that count as a release.
const RELEASE_POLLS: u8 = 3;

/// Failed reads within [`ERROR_WINDOW`] that reset the touch controller.
/// After a burst of I2C timeouts and NACKs while being touched, the ST7123
/// answered again but never reported a touch until it was reset.
const RESET_AFTER_ERRORS: u32 = 5;
const ERROR_WINDOW: Duration = Duration::from_secs(1);

/// True while the touch controller is being read (for the stall report).
pub static READING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// When the last touch event was queued (ms since boot), for
/// [`idle`].
static LAST_EVENT_MS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Time since the last touch.
pub fn idle() -> Duration {
    let last = LAST_EVENT_MS.load(core::sync::atomic::Ordering::Relaxed);
    Duration::from_millis(u64::from(
        (Instant::now().as_millis() as u32).wrapping_sub(last),
    ))
}

/// Starts [`idle`] from now, as a touch would: the display woke by itself
/// (daylight, an alarm), and its sleep timer counts from then.
pub fn restart_idle() {
    touched();
}

fn touched() {
    LAST_EVENT_MS.store(
        Instant::now().as_millis() as u32,
        core::sync::atomic::Ordering::Relaxed,
    );
}

pub static EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, 32> = Channel::new();

/// Touch panel coordinates → landscape UI coordinates. The touch
/// controller's origin is rotated 180° against the display memory (whose
/// mapping is in the [`crate::ui`] docs): landscape x = touch y, landscape
/// y = WIDTH-1 - touch x. Checked on the device (drag direction, list area).
fn to_landscape(px: u16, py: u16) -> (f32, f32) {
    let x = (py as usize).min(display::HEIGHT - 1) as f32;
    let y = (display::WIDTH - 1).saturating_sub(px as usize) as f32;
    (x, y)
}

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

/// Pulses the touch controller's reset line and waits until it answers.
fn reset_controller(bus: &SharedI2c, ioexp: &IoExpanders) {
    let pulse = ioexp.set(Pin::TpRst, false).and_then(|()| {
        esp_idf_svc::hal::delay::FreeRtos::delay_ms(10);
        ioexp.set(Pin::TpRst, true)
    });
    if let Err(e) = pulse {
        log::error!("touch: reset failed ({e})");
        return;
    }
    // The controller needs a moment after reset (see `board::Board::init`).
    std::thread::sleep(std::time::Duration::from_millis(300));
    let fw = touch::firmware(&mut i2c::lock(bus));
    log::warn!("touch: controller reset, firmware {fw:?}");
}

/// Starts the touch thread: on core 1 above the audio decoder (5), which
/// could otherwise hold it off while it decodes; a poll takes well under
/// a millisecond.
pub fn start(bus: SharedI2c, ioexp: IoExpanders) {
    if let Err(e) = crate::runtime::Thread::new(c"touch", 6 * 1024)
        .core(Core::Core1)
        .priority(6)
        .spawn(move || poll(bus, ioexp))
    {
        log::error!("touch: thread not started: {e}");
    }
}

/// Polls the touch controller forever (shares the system I2C bus), and
/// resets it after a burst of failed reads.
fn poll(bus: SharedI2c, ioexp: IoExpanders) {
    let mut down = false;
    let mut last = (0.0, 0.0);
    let mut errors = 0u32;
    // Failed reads since `window_start`.
    let mut recent = 0u32;
    let mut window_start = Instant::now();
    // Consecutive readings without a touch. The controller sometimes
    // reports nothing (or times out) for one poll during a drag; only a
    // few empty readings in a row count as lifting the finger.
    let mut empty = 0u8;
    loop {
        READING.store(true, core::sync::atomic::Ordering::Relaxed);
        let read = touch::read_point(&mut i2c::lock(&bus));
        READING.store(false, core::sync::atomic::Ordering::Relaxed);
        match read {
            Ok(Some((px, py))) => {
                empty = 0;
                touched();
                let (x, y) = to_landscape(px, py);
                let phase = if down { Phase::Move } else { Phase::Down };
                if phase == Phase::Down || (x, y) != last {
                    let _ = EVENTS.try_send(TouchEvent {
                        phase,
                        x,
                        y,
                        at: Instant::now(),
                    });
                }
                down = true;
                last = (x, y);
            }
            Ok(None) => {
                empty = empty.saturating_add(1);
                if empty >= RELEASE_POLLS {
                    release(&mut down, last);
                }
            }
            Err(e) => {
                errors += 1;
                if errors.is_power_of_two() {
                    log::warn!("touch: read failed ({e}), {errors} errors so far");
                }
                if window_start.elapsed() > ERROR_WINDOW {
                    window_start = Instant::now();
                    recent = 0;
                }
                recent += 1;
                if recent >= RESET_AFTER_ERRORS {
                    // Nothing pressed stays pressed (a held step button
                    // would keep repeating).
                    release(&mut down, last);
                    empty = 0;
                    reset_controller(&bus, &ioexp);
                    recent = 0;
                    window_start = Instant::now();
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
