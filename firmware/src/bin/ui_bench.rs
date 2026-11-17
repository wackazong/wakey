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
use wakey::ui::list_layer::{Layout, ListLayer, ListSource, SwipeEnd, SwipePart};
use wakey::ui::{input, platform, render_loop, rgb565, stats};

include!(concat!(env!("OUT_DIR"), "/ui_bench.rs"));

const ROWS: usize = 300;
const ROW_H: usize = 96;

/// The bench's rows: generated text and colours, never changing.
struct Tracks {
    row: TrackRow,
}

impl ListSource for Tracks {
    fn shown(&self) -> bool {
        true
    }
    fn rows(&self) -> usize {
        ROWS
    }
    fn stamp(&self, _: usize) -> u64 {
        0
    }
    fn prepare(&mut self, i: usize) {
        const ARTISTS: [&str; 5] = [
            "Ada Lovelace",
            "Café Tacvba",
            "Björk",
            "Sigur Rós",
            "Mötley Crüe",
        ];
        self.row.set_index(i as i32);
        self.row.set_track_title(SharedString::from(format!(
            "Track {:03} – Morgenstund",
            i + 1
        )));
        self.row
            .set_artist(SharedString::from(ARTISTS[i % ARTISTS.len()]));
        self.row.set_tint(Color::from_rgb_u8(
            (i * 37 % 256) as u8,
            (i * 91 % 256) as u8,
            (i * 53 % 256) as u8,
        ));
    }
    fn offset(&self) -> f32 {
        0.0
    }
    fn prepare_swipe(&mut self, _: usize, _: SwipePart) {}
    fn layer_started(&mut self) {}
    fn scroll_ended(&mut self, _: f32) {}
    fn swipe_ended(&mut self, _: usize, _: SwipeEnd) {}
}

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app().unwrap()))
}

#[embassy_executor::task]
async fn app() {
    let Board {
        display,
        mut backlight,
        i2c,
        ioexp,
        ..
    } = Board::init().expect("board init");
    input::start(i2c, ioexp);
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
    let layout = Layout {
        x: 640,
        y: 0,
        width: 640,
        height: 720,
        row_h: ROW_H,
        slots: 64,
        slop: 12.0,
        // The list area's background in `ui_bench.slint`.
        background: rgb565(Color::from_rgb_u8(0x14, 0x1a, 0x20)),
        page_draws_still: false,
        // The layer has every touch: it only scrolls.
        swipe: false,
        row_radius: 0,
        corners: None,
    };
    let mut list = ListLayer::new(layout, row_window, Tracks { row });
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

    backlight.set_percent(100).expect("backlight");
    render_loop::run(window, display, &mut rot, Some(&mut list)).await
}
