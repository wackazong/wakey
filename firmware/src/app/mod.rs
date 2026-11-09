//! The alarm clock application: wires the services together and runs the
//! UI. So far: the clock screen with the real time, Wi-Fi and NTP, touch,
//! and the USB speaker set up and idle (alarms come next).
//!
//! Start-up order: board → clock from the RTC → config from the SD card →
//! Wi-Fi thread → touch task → USB audio thread → UI. The screens are
//! defined in `ui/wakey.slint`.

mod clock_screen;

use crate::audio::player::Player;
use crate::audio::uac::UsbAudio;
use crate::board::Board;
use crate::board::ioexp::Pin;
use crate::display::ppa::PpaRotator;
use crate::ui::{input, platform, render_loop};
use crate::{clock, config, net, runtime};
use slint::ComponentHandle;

/// Components generated from `ui/wakey.slint` (see `build.rs`).
mod screens {
    include!(concat!(env!("OUT_DIR"), "/wakey.rs"));
}

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
    let wifi = net::start(
        modem,
        ioexp.switch(Pin::WlanPwrEn),
        config.wifi,
        move || clock::store_system_time(&rtc),
    );
    spawner.spawn(input::touch_task(i2c).unwrap());

    // Speaker: powered and set up now, streaming once an alarm rings.
    let player = Player::new();
    let speaker = UsbAudio::start(ioexp.switch(Pin::Usb5vEn), player.source()).expect("USB host");

    let mut rot = PpaRotator::new().expect("PPA");
    let window = platform::init();
    let screen = screens::ClockScreen::new().expect("clock screen");
    screen.show().expect("show");
    let _ticker = clock_screen::start(&screen, speaker, wifi);

    backlight.set_percent(100).expect("backlight");
    render_loop::run(window, display, &mut rot, None).await
}
