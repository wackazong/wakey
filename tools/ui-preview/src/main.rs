//! Renders the firmware's pages (`firmware/ui/wakey.slint`) on the host,
//! filled with sample data, to PNG files: one per scene. Same Slint, same
//! software renderer, same RGB565 pixels as on the Tab5, so layout, fonts
//! and missing glyphs show up here first.
//!
//! `cargo run --release -- [--by-line] [output dir] [scene name filter]`
//!
//! The output dir defaults to `target/ui-preview`. `--by-line` renders line
//! by line through a `LineBufferProvider`, as the firmware does, instead of
//! into one buffer (same pixels; the timing then follows the device's
//! code path).

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

slint::include_modules!();

const W: usize = 1280;
const H: usize = 720;

struct Platform(Rc<MinimalSoftwareWindow>);

impl slint::platform::Platform for Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

struct Preview {
    window: Rc<MinimalSoftwareWindow>,
    ui: AppWindow,
    out: PathBuf,
    filter: Option<String>,
    by_line: bool,
}

impl Preview {
    /// Lets timers and animations run for `ms` (in 16 ms steps).
    fn run(&self, ms: u64) {
        for _ in 0..ms / 16 {
            std::thread::sleep(Duration::from_millis(16));
            slint::platform::update_timers_and_animations();
        }
    }

    fn press(&self, x: f32, y: f32) {
        let position = LogicalPosition::new(x, y);
        self.window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
    }

    fn move_to(&self, x: f32, y: f32) {
        self.window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(x, y),
        });
    }

    fn release(&self, x: f32, y: f32) {
        let position = LogicalPosition::new(x, y);
        self.window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
        self.window.dispatch_event(WindowEvent::PointerExited);
    }

    fn shot(&self, name: &str) {
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !name.contains(f.as_str()))
        {
            return;
        }
        slint::platform::update_timers_and_animations();
        let mut buf = vec![Rgb565Pixel::default(); W * H];
        self.window.request_redraw();
        let t = std::time::Instant::now();
        self.window.draw_if_needed(|r| {
            if self.by_line {
                r.render_by_line(Lines(&mut buf));
            } else {
                r.render(&mut buf, W);
            }
        });
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        let path = self.out.join(format!("{name}.png"));
        write_png(&path, &buf);
        println!("{ms:6.2} ms {}", path.display());
    }
}

/// Line-by-line rendering into a full-screen buffer, as on the device.
struct Lines<'a>(&'a mut [Rgb565Pixel]);

impl slint::platform::software_renderer::LineBufferProvider for Lines<'_> {
    type TargetPixel = Rgb565Pixel;
    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Rgb565Pixel]),
    ) {
        render_fn(&mut self.0[line * W + range.start..line * W + range.end]);
    }
}

fn write_png(path: &Path, buf: &[Rgb565Pixel]) {
    let file = std::fs::File::create(path).expect("create PNG");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), W as u32, H as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("PNG header");
    let rgb: Vec<u8> = buf
        .iter()
        .flat_map(|p| {
            let v = p.0;
            let r = ((v >> 11) & 0x1f) as u8;
            let g = ((v >> 5) & 0x3f) as u8;
            let b = (v & 0x1f) as u8;
            [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
        })
        .collect();
    w.write_image_data(&rgb).expect("PNG data");
}

fn days(d: [u8; 7]) -> ModelRc<bool> {
    ModelRc::new(VecModel::from(
        d.iter().map(|&x| x != 0).collect::<Vec<_>>(),
    ))
}

fn row(id: i32, time: &str, name: &str, d: [u8; 7], enabled: bool, next: bool) -> AlarmRow {
    AlarmRow {
        id,
        time: time.into(),
        name: name.into(),
        // Bit i for weekday i, as `AlarmConfig::weekday_bits`.
        days: d
            .iter()
            .enumerate()
            .filter(|&(_, &x)| x != 0)
            .fold(0, |bits, (i, _)| bits | 1 << i),
        enabled,
        next,
    }
}

fn sound(title: &str, sub: &str, selected: bool, kind: i32) -> SoundRow {
    SoundRow {
        title: title.into(),
        sub: sub.into(),
        selected,
        kind,
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let by_line = args.iter().any(|a| a == "--by-line");
    args.retain(|a| a != "--by-line");
    let mut args = args.into_iter();
    let out = PathBuf::from(args.next().unwrap_or_else(|| "target/ui-preview".into()));
    let filter = args.next();
    std::fs::create_dir_all(&out).expect("output dir");

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(PhysicalSize::new(W as u32, H as u32));
    slint::platform::set_platform(Box::new(Platform(window.clone()))).expect("platform");
    let ui = AppWindow::new().expect("UI");
    ui.show().expect("show");
    let p = Preview {
        window,
        ui,
        out,
        filter,
        by_line,
    };
    let ui = &p.ui;

    // Clock.
    let clock = ui.global::<Clock>();
    clock.set_hours("06".into());
    clock.set_minutes("12".into());
    clock.set_seconds("41".into());
    clock.set_date("Dienstag, 10. November 2026".into());
    clock.set_alarm("Wecker morgen 06:30".into());
    clock.set_speaker_connected(true);
    clock.set_wifi_text("MeinNetz · 192.168.1.23 · Zeit via NTP".into());
    clock.set_wifi_state(2);
    clock.set_wifi_bars(3);
    ui.set_page(0);
    p.shot("0-clock");

    // Alarm list.
    let list = ui.global::<AlarmList>();
    list.set_rows(ModelRc::new(VecModel::from(vec![
        row(1, "06:30", "Arbeit", [1, 1, 0, 0, 0, 0, 0], true, true),
        row(2, "07:15", "", [0, 0, 1, 1, 1, 0, 0], true, false),
        row(3, "16:45", "Yoga", [1, 0, 0, 0, 0, 0, 0], true, false),
        row(
            4,
            "08:30",
            "Wochenende",
            [0, 0, 0, 0, 0, 1, 1],
            false,
            false,
        ),
        row(
            5,
            "22:00",
            "Medikamente",
            [1, 1, 1, 1, 1, 1, 1],
            true,
            false,
        ),
    ])));
    ui.set_page(1);
    p.shot("1-list");
    list.on_delete(|id| println!("delete {id}"));
    list.on_open(|id| println!("open {id}"));
    list.on_toggle(|id, on| println!("toggle {id} {on}"));
    // Swipe the second row left and let go: the delete button stays.
    p.press(1000.0, 369.0);
    for x in (700..1000).rev().step_by(20) {
        p.move_to(x as f32, 371.0);
        p.run(16);
    }
    p.release(700.0, 371.0);
    p.run(300);
    p.shot("1-list-swiped");
    // Tap elsewhere: closes it.
    p.press(600.0, 207.0);
    p.release(600.0, 207.0);
    p.run(300);
    // Flick up: the list scrolls on by itself.
    p.press(600.0, 600.0);
    for k in 1..=6 {
        p.move_to(600.0, 600.0 - k as f32 * 30.0);
        p.run(16);
    }
    p.release(600.0, 420.0);
    p.run(800);
    p.shot("1-list-scrolled");
    // Swipe a row of the scrolled list all the way: deleted.
    p.press(1000.0, 400.0);
    for k in 1..=20 {
        p.move_to(1000.0 - k as f32 * 45.0, 402.0);
        p.run(16);
    }
    p.release(100.0, 402.0);
    p.run(300);
    // Tap a switch and a row.
    p.press(1150.0, 207.0);
    p.release(1150.0, 207.0);
    p.run(50);
    ui.set_page(0);
    list.set_rows(ModelRc::new(VecModel::from(Vec::<AlarmRow>::new())));
    ui.set_page(1);
    p.shot("1-list-empty");

    // Alarm page.
    let ed = ui.global::<AlarmEditor>();
    ed.set_hour(16);
    ed.set_minute(45);
    ed.set_name("Yoga".into());
    ed.set_days(days([1, 0, 0, 0, 0, 0, 0]));
    ed.set_any_day(true);
    ed.set_relative("Klingelt in 6 Tg. 4 Std. 33 Min.".into());
    ed.set_sound("Morgenlicht".into());
    ed.set_volume_bars(3);
    ed.set_fade("10 s".into());
    ui.set_page(2);
    p.shot("2-alarm");
    // Drag the minute wheel up by two and a half rows, mid-drag.
    p.press(420.0, 352.0);
    for i in 1..=11 {
        p.move_to(420.0, 352.0 - i as f32 * 20.0);
        p.run(16);
    }
    p.shot("2-alarm-dragging");
    p.release(420.0, 132.0);
    p.run(1500);
    p.shot("2-alarm-settled");
    ui.set_page(0);
    ed.set_name("".into());
    ed.set_hour(7);
    ed.set_minute(0);
    ed.set_days(days([0; 7]));
    ed.set_any_day(false);
    ed.set_sound("Eingebauter Ton".into());
    ui.set_page(2);
    p.shot("2-alarm-new-no-days");

    // Sound page.
    let sp = ui.global::<SoundPicker>();
    sp.set_rows(ModelRc::new(VecModel::from(vec![
        sound("Eingebauter Ton", "Piepen", false, 0),
        sound("Zufällig", "wechselt jeden Tag", false, 1),
        sound("Morgenlicht", "", true, 2),
        sound("Vogelstimmen", "", false, 2),
        sound("Kirchenglocken", "", false, 2),
        sound("Meeresrauschen", "", false, 2),
    ])));
    sp.set_volume(60);
    sp.set_fade(1);
    ui.set_page(3);
    p.shot("3-sound");
    sp.set_previewing(true);
    p.shot("3-sound-previewing");
    sp.set_previewing(false);
    sp.set_tab(1);
    sp.set_rows(ModelRc::new(VecModel::from(Vec::<SoundRow>::new())));
    sp.set_empty("TIDAL ist noch nicht eingerichtet.".into());
    p.shot("3-sound-tidal");

    // Name.
    let ne = ui.global::<NameEditor>();
    ne.set_text("Arbeit".into());
    ui.set_page(4);
    p.shot("4-name");
    ne.set_text("".into());
    ne.set_shift(true);
    p.shot("4-name-empty-shift");
    ne.set_numbers(true);
    p.shot("4-name-numbers");
    ne.set_numbers(false);
    ne.set_text("Übung Ärger Öl äöü ß".into());
    p.shot("4-name-umlauts");
    ed.set_name("Übung Ärger Öl".into());
    ui.set_page(2);
    p.shot("2-alarm-umlauts");
    clock.set_wifi_text("24px: ÜÄÖ üäö".into());
    clock.set_alarm("30px: ÜÄÖ üäö".into());
    clock.set_date("40px: ÜÄÖ üäö".into());
    ui.set_page(0);
    p.shot("0-clock-umlauts");
}
