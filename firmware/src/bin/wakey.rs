//! The alarm clock. Clock screen with the real time (RX8130 RTC, Europe/
//! Zurich), touch, and the USB speaker ready. Alarms come next.
//!
//! `cargo run --release --bin wakey`

use slint::{SharedString, Timer, TimerMode};
use std::rc::Rc;
use wakey::audio::player::Player;
use wakey::audio::uac::UsbAudio;
use wakey::board::Board;
use wakey::board::ioexp::Pin;
use wakey::clock;
use wakey::config::Config;
use wakey::display::ppa::PpaRotator;
use wakey::net::{self, Status};
use wakey::ui::slint_platform;

include!(concat!(env!("OUT_DIR"), "/wakey.rs"));

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
        rtc,
        modem,
        sd,
        ..
    } = Board::init().expect("board init");
    clock::init(&rtc);
    let config = Config::load();
    // The card stays mounted (config writes, sounds).
    let _sd = sd;
    let wifi = net::start(modem, ioexp.clone(), rtc.clone(), config.wifi.clone());
    spawner.spawn(wakey::ui::touch::touch_task(i2c).unwrap());

    // Speaker: powered and set up now, stream off until an alarm rings.
    let player = Player::new();
    let vbus = ioexp.clone();
    let usb = UsbAudio::start(
        Box::new(move |on| vbus.set(Pin::Usb5vEn, on)),
        player.source(),
    )
    .expect("USB host");

    let mut rot = PpaRotator::new().expect("PPA");
    let window = slint_platform::init();
    let ui = ClockScreen::new().expect("Slint UI");
    ui.set_alarm(SharedString::from("Kein Wecker gestellt"));
    ui.show().expect("show");

    // Once per second, aligned to the second: set only the fields that
    // changed, so unchanged ones stay clean.
    let weak = ui.as_weak();
    let tick = {
        let usb = usb.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let t = clock::now();
            let set = |cur: SharedString, new: String, f: &dyn Fn(SharedString)| {
                if cur.as_str() != new {
                    f(SharedString::from(new));
                }
            };
            set(ui.get_hours(), format!("{:02}", t.hour), &|v| {
                ui.set_hours(v)
            });
            set(ui.get_minutes(), format!("{:02}", t.minute), &|v| {
                ui.set_minutes(v)
            });
            set(ui.get_seconds(), format!("{:02}", t.second), &|v| {
                ui.set_seconds(v)
            });
            set(ui.get_date(), t.date_de(), &|v| ui.set_date(v));
            if ui.get_speaker_connected() != usb.connected() {
                ui.set_speaker_connected(usb.connected());
            }
            let (text, state, bars) = wifi_line(&wifi.status());
            set(ui.get_wifi_text(), text, &|v| ui.set_wifi_text(v));
            if ui.get_wifi_state() != state {
                ui.set_wifi_state(state);
            }
            if ui.get_wifi_bars() != bars {
                ui.set_wifi_bars(bars);
            }
        }
    };
    tick();
    let second = Rc::new(Timer::default());
    let aligner = Timer::default();
    {
        let second = second.clone();
        let tick = tick.clone();
        let to_boundary = 1_000_000 - clock::now().micros as u64;
        aligner.start(
            TimerMode::SingleShot,
            core::time::Duration::from_micros(to_boundary + 2_000),
            move || {
                tick();
                second.start(
                    TimerMode::Repeated,
                    core::time::Duration::from_secs(1),
                    tick.clone(),
                );
            },
        );
    }

    backlight.set_percent(100).expect("backlight");
    slint_platform::run(window, display, &mut rot, None).await
}

/// Status line text, colour state (0 off, 1 busy, 2 connected, 3 error) and
/// signal bars for the clock screen.
fn wifi_line(s: &Status) -> (String, i32, i32) {
    match s {
        Status::NotConfigured { networks: Some(n) } => (
            format!("WLAN nicht eingerichtet · {n} Netze in Reichweite"),
            0,
            0,
        ),
        Status::NotConfigured { networks: None } => ("WLAN nicht eingerichtet".into(), 0, 0),
        Status::Starting => ("WLAN startet…".into(), 1, 0),
        Status::Connecting { ssid } => (format!("Verbinde mit {ssid}…"), 1, 0),
        Status::Connected {
            ssid,
            ip,
            rssi,
            time_synced,
        } => {
            let bars = match rssi {
                Some(r) if *r >= -55 => 4,
                Some(r) if *r >= -65 => 3,
                Some(r) if *r >= -75 => 2,
                Some(_) => 1,
                None => 4,
            };
            let ntp = if *time_synced { " · Zeit via NTP" } else { "" };
            (format!("{ssid} · {ip}{ntp}"), 2, bars)
        }
        Status::Failed { ssid, reason } => (format!("{ssid}: {reason}"), 3, 0),
    }
}
