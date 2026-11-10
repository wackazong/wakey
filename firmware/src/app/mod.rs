//! The alarm clock application: wires the services together and runs the
//! UI: the clock screen with the real time, Wi-Fi and NTP, touch, and the
//! alarm ringing on the USB speaker.
//!
//! Start-up order: board → clock from the RTC → config from the SD card →
//! Wi-Fi thread → touch task → USB audio thread → alarm → UI. The UI is
//! defined in `ui/wakey.slint` (one window, one page at a time); each page
//! has its view model here.

mod alarm;
mod alarm_page;
mod clock_screen;

use crate::audio::player::Player;
use crate::audio::uac::UsbAudio;
use crate::board::Board;
use crate::board::ioexp::Pin;
use crate::display::ppa::PpaRotator;
use crate::ui::{input, platform, render_loop};
use crate::{clock, config, net, runtime};
use alarm::AlarmClock;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

/// Components generated from `ui/wakey.slint` (see `build.rs`).
mod screens {
    include!(concat!(env!("OUT_DIR"), "/wakey.rs"));
}

/// `AppWindow.page` values.
const PAGE_CLOCK: i32 = 0;
const PAGE_ALARM: i32 = 1;

/// Entry point of the `wakey` binary.
pub fn main() -> ! {
    runtime::start(|spawner| spawner.spawn(app(spawner).unwrap()))
}

#[embassy_executor::task]
async fn app(spawner: embassy_executor::Spawner) {
    let Board {
        display,
        mut backlight,
        i2c,
        ioexp,
        rtc,
        modem,
        sd: _sd, // stays mounted
        ..
    } = Board::init().expect("board init");

    clock::init(&rtc);
    let config = config::load();
    let nvs = EspDefaultNvsPartition::take().expect("NVS");
    let wifi = net::start(
        modem,
        ioexp.switch(Pin::WlanPwrEn),
        nvs.clone(),
        config.wifi,
        move || clock::store_system_time(&rtc),
    );
    spawner.spawn(input::touch_task(i2c, ioexp.clone()).unwrap());

    // Speaker: powered and set up now, streaming while the alarm rings.
    let player = Player::new();
    let speaker = UsbAudio::start(ioexp.switch(Pin::Usb5vEn), player.source()).expect("USB host");
    let alarm = Rc::new(RefCell::new(AlarmClock::new(
        config.alarm,
        nvs,
        player,
        speaker.clone(),
    )));

    let mut rot = PpaRotator::new().expect("PPA");
    let window = platform::init();
    let ui = screens::AppWindow::new().expect("UI");
    ui.show().expect("show");
    let _alarm_page = alarm_page::start(&ui, alarm.clone());
    let _ticker = clock_screen::start(&ui, speaker, wifi, alarm);

    backlight.set_percent(100).expect("backlight");
    watch_render_loop();
    render_loop::run(window, display, &mut rot, None).await
}

/// Logs when the render loop has not gone round for 2 s, and where it is.
fn watch_render_loop() {
    use core::sync::atomic::Ordering::Relaxed;
    std::thread::Builder::new()
        .name("ui-watch".into())
        .stack_size(4096)
        .spawn(|| {
            let mut last = render_loop::ITERATIONS.load(Relaxed);
            let mut still = 0u32;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let now = render_loop::ITERATIONS.load(Relaxed);
                if now != last {
                    last = now;
                    still = 0;
                    continue;
                }
                still += 1;
                if still == 2 || still.is_multiple_of(10) {
                    let stage = render_loop::STAGE.load(Relaxed) as usize;
                    log::error!(
                        "ui: render loop stuck for {still} s in '{}', panel frames {}, touch read {}",
                        render_loop::STAGES.get(stage).unwrap_or(&"?"),
                        crate::display::Display::frames_scanned(),
                        if input::READING.load(Relaxed) { "busy" } else { "idle" },
                    );
                }
            }
        })
        .expect("ui-watch thread");
}

