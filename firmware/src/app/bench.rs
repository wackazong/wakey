//! Scripted touch gestures for measuring the UI on the device (feature
//! `ui-bench`). Ten seconds after boot it opens the alarm list from the
//! clock page, holds a row (the first press must repaint only that row,
//! twice: no slide), flings the list, stops a fling with a tap (must not
//! open the alarm), flings back, presses, swipes and closes a row,
//! toggles a switch twice (saved; the state ends as it began), turns the
//! minute wheel of an alarm (clicks) and goes back, with the per-frame trace (`ui::trace`) on. The gestures go through
//! the touch queue like real ones. Each step is logged as `bench: <step>`;
//! the frames after it belong to that step.
//!
//! The positions follow the layouts in `ui/wakey/clock_page.slint` and
//! `ui/wakey/alarm_list.slint`; the list needs at least two alarms.

use crate::ui::input::{EVENTS, Phase, TouchEvent};
use crate::ui::trace;
use core::sync::atomic::Ordering::Relaxed;
use embassy_time::{Instant, Timer};

/// The alarm line on the clock page (opens the list).
const CLOCK_ALARM_LINE: (f32, f32) = (290.0, 658.0);
/// "‹ Uhr" on the list page.
const LIST_BACK: (f32, f32) = (140.0, 64.0);
/// The middle of the first row's text, and its switch.
const FIRST_ROW: (f32, f32) = (600.0, 210.0);
const FIRST_SWITCH: (f32, f32) = (1150.0, 210.0);
/// A point in the lower part of the list.
const LOWER_LIST: (f32, f32) = (640.0, 600.0);

async fn send(phase: Phase, (x, y): (f32, f32)) {
    let at = Instant::now();
    EVENTS.send(TouchEvent { phase, x, y, at }).await;
}

async fn tap(at: (f32, f32)) {
    send(Phase::Down, at).await;
    Timer::after_millis(60).await;
    send(Phase::Up, at).await;
}

/// A drag from `from` by (`dx`, `dy`) in `steps` touch polls of 10 ms.
async fn drag(from: (f32, f32), (dx, dy): (f32, f32), steps: u32) {
    send(Phase::Down, from).await;
    for i in 1..=steps {
        Timer::after_millis(10).await;
        let f = i as f32 / steps as f32;
        send(Phase::Move, (from.0 + dx * f, from.1 + dy * f)).await;
    }
    Timer::after_millis(10).await;
    send(Phase::Up, (from.0 + dx, from.1 + dy)).await;
}

/// Waits `wait_ms` (the previous step settles), then logs the next step.
async fn step(name: &str, wait_ms: u64) {
    Timer::after_millis(wait_ms).await;
    log::info!("bench: {name}");
}

#[embassy_executor::task]
pub async fn run() {
    Timer::after_secs(10).await;
    trace::ENABLED.store(true, Relaxed);

    step("open list", 0).await;
    tap(CLOCK_ALARM_LINE).await;
    // Held, then moved: lights up, then scrolls (as "press" below, but the
    // first touch on the fresh page).
    step("first press", 1500).await;
    send(Phase::Down, FIRST_ROW).await;
    Timer::after_millis(400).await;
    send(Phase::Move, (FIRST_ROW.0, FIRST_ROW.1 + 30.0)).await;
    Timer::after_millis(100).await;
    send(Phase::Up, (FIRST_ROW.0, FIRST_ROW.1 + 30.0)).await;
    step("fling up", 1500).await;
    drag(LOWER_LIST, (0.0, -300.0), 15).await;
    // A tap while it still moves only stops it.
    step("catch fling", 300).await;
    tap(FIRST_ROW).await;
    step("fling down", 2000).await;
    drag((LOWER_LIST.0, LOWER_LIST.1 - 300.0), (0.0, 300.0), 15).await;
    // Held, then moved a little: the row lights up, and the move turns the
    // touch into a scroll, so lifting does not open the alarm.
    step("press", 3000).await;
    send(Phase::Down, FIRST_ROW).await;
    Timer::after_millis(400).await;
    send(Phase::Move, (FIRST_ROW.0, FIRST_ROW.1 + 30.0)).await;
    Timer::after_millis(100).await;
    send(Phase::Up, (FIRST_ROW.0, FIRST_ROW.1 + 30.0)).await;
    step("swipe row", 1500).await;
    drag(FIRST_ROW, (-300.0, 0.0), 20).await;
    // A touch elsewhere closes the open row.
    step("close row", 1500).await;
    tap(LOWER_LIST).await;
    step("toggle switch", 1500).await;
    tap(FIRST_SWITCH).await;
    step("toggle back", 1500).await;
    tap(FIRST_SWITCH).await;
    // Opens an alarm and turns the minute wheel: the log shows
    // `click: amplifier on` (the clicks reach the speaker's driver).
    step("wheel", 1500).await;
    tap(FIRST_ROW).await;
    Timer::after_millis(1000).await;
    drag((420.0, 450.0), (0.0, -200.0), 20).await;
    Timer::after_millis(1500).await;
    tap((140.0, 64.0)).await;
    step("back to clock", 1500).await;
    tap(LIST_BACK).await;

    step("done", 1500).await;
    trace::ENABLED.store(false, Relaxed);
}
