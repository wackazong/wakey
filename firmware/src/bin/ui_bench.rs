//! UI bench (`ui/ui_bench.slint`): clock digits, a continuous animation
//! and a 300-row list scrolled by touch (drag, fling, overscroll). The list
//! is composed by the list layer (rows pre-rendered into a ring, scrolled
//! by 2D-DMA), everything else by Slint. Frame statistics are logged once
//! per second and shown on screen. Target: 66 fps while scrolling.
//!
//! `cargo run --release --bin ui_bench`

use embassy_time::Instant;
use slint::{Color, SharedString, Timer, TimerMode};
use wakey::board::Board;
use wakey::display::ppa::PpaRotator;
use wakey::ui::list_layer::ListLayer;
use wakey::ui::{platform, render_loop, stats};

include!(concat!(env!("OUT_DIR"), "/ui_bench.rs"));

/// Inject a synthetic swipe through the touch event queue.
const SELF_TEST_SWIPE: bool = false;

#[embassy_executor::task]
async fn swipe() {
    use embassy_time::{Duration, Timer};
    use wakey::ui::input::{EVENTS, Phase, TouchEvent};
    let ev = |phase, y: f32| TouchEvent {
        phase,
        x: 960.0,
        y,
        at: Instant::now(),
    };
    Timer::after(Duration::from_secs(4)).await;
    // 1. Pull 400 px past the top, hold, release: rubber band, spring back.
    log::info!("self-test: pull down at the top");
    EVENTS.send(ev(Phase::Down, 100.0)).await;
    for k in 1..=20 {
        Timer::after(Duration::from_millis(10)).await;
        EVENTS.send(ev(Phase::Move, 100.0 + 20.0 * k as f32)).await;
    }
    Timer::after(Duration::from_millis(150)).await;
    EVENTS.send(ev(Phase::Up, 500.0)).await;
    Timer::after(Duration::from_secs(2)).await;
    // 2. Hard fling up from the top.
    log::info!("self-test: fling");
    EVENTS.send(ev(Phase::Down, 600.0)).await;
    for k in 1..=5 {
        Timer::after(Duration::from_millis(10)).await;
        EVENTS.send(ev(Phase::Move, 600.0 - 60.0 * k as f32)).await;
    }
    EVENTS.send(ev(Phase::Up, 300.0)).await;
}

/// Run the sampling profiler for 8 s and log the hottest addresses.
const PROFILE: bool = false;

const ROWS: usize = 300;
const ROW_H: usize = 96;

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app(spawner).unwrap()))
}

#[embassy_executor::task]
async fn app(spawner: embassy_executor::Spawner) {
    let Board {
        display,
        mut backlight,
        i2c,
        ioexp,
        ..
    } = Board::init().expect("board init");
    spawner.spawn(wakey::ui::input::touch_task(i2c, ioexp).unwrap());
    let variant = display.variant();
    let mut rot = PpaRotator::new().expect("PPA");

    let window = platform::init();
    let ui = UiBench::new().expect("Slint UI");
    ui.show().expect("show");

    // The list: one row component in an offscreen window, re-rendered for
    // each row that scrolls into view.
    let row_window = platform::offscreen_window(640, ROW_H as u32);
    let row = TrackRow::new().expect("Slint row");
    row.show().expect("show row");
    let artists = [
        "Ada Lovelace",
        "Café Tacvba",
        "Björk",
        "Sigur Rós",
        "Mötley Crüe",
    ];
    let set_row = move |i: usize| {
        row.set_index(i as i32);
        row.set_track_title(SharedString::from(format!(
            "Track {:03} – Morgenstund",
            i + 1
        )));
        row.set_artist(SharedString::from(artists[i % artists.len()]));
        row.set_tint(Color::from_rgb_u8(
            (i * 37 % 256) as u8,
            (i * 91 % 256) as u8,
            (i * 53 % 256) as u8,
        ));
    };
    // #141a20, the list area's background in the .slint file, as RGB565.
    const LIST_BACKGROUND: u16 = (0x14 >> 3) << 11 | (0x1a >> 2) << 5 | (0x20 >> 3);
    let mut list = ListLayer::new(640, 640, ROW_H, ROWS, row_window, set_row, LIST_BACKGROUND);
    list.prerender(&mut rot);

    // Once per second: uptime as the clock and the frame stats.
    let weak = ui.as_weak();
    let clock = Timer::default();
    clock.start(
        TimerMode::Repeated,
        core::time::Duration::from_millis(1000),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let s = Instant::now().as_secs();
            // Set only what changed, so unchanged fields stay clean.
            let set = |cur: SharedString, new: u64, f: &dyn Fn(SharedString)| {
                let new = SharedString::from(format!("{new:02}"));
                if cur != new {
                    f(new);
                }
            };
            set(ui.get_hours(), s / 3600 % 24, &|v| ui.set_hours(v));
            set(ui.get_minutes(), s / 60 % 60, &|v| ui.set_minutes(v));
            set(ui.get_seconds(), s % 60, &|v| ui.set_seconds(v));
            let (fps, render_ms) = stats::last();
            ui.set_stats(SharedString::from(format!(
                "{fps:.1} fps · render {render_ms:.1} ms · {variant:?}"
            )));
        },
    );

    // Sampling profiler: resolve the log with `scripts/profile.sh`.
    let profile_end = Timer::default();
    if PROFILE {
        wakey::profiler::start();
        profile_end.start(
            TimerMode::SingleShot,
            core::time::Duration::from_secs(11),
            || {
                wakey::profiler::report(400);
            },
        );
    }

    // Self-test: a synthetic swipe up in the list 4 s after start.
    if SELF_TEST_SWIPE {
        spawner.spawn(swipe().unwrap());
    }

    backlight.set_percent(100).expect("backlight");
    render_loop::run(window, display, &mut rot, Some(&mut list)).await
}
