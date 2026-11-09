//! Static panel test: draws one fixed picture into both frame buffers, then
//! never flips again. If this flickers, the cause is the panel, its timing,
//! the backlight or its supply, not the rendering.
//!
//! Every 10 s the backlight steps 100 % → 50 % → 20 %; the number of white
//! squares in the top-left corner shows the step (1, 2, 3). The INA226 battery monitor is sampled every 20 ms and min/max per
//! second is logged, which shows supply oscillations that are hard to see.
//!
//! Picture (landscape): left third the Slint test background (#101418),
//! middle third 50 % grey, right third white, a grey ramp along the bottom.
//!
//! `cargo run --release --bin panel_test`

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use wakey::board::Board;
use wakey::board::i2c::SysI2c;
use wakey::board::ioexp::Pin;
use wakey::display;
use wakey::ui::{UI_HEIGHT, UI_WIDTH, canvas::Canvas};

const INA226: u8 = 0x41;

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app().unwrap()))
}

#[embassy_executor::task]
async fn app() {
    let Board {
        mut display,
        mut backlight,
        mut i2c,
        mut ioexp,
    } = Board::init().expect("board init");

    backlight.set_percent(100).expect("backlight");
    log::info!("panel_test: static picture, backlight 100 %");

    let ina = i2c.probe(INA226);
    if !ina {
        log::warn!("panel_test: INA226 (0x41) does not answer, no supply readings");
    }

    // Phases; the number of white squares in the top-left corner = phase.
    let phases: [(&str, u8); 3] = [
        ("backlight 100 %", 100),
        ("backlight 50 %", 50),
        ("backlight 20 %", 20),
    ];
    loop {
        for (i, (what, percent)) in phases.iter().enumerate() {
            backlight.set_percent(*percent).expect("backlight");
            // Same picture into both buffers, so the screen stays static.
            for _ in 0..2 {
                let buf = display.back_buffer().await;
                let mut canvas = Canvas::new(buf);
                draw(&mut canvas);
                draw_phase(&mut canvas, i as u32 + 1);
                display.present(0..display::HEIGHT).expect("present");
            }
            log::info!("panel_test: phase {} ({} squares) — {what}", i + 1, i + 1);
            let phase_end = Instant::now() + Duration::from_secs(10);
            while Instant::now() < phase_end {
                if ina {
                    log_supply_for_one_second(&mut i2c, &mut ioexp).await;
                } else {
                    Timer::after(Duration::from_secs(1)).await;
                }
            }
        }
    }
}

/// `n` white squares in the top-left (dark) corner.
fn draw_phase(canvas: &mut Canvas<'_>, n: u32) {
    for k in 0..n {
        let _ = Rectangle::new(Point::new(40 + k as i32 * 70, 40), Size::new(50, 50))
            .into_styled(PrimitiveStyle::with_fill(Rgb565::WHITE))
            .draw(canvas);
    }
}

/// Samples battery voltage and current every 20 ms for one second and logs
/// min/max. Shunt 5 mΩ: 2.5 µV/LSB → 0.5 mA/LSB. Bus: 1.25 mV/LSB.
async fn log_supply_for_one_second(i2c: &mut SysI2c, ioexp: &mut wakey::board::ioexp::IoExpanders) {
    let (mut v_min, mut v_max) = (i32::MAX, i32::MIN);
    let (mut i_min, mut i_max) = (i32::MAX, i32::MIN);
    for _ in 0..50 {
        let read = |i2c: &mut SysI2c, reg: u8| -> Option<i16> {
            let mut b = [0u8; 2];
            i2c.write_read(INA226, &[reg], &mut b).ok()?;
            Some(i16::from_be_bytes(b))
        };
        if let (Some(shunt), Some(bus)) = (read(i2c, 0x01), read(i2c, 0x02)) {
            let mv = bus as i32 * 125 / 100;
            let ma = shunt as i32 / 2;
            v_min = v_min.min(mv);
            v_max = v_max.max(mv);
            i_min = i_min.min(ma);
            i_max = i_max.max(ma);
        }
        Timer::after(Duration::from_millis(20)).await;
    }
    let charging = ioexp
        .get(i2c, Pin::ChgStat)
        .map(|high| !high)
        .unwrap_or(false);
    log::info!(
        "supply: battery {v_min}..{v_max} mV, current {i_min}..{i_max} mA (+ = into battery?), CHG_STAT {}",
        if charging { "charging" } else { "not charging" }
    );
}

fn draw(canvas: &mut Canvas<'_>) {
    let third = UI_WIDTH / 3;
    let fill = |c: &mut Canvas<'_>, x: u32, w: u32, color: Rgb565| {
        let _ = Rectangle::new(Point::new(x as i32, 0), Size::new(w, UI_HEIGHT))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(c);
    };
    fill(
        canvas,
        0,
        third,
        Rgb565::new(0x10 >> 3, 0x14 >> 2, 0x18 >> 3),
    );
    fill(canvas, third, third, Rgb565::new(16, 32, 16));
    fill(canvas, 2 * third, UI_WIDTH - 2 * third, Rgb565::WHITE);
    // 32-step grey ramp along the bottom edge.
    let (ramp_h, step) = (120, UI_WIDTH / 32);
    for i in 0..32u32 {
        let _ = Rectangle::new(
            Point::new((i * step) as i32, (UI_HEIGHT - ramp_h) as i32),
            Size::new(step, ramp_h),
        )
        .into_styled(PrimitiveStyle::with_fill(Rgb565::new(
            i as u8,
            (i * 2) as u8,
            i as u8,
        )))
        .draw(canvas);
    }
}
