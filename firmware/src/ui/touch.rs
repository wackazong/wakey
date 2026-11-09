//! Touch input as landscape pointer events.
//!
//! [`touch_task`] polls the touch controller every 10 ms (interrupt-driven
//! waiting comes with the sleep work) and turns readings into
//! Down/Move/Up events in UI (landscape) coordinates, queued in [`EVENTS`].
//! The render loop drains the queue once per frame.

use crate::board::i2c::SysI2c;
use crate::board::touch;
use crate::display;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};

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

pub static EVENTS: Channel<CriticalSectionRawMutex, TouchEvent, 32> = Channel::new();

/// Touch panel coordinates → landscape UI coordinates. The touch
/// controller's origin is rotated 180° against the display memory (whose
/// mapping is in the `ui` module docs): landscape x = touch y, landscape
/// y = WIDTH-1 - touch x. Checked on the device (drag direction, list area).
fn to_landscape(px: u16, py: u16) -> (f32, f32) {
    let x = (py as usize).min(display::HEIGHT - 1) as f32;
    let y = (display::WIDTH - 1).saturating_sub(px as usize) as f32;
    (x, y)
}

/// Polls the touch controller forever. Owns the system I2C bus.
#[embassy_executor::task]
pub async fn touch_task(mut i2c: SysI2c) {
    let mut down = false;
    let mut last = (0.0, 0.0);
    let mut errors = 0u32;
    // Consecutive readings without a touch. The controller sometimes
    // reports nothing (or times out) for one poll during a drag; only a
    // few empty readings in a row count as lifting the finger.
    let mut empty = 0u8;
    loop {
        match touch::read_point(&mut i2c) {
            Ok(Some((px, py))) => {
                empty = 0;
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
                if down && empty >= RELEASE_POLLS {
                    let _ = EVENTS.try_send(TouchEvent {
                        phase: Phase::Up,
                        x: last.0,
                        y: last.1,
                        at: Instant::now(),
                    });
                    down = false;
                }
            }
            Err(e) => {
                errors += 1;
                if errors.is_power_of_two() {
                    log::warn!("touch: read failed ({e}), {errors} errors so far");
                }
            }
        }
        Timer::after(Duration::from_millis(10)).await;
    }
}
