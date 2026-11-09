//! Raw display test: no UI toolkit. Proves the panel, the PSRAM frame
//! buffers and tear-free flipping, and measures the cost of a full-screen
//! CPU fill. A second Embassy task logs a heartbeat to show the executor.
//!
//! `cargo run --release --bin display_test`

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use wakey::board::Board;
use wakey::display::{self, Display};
use wakey::ui::{UI_HEIGHT, UI_WIDTH, canvas::Canvas, stats::FrameStats};

fn main() -> ! {
    wakey::runtime::start(|spawner| {
        spawner.spawn(heartbeat().unwrap());
        spawner.spawn(app().unwrap());
    })
}

#[embassy_executor::task]
async fn heartbeat() {
    loop {
        Timer::after(Duration::from_secs(5)).await;
        log::info!("heartbeat: uptime {} s", Instant::now().as_secs());
    }
}

#[embassy_executor::task]
async fn app() {
    let Board {
        mut display,
        mut backlight,
        ..
    } = Board::init().expect("board init");
    backlight.set_percent(100).expect("backlight");
    render(&mut display).await
}

async fn render(display: &mut Display) -> ! {
    let mut stats = FrameStats::new("display_test");
    let mut hue: u32 = 0;
    let mut x: i32 = 0;
    loop {
        let buf = display.back_buffer().await;
        let t0 = Instant::now();
        let mut canvas = Canvas::new(buf);
        // Full-screen background (the worst case: every pixel, every frame).
        let _ = canvas.clear(hue_to_rgb565(hue));
        // A white bar sweeping left to right shows tearing if there is any.
        let _ = Rectangle::new(Point::new(x, 0), Size::new(40, UI_HEIGHT))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
            .draw(&mut canvas);
        let render = t0.elapsed();
        display.present(0..display::HEIGHT).expect("present");
        stats.frame(render, display::HEIGHT);

        hue = (hue + 1) % 360;
        x = (x + 8) % (UI_WIDTH as i32 - 40);
    }
}

fn hue_to_rgb565(hue: u32) -> Rgb565 {
    let f = (hue % 60) * 255 / 60;
    let (r, g, b) = match hue / 60 {
        0 => (255, f, 0),
        1 => (255 - f, 255, 0),
        2 => (0, 255, f),
        3 => (0, 255 - f, 255),
        4 => (f, 0, 255),
        _ => (255, 0, 255 - f),
    };
    Rgb565::new((r >> 3) as u8, (g >> 2) as u8, (b >> 3) as u8)
}
