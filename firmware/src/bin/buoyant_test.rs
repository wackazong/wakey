//! Buoyant GUI test: clock digits, an animated toggle, a sliding bar and a
//! list of rows that shift every second. Buoyant redraws the full frame
//! whenever something changes or animates, so this measures its worst case
//! next to Slint's partial rendering.
//!
//! `cargo run --release --bin buoyant_test`

use buoyant::app::App;
use buoyant::render_target::{EmbeddedGraphicsRenderTarget, RenderTarget as _};
use buoyant::view::prelude::*;
use core::time::Duration;
use embassy_time::{Instant, Timer};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use u8g2_fonts::{FontRenderer, fonts};
use wakey::board::Board;
use wakey::display::{self, Display};
use wakey::ui::{UI_HEIGHT, UI_WIDTH, canvas::Canvas, stats::FrameStats};

static DIGITS: FontRenderer = FontRenderer::new::<fonts::u8g2_font_logisoso92_tn>();
static LABEL: FontRenderer = FontRenderer::new::<fonts::u8g2_font_helvR24_tf>();

const BACKGROUND: Rgb565 = Rgb565::new(2, 5, 3);
const FOREGROUND: Rgb565 = Rgb565::new(30, 61, 31);
const MUTED: Rgb565 = Rgb565::new(17, 34, 21);
const ACCENT: Rgb565 = Rgb565::new(31, 44, 4);

#[derive(Debug, Clone, Default, PartialEq)]
struct State {
    seconds: u64,
    stats: heapless::String<64>,
}

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app().unwrap()))
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
    let start = Instant::now();
    let size = Size::new(UI_WIDTH, UI_HEIGHT);
    let mut app = App::new(State::default(), size.into(), root_view);
    let mut stats = FrameStats::new("buoyant");
    let mut animating = true;

    loop {
        let now = start.elapsed();
        app.set_time(Duration::from_micros(now.as_micros()));

        let seconds = now.as_secs();
        if seconds != app.state().seconds {
            let (fps, render_ms) = wakey::ui::stats::last();
            let mut s = app.state_mut();
            s.seconds = seconds;
            s.stats.clear();
            let _ = core::fmt::write(
                &mut s.stats,
                format_args!("{fps:.1} fps  render {render_ms:.1} ms"),
            );
        }

        if animating || app.should_redraw() {
            let buf = display.back_buffer().await;
            let t0 = Instant::now();
            {
                let mut canvas = Canvas::new(buf);
                let mut target = EmbeddedGraphicsRenderTarget::new_hinted(&mut canvas, BACKGROUND);
                target.clear(BACKGROUND);
                app.render_animated(&mut target, &FOREGROUND);
                animating = target.clear_animation_status();
            }
            let render = t0.elapsed();
            display.present(0..display::HEIGHT).expect("present");
            stats.frame(render, display::HEIGHT);
        } else {
            stats.maybe_log();
            Timer::after(embassy_time::Duration::from_millis(10)).await;
        }
    }
}

fn root_view(state: &State) -> impl View<Rgb565, State> + use<> {
    let s = state.seconds;
    let on = s.is_multiple_of(2);
    HStack::new((
        VStack::new((
            Text::new_fmt::<16>(
                format_args!("{:02}:{:02}:{:02}", s / 3600 % 24, s / 60 % 60, s % 60),
                &DIGITS,
            )
            .foreground_color(FOREGROUND),
            Text::new("Wakey - Buoyant", &LABEL).foreground_color(MUTED),
            Text::new_fmt::<64>(format_args!("{}", state.stats), &LABEL)
                .foreground_color(Rgb565::GREEN),
            toggle(on),
            sliding_bar(s % 4),
        ))
        .with_spacing(28)
        .flex_frame()
        .with_infinite_max_width(),
        VStack::new((
            row(s, 0),
            row(s, 1),
            row(s, 2),
            row(s, 3),
            row(s, 4),
            row(s, 5),
        ))
        .with_spacing(12)
        .padding(Edges::All, 24)
        .flex_frame()
        .with_infinite_max_width(),
    ))
}

fn toggle(on: bool) -> impl View<Rgb565, State> + use<> {
    let alignment = if on {
        Alignment::Trailing
    } else {
        Alignment::Leading
    };
    let color = if on { Rgb565::GREEN } else { Rgb565::RED };
    Capsule
        .foreground_color(color)
        .overlay(
            alignment,
            Circle
                .foreground_color(Rgb565::WHITE)
                .padding(Edges::All, 6),
        )
        .frame_sized(160, 72)
        .animated(Animation::ease_in_out(Duration::from_millis(500)), on)
        .geometry_group()
}

fn sliding_bar(step: u64) -> impl View<Rgb565, State> + use<> {
    RoundedRectangle::new(12)
        .foreground_color(ACCENT)
        .frame_sized(120, 24)
        .offset(step as i32 * 120 - 180, 0)
        .animated(Animation::ease_in_out(Duration::from_millis(900)), step)
}

fn row(seconds: u64, i: u64) -> impl View<Rgb565, State> + use<> {
    let n = seconds + i;
    let tint = Rgb565::new((n * 7 % 32) as u8, (n * 13 % 64) as u8, (n * 5 % 32) as u8);
    HStack::new((
        RoundedRectangle::new(10)
            .foreground_color(tint)
            .frame_sized(72, 72),
        Text::new_fmt::<32>(format_args!("Track {:03}", n % 1000), &LABEL)
            .foreground_color(FOREGROUND),
        Spacer::default(),
    ))
    .with_spacing(20)
    .padding(Edges::All, 8)
    .background_color(Rgb565::new(3, 8, 5), RoundedRectangle::new(12))
    .animated(Animation::ease_out(Duration::from_millis(400)), n)
}
