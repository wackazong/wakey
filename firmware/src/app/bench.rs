//! Scripted touch gestures for measuring the UI on the device
//! (`WAKEY_UI_BENCH=1` at build time): ten seconds after boot it opens the
//! alarm list from the clock page, flings it, presses, swipes and closes a
//! row, toggles a switch twice and goes back, logging every frame
//! (`render_loop::TRACE`). The gestures go through the touch queue like
//! real ones. Toggling saves the alarms (the state ends as it began).

use crate::ui::input::{EVENTS, Phase, TouchEvent};
use crate::ui::render_loop;
use core::sync::atomic::Ordering::Relaxed;
use embassy_time::{Instant, Timer};

async fn send(phase: Phase, x: f32, y: f32) {
    EVENTS
        .send(TouchEvent {
            phase,
            x,
            y,
            at: Instant::now(),
        })
        .await;
}

async fn tap(x: f32, y: f32) {
    send(Phase::Down, x, y).await;
    Timer::after_millis(60).await;
    send(Phase::Up, x, y).await;
}

/// A drag from `from` by (`dx`, `dy`) in `steps` touch polls (10 ms).
async fn drag(from: (f32, f32), dx: f32, dy: f32, steps: u32) {
    send(Phase::Down, from.0, from.1).await;
    for i in 1..=steps {
        Timer::after_millis(10).await;
        let f = i as f32 / steps as f32;
        send(Phase::Move, from.0 + dx * f, from.1 + dy * f).await;
    }
    Timer::after_millis(10).await;
    send(Phase::Up, from.0 + dx, from.1 + dy).await;
}

async fn step(name: &str, wait_ms: u64) {
    Timer::after_millis(wait_ms).await;
    log::info!("bench: {name}");
}

#[embassy_executor::task]
pub async fn run() {
    Timer::after_secs(10).await;
    render_loop::TRACE.store(true, Relaxed);
    step("open list", 0).await;
    tap(290.0, 658.0).await;
    step("fling up", 1500).await;
    drag((640.0, 600.0), 0.0, -300.0, 15).await;
    step("fling down", 3000).await;
    drag((640.0, 300.0), 0.0, 300.0, 15).await;
    step("press", 3000).await;
    send(Phase::Down, 500.0, 210.0).await;
    Timer::after_millis(400).await;
    send(Phase::Move, 500.0, 240.0).await;
    Timer::after_millis(100).await;
    send(Phase::Up, 500.0, 240.0).await;
    step("swipe row", 1500).await;
    drag((700.0, 210.0), -300.0, 0.0, 20).await;
    step("close row", 1500).await;
    tap(700.0, 500.0).await;
    step("toggle switch", 1500).await;
    tap(1150.0, 210.0).await;
    step("toggle back", 1500).await;
    tap(1150.0, 210.0).await;
    step("back to clock", 1500).await;
    tap(140.0, 64.0).await;
    step("done", 1500).await;
    render_loop::TRACE.store(false, Relaxed);
}
