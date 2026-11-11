//! The alarm clock application: wires the services together and runs the
//! UI: the clock screen with the real time, Wi-Fi and NTP, touch, and the
//! alarm ringing on the USB speaker.
//!
//! Start-up order: board → clock from the RTC → config from the SD card →
//! Wi-Fi thread → touch task → USB audio thread → alarm → UI. The UI is
//! defined in `ui/wakey.slint` (one window, one page at a time); each page
//! has its view model here.

mod alarm;
mod alarm_list;
mod alarm_page;
#[cfg(feature = "ui-bench")]
mod bench;
mod clock_screen;
mod name_page;
mod sound_page;

use crate::audio::click::Clicker;
use crate::audio::player::Player;
use crate::audio::uac::UsbAudio;
use crate::board::Board;
use crate::board::ioexp::Pin;
use crate::display::ppa::PpaRotator;
use crate::sounds::SoundFile;
use crate::ui::{input, platform, render_loop};
use crate::{clock, config, net, runtime};
use alarm::AlarmClock;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wakey_core::alarm::AlarmConfig;

/// Components generated from `ui/wakey.slint` (see `build.rs`).
mod screens {
    include!(concat!(env!("OUT_DIR"), "/wakey.rs"));
}

/// `AppWindow.page` values.
const PAGE_CLOCK: i32 = 0;
const PAGE_LIST: i32 = 1;
const PAGE_ALARM: i32 = 2;
const PAGE_SOUND: i32 = 3;
const PAGE_NAME: i32 = 4;
/// One alarm list row, for the list layer's offscreen window.
const PAGE_LIST_ROW: i32 = 5;

/// Settings pages go back to the clock after this long without a touch.
const IDLE_CLOSE: Duration = Duration::from_secs(60);

/// What the settings pages share.
pub struct Ctx {
    window: slint::Weak<screens::AppWindow>,
    alarm: Rc<RefCell<AlarmClock>>,
    /// The alarm being edited (alarm, sound and name pages).
    draft: RefCell<AlarmConfig>,
    /// `sounds/` as listed when the sound page opened.
    sound_files: RefCell<Vec<SoundFile>>,
    /// The name being typed.
    name: RefCell<String>,
    click: Clicker,
}

impl Ctx {
    /// A callback that runs `f` with the window and the context.
    fn with(self: &Rc<Self>, f: fn(&screens::AppWindow, &Ctx)) -> impl Fn() + 'static {
        let ctx = self.clone();
        move || {
            if let Some(window) = ctx.window.upgrade() {
                f(&window, &ctx);
            }
        }
    }

    fn with_arg<A: 'static>(
        self: &Rc<Self>,
        f: fn(&screens::AppWindow, &Ctx, A),
    ) -> impl Fn(A) + 'static {
        let ctx = self.clone();
        move |a| {
            if let Some(window) = ctx.window.upgrade() {
                f(&window, &ctx, a);
            }
        }
    }

    fn with_args<A: 'static, B: 'static>(
        self: &Rc<Self>,
        f: fn(&screens::AppWindow, &Ctx, A, B),
    ) -> impl Fn(A, B) + 'static {
        let ctx = self.clone();
        move |a, b| {
            if let Some(window) = ctx.window.upgrade() {
                f(&window, &ctx, a, b);
            }
        }
    }
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
        codec,
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
    let click = Clicker::start(codec, i2c.clone(), ioexp.clone());
    spawner.spawn(input::touch_task(i2c, ioexp.clone()).unwrap());

    // Speaker: powered and set up now, streaming while the alarm rings.
    let player = Player::new();
    let speaker = UsbAudio::start(ioexp.switch(Pin::Usb5vEn), player.source()).expect("USB host");
    let alarm = Rc::new(RefCell::new(AlarmClock::new(
        config.alarms,
        config.ringing,
        nvs,
        player,
        speaker.clone(),
    )));

    let mut rot = PpaRotator::new().expect("PPA");
    let window = platform::init();
    let ui = screens::AppWindow::new().expect("UI");
    ui.show().expect("show");
    let ctx = Rc::new(Ctx {
        window: ui.as_weak(),
        alarm: alarm.clone(),
        draft: RefCell::new(AlarmConfig::default()),
        sound_files: RefCell::new(Vec::new()),
        name: RefCell::new(String::new()),
        click,
    });
    alarm_list::start(&ui, &ctx);
    // Rows ready before the list is first opened (rendered in idle time).
    alarm_list::refresh(&ui, &ctx.alarm.borrow());
    let mut list = alarm_list::layer(&ui);
    alarm_page::start(&ui, &ctx);
    sound_page::start(&ui, &ctx);
    name_page::start(&ui, &ctx);
    let _idle = close_when_idle(&ctx);
    let _ticker = clock_screen::start(&ui, speaker, wifi, alarm);

    backlight.set_percent(100).expect("backlight");
    watch_render_loop();
    #[cfg(feature = "ui-bench")]
    spawner.spawn(bench::run().unwrap());
    render_loop::run(window, display, &mut rot, Some(&mut list)).await
}

/// Every few seconds: back to the clock after a minute without a touch
/// (not while a preview plays); the alarm page's "Klingelt in" kept
/// current.
fn close_when_idle(ctx: &Rc<Ctx>) -> Timer {
    let timer = Timer::default();
    timer.start(
        TimerMode::Repeated,
        Duration::from_secs(5),
        ctx.with(|window, ctx| {
            let page = window.get_page();
            if page == PAGE_CLOCK {
                return;
            }
            if input::idle().as_millis() >= IDLE_CLOSE.as_millis() as u64
                && !ctx.alarm.borrow().is_previewing()
            {
                log::info!(
                    "ui: back to the clock after {} s without a touch",
                    IDLE_CLOSE.as_secs()
                );
                window.set_page(PAGE_CLOCK);
            } else if page == PAGE_ALARM {
                alarm_page::fill(window, ctx);
                // Keeps the speaker on while the wheels are shown.
                ctx.click.wake();
            }
        }),
    );
    timer
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
