//! The alarm clock application: wires the services together and runs the
//! UI: the clock screen with the real time, Wi-Fi and NTP, touch, and the
//! alarm ringing on the USB speaker.
//!
//! Start-up order: board → clock from the RTC → config from the SD card →
//! Wi-Fi thread → player and built-in speaker → touch thread → USB audio
//! thread → alarm → UI → light meter → render loop. The UI is defined in
//! `ui/wakey.slint` (one window, one page at a time); each page has its
//! view model here.

mod alarm;
mod alarm_list;
mod alarm_page;
#[cfg(feature = "ui-bench")]
mod bench;
mod clock_screen;
mod name_page;
mod power;
mod sound_page;
mod texts;

use crate::audio::builtin::BuiltinSpeaker;
use crate::audio::output::Output;
use crate::audio::player::Player;
use crate::audio::uac::UsbAudio;
use crate::board::Board;
use crate::board::ioexp::Pin;
use crate::display::ppa::PpaRotator;
use crate::ui::{input, platform, render_loop, transition};
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

use screens::Page;

/// How deep a page is: going deeper slides the new page in from the
/// right, going back from the left.
fn depth(page: Page) -> u8 {
    match page {
        Page::Clock => 0,
        Page::AlarmList => 1,
        Page::Alarm => 2,
        // Below the alarm page; the others are never shown by `show_page`.
        _ => 3,
    }
}

/// Shows `page`, sliding it in: the alarm pages come up over the clock and
/// go down again; within them deeper pages come from the right. Leaving the
/// sound page ends what it runs (TIDAL, AirDrop, a preview), whatever made
/// it go: its buttons, the idle timeout, an alarm ringing.
fn show_page(window: &screens::AppWindow, ctx: &Ctx, page: Page) {
    use transition::Direction;
    // A Wi-Fi session for TIDAL stays up while an alarm is edited.
    crate::tidal::set_editing(matches!(page, Page::Alarm | Page::Sound | Page::Name));
    let from = window.get_page();
    if from == page {
        return;
    }
    if from == Page::Sound {
        sound_page::leave(window, ctx);
    }
    let direction = if from == Page::Clock {
        Direction::Up
    } else if page == Page::Clock {
        Direction::Down
    } else if depth(page) > depth(from) {
        Direction::Forward
    } else {
        Direction::Back
    };
    // What fills the new page's part of the screen until it is rendered:
    // the clock's sky, or the settings pages' background.
    let background = if page == Page::Clock {
        window.global::<screens::Clock>().get_sky_middle()
    } else {
        window.global::<screens::Theme>().get_background()
    };
    transition::set_background(crate::ui::rgb565(background));
    transition::request(direction);
    window.set_page(page);
}

/// Repaints the whole window when the pixel dimming changes (the light
/// meter switches it from its own thread: `ui::dim`).
fn repaint_when_dimmed(ui: &screens::AppWindow) -> slint::Timer {
    let weak = ui.as_weak();
    let mut seen = crate::ui::dim::epoch();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            if crate::ui::dim::changed_since(&mut seen)
                && let Some(ui) = weak.upgrade()
            {
                ui.set_repaint_all(!ui.get_repaint_all());
            }
        },
    );
    timer
}

/// Settings pages go back to the clock after this long without a touch.
const IDLE_CLOSE: Duration = Duration::from_secs(60);
/// On the sound page: TIDAL downloads and AirDrop take their time.
const IDLE_CLOSE_SOUND: Duration = Duration::from_secs(5 * 60);

/// What the settings pages share.
pub struct Ctx {
    window: slint::Weak<screens::AppWindow>,
    alarm: Rc<RefCell<AlarmClock>>,
    /// The alarm being edited (alarm, sound and name pages).
    draft: RefCell<AlarmConfig>,
    /// The alarm as saved when its page opened (`None`: a new alarm);
    /// Sichern only after a change.
    original: RefCell<Option<AlarmConfig>>,
    /// The name being typed.
    name: RefCell<String>,
    /// The built-in speaker, for the wheel click.
    click: BuiltinSpeaker,
    /// TIDAL login and state (sound page's TIDAL tab).
    tidal: crate::tidal::Tidal,
    /// The sound page's lists, timers and covers.
    sound: sound_page::SoundPageState,
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
async fn app(
    // Only the UI bench spawns a task here (its scripted gestures).
    #[allow(unused_variables)] spawner: embassy_executor::Spawner,
) {
    let Board {
        display,
        backlight,
        i2c,
        ioexp,
        rtc,
        modem,
        c6_link,
        sd: _sd, // stays mounted
        codec,
        ..
    } = Board::init().expect("board init");

    clock::init(&rtc);
    let config = match config::load() {
        Ok(c) => c,
        Err(e) => config_error(display, backlight, e).await,
    };
    let nvs = EspDefaultNvsPartition::take().expect("NVS");
    let wifi = net::start(
        net::Setup {
            modem,
            c6_power: ioexp.switch(Pin::WlanPwrEn),
            c6_link,
            nvs: nvs.clone(),
            cfg: config.wifi.clone(),
            interval: Duration::from_secs(u64::from(config.sync_interval_h) * 3600),
            display_asleep: crate::ui::power::asleep,
        },
        move || clock::store_system_time(&rtc),
    );
    // Speakers: set up now; the player's sound goes to the USB speaker,
    // or the built-in one if none is connected, while an alarm rings.
    let player = Player::new();
    let builtin = BuiltinSpeaker::start(codec, i2c.clone(), ioexp.clone(), player.source());
    input::start(i2c.clone(), ioexp.clone());
    let speaker = UsbAudio::start(ioexp.switch(Pin::Usb5vEn), player.source()).expect("USB host");
    let alarm = Rc::new(RefCell::new(AlarmClock::new(
        config.alarms,
        config.ringing,
        nvs,
        player,
        Output::new(speaker.clone(), builtin.clone()),
    )));

    let mut rot = PpaRotator::new().expect("PPA");
    let window = platform::init();
    let ui = screens::AppWindow::new().expect("UI");
    ui.show().expect("show");
    let ctx = Rc::new(Ctx {
        window: ui.as_weak(),
        alarm: alarm.clone(),
        draft: RefCell::new(AlarmConfig::default()),
        original: RefCell::new(None),
        name: RefCell::new(String::new()),
        click: builtin,
        tidal: crate::tidal::Tidal::new(config.tidal.clone(), wifi.clone()),
        sound: sound_page::SoundPageState::default(),
    });
    // The saved alarms' TIDAL tracks on the card (downloads if missing).
    sound_page::select_tidal(&ctx);
    alarm_list::start(&ui, &ctx);
    // Rows ready before the list is first opened (rendered in idle time).
    alarm_list::refresh(&ui, &ctx.alarm.borrow());
    // The alarm list and the sound page's list each scroll on a list layer;
    // their pages are never shown together, so they share its buffers.
    let alarm_layer = alarm_list::layer(&ui);
    let sound_layer = sound_page::layer(&ui, alarm_layer.buffers());
    let mut list = crate::ui::list_layer::SharedLayers::new(alarm_layer, sound_layer);
    alarm_page::start(&ui, &ctx);
    sound_page::start(&ui, &ctx);
    name_page::start(&ui, &ctx);
    let _idle = close_when_idle(&ctx);
    let _dim = repaint_when_dimmed(&ui);
    let _power = power::start(
        &ui,
        config.power.clone(),
        alarm.clone(),
        i2c.clone(),
        speaker.clone(),
    );
    let _ticker = clock_screen::start(&ui, &ctx, speaker, wifi);

    // The light meter owns the backlight: full at first, then the room's.
    let screen = crate::ambient::Screen {
        asleep: crate::ui::power::asleep,
        pixels: crate::ui::dim::factor,
        set_pixels: crate::ui::dim::set,
        full: crate::ui::dim::FULL,
    };
    crate::ambient::start(
        i2c.clone(),
        backlight,
        screen,
        config.display.night_pixels_percent,
    );
    watch_render_loop();
    #[cfg(feature = "task-stats")]
    crate::taskstats::start();
    #[cfg(feature = "ui-bench")]
    spawner.spawn(bench::run().unwrap());
    render_loop::run(window, display, &mut rot, Some(&mut list)).await
}

/// `config.json` is broken: says so on screen and goes no further (no
/// alarms, no Wi-Fi, nothing written).
async fn config_error(
    display: crate::display::Display,
    mut backlight: crate::board::Backlight,
    error: String,
) -> ! {
    let mut rot = PpaRotator::new().expect("PPA");
    let window = platform::init();
    let ui = screens::AppWindow::new().expect("UI");
    ui.set_error_text(error.into());
    ui.set_page(Page::ConfigError);
    ui.show().expect("show");
    let _ = backlight.set_level(0.6);
    render_loop::run(window, display, &mut rot, None).await
}

/// Every few seconds: back to the clock after [`IDLE_CLOSE`] without a
/// touch ([`IDLE_CLOSE_SOUND`] on the sound page), not while a preview
/// plays, a TIDAL login waits or a file arrives by AirDrop; the alarm
/// page's "Klingelt in" kept current.
fn close_when_idle(ctx: &Rc<Ctx>) -> Timer {
    let timer = Timer::default();
    timer.start(
        TimerMode::Repeated,
        Duration::from_secs(5),
        ctx.with(|window, ctx| {
            let page = window.get_page();
            if page == Page::Clock {
                return;
            }
            let limit = if page == Page::Sound {
                IDLE_CLOSE_SOUND
            } else {
                IDLE_CLOSE
            };
            // Not while a preview plays, nor while a TIDAL login waits for
            // the user (the QR code on the sound page), nor while a file
            // arrives by AirDrop.
            if input::idle().as_secs() >= limit.as_secs()
                && !ctx.alarm.borrow().is_previewing()
                && !ctx.tidal.logging_in()
                && !matches!(crate::airdrop::state(), crate::airdrop::State::Receiving(_))
            {
                log::info!(
                    "ui: back to the clock after {} s without a touch",
                    limit.as_secs()
                );
                show_page(window, ctx, Page::Clock);
            } else if page == Page::Alarm {
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
    let watch = runtime::Thread::new(c"ui-watch", 4096).spawn(|| {
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
                log::error!(
                    "ui: render loop stuck for {still} s in '{}', panel frames {}, touch read {}",
                    render_loop::stage().name(),
                    crate::display::Display::frames_scanned(),
                    if input::READING.load(Relaxed) {
                        "busy"
                    } else {
                        "idle"
                    },
                );
            }
        }
    });
    if let Err(e) = watch {
        log::error!("ui: render loop watch not started: {e}");
    }
}
