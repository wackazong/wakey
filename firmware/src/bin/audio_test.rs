//! USB audio proof: plays a quiet 440 Hz sine (−20 dBFS) on a USB Audio
//! Class 1.0 speaker on the USB-A port (Creative Pebble V3). Logs the
//! device's audio descriptors and packet counters.
//!
//! `cargo run --release --bin audio_test`

use embassy_time::{Duration, Timer};
use std::sync::atomic::Ordering;
use wakey::audio::uac;
use wakey::board::Board;
use wakey::board::ioexp::Pin;

/// Tone frequency and level (fraction of full scale; 0.1 = −20 dBFS).
const TONE_HZ: f32 = 440.0;
const LEVEL: f32 = 0.1;

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app().unwrap()))
}

#[embassy_executor::task]
async fn app() {
    let Board {
        mut i2c, mut ioexp, ..
    } = Board::init().expect("board init");

    // Verbose logs from the USB host stack while bringing up audio.
    for tag in [c"HUB", c"ENUM", c"USBH", c"HCD DWC", c"USB HOST"] {
        unsafe {
            esp_idf_svc::sys::esp_log_level_set(
                tag.as_ptr(),
                esp_idf_svc::sys::esp_log_level_t_ESP_LOG_DEBUG,
            )
        };
    }
    uac::install_host().expect("USB host");

    // 5 V on the USB-A socket powers the speaker; give it time to boot
    // before the host looks at the port.
    ioexp.set(&mut i2c, Pin::Usb5vEn, true).expect("USB5V_EN");
    log::info!("audio_test: USB-A 5 V on");
    Timer::after(Duration::from_secs(1)).await;
    let mut phase = 0.0f32;
    let step = TONE_HZ / uac::SAMPLE_RATE as f32 * core::f32::consts::TAU;
    uac::play(Box::new(move |buf: &mut [i16]| {
        for frame in buf.as_chunks_mut::<{ uac::CHANNELS }>().0 {
            let s = (phase.sin() * LEVEL * i16::MAX as f32) as i16;
            frame.fill(s);
            phase += step;
            if phase > core::f32::consts::TAU {
                phase -= core::f32::consts::TAU;
            }
        }
    }));

    // Power the root port; retry by cycling it if nothing enumerates.
    for attempt in 1..=5 {
        uac::set_port_power(true).expect("port power");
        log::info!("audio_test: root port on (attempt {attempt})");
        Timer::after(Duration::from_secs(3)).await;
        if uac::DEVICE_FOUND.load(Ordering::Acquire) {
            break;
        }
        log::warn!("audio_test: no device yet, cycling the root port");
        uac::set_port_power(false).expect("port power");
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        Timer::after(Duration::from_secs(2)).await;
        log::info!(
            "audio_test: packets ok {} failed {}",
            uac::PACKETS_OK.load(Ordering::Relaxed),
            uac::PACKETS_ERR.load(Ordering::Relaxed)
        );
    }
}
