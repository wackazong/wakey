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
use slint::{ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, VecModel};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

slint::include_modules!();

const W: usize = 1280;
const H: usize = 720;

/// Hands out the prepared windows in order: the main window, then the
/// alarm list's row window (as the firmware's `ui::platform`).
struct Platform(std::cell::RefCell<Vec<Rc<MinimalSoftwareWindow>>>);

impl slint::platform::Platform for Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let mut w = self.0.borrow_mut();
        if w.is_empty() {
            return Err(slint::PlatformError::Other("no window prepared".into()));
        }
        Ok(w.remove(0))
    }
}

struct Preview {
    /// Renders like the firmware: only what changed, into two buffers in
    /// turn (`RepaintBufferType::SwappedBuffers`), once per 16 ms step.
    window: Rc<MinimalSoftwareWindow>,
    buffers: std::cell::RefCell<[Vec<Rgb565Pixel>; 2]>,
    /// The buffer rendered last (what the panel would show).
    front: std::cell::Cell<usize>,
    ui: AppWindow,
    /// The alarm list's row as the list layer renders it.
    row_window: Rc<MinimalSoftwareWindow>,
    row_view: AppWindow,
    out: PathBuf,
    filter: Option<String>,
    by_line: bool,
}

impl Preview {
    /// Lets timers and animations run for `ms` (in 16 ms steps).
    fn run(&self, ms: u64) {
        for _ in 0..ms / 16 {
            std::thread::sleep(Duration::from_millis(16));
            self.frame();
        }
    }

    /// One frame: timers and animations, then Slint renders what changed
    /// into the back buffer, which becomes the front. Returns the render
    /// time, or `None` if nothing changed.
    fn frame(&self) -> Option<f32> {
        slint::platform::update_timers_and_animations();
        let back = 1 - self.front.get();
        let mut buffers = self.buffers.borrow_mut();
        let buf = &mut buffers[back];
        let t = std::time::Instant::now();
        let drawn = self.window.draw_if_needed(|r| {
            if self.by_line {
                r.render_by_line(Lines(buf));
            } else {
                r.render(buf, W);
            }
        });
        drawn.then(|| {
            self.front.set(back);
            t.elapsed().as_secs_f32() * 1000.0
        })
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

    /// Renders the main window into a new buffer.
    /// Renders a frame; returns the screen.
    fn render(&self) -> Vec<Rgb565Pixel> {
        self.frame();
        self.buffers.borrow()[self.front.get()].clone()
    }

    /// Checks that every row fully in view on the list page has the same
    /// pixels as the list layer's rendering of it (a second `AppWindow` showing one row), so the
    /// hand-over between them does not show.
    fn check_rows(&self, what: &str) {
        let screen = self.render();
        let layout = self.ui.global::<AlarmListLayout>();
        let list = self.ui.global::<AlarmList>();
        let px = |v: f32| v.round() as i32;
        let (x0, y0, w) = (
            px(layout.get_x()),
            px(layout.get_y()),
            px(layout.get_width()),
        );
        let (view_h, pitch) = (px(layout.get_height()), px(layout.get_pitch()));
        let scroll = px(list.get_scroll());
        let rows = list.get_rows();
        let mut checked = 0;
        for i in 0..rows.row_count() {
            let top = i as i32 * pitch - scroll;
            if top < 0 || top + pitch > view_h {
                continue;
            }
            self.row_view.set_list_row(rows.row_data(i).unwrap());
            let mut row = vec![Rgb565Pixel::default(); (w * pitch) as usize];
            self.row_window.request_redraw();
            self.row_window.draw_if_needed(|r| {
                r.render(&mut row, w as usize);
            });
            for y in 0..pitch {
                let s = ((y0 + top + y) * W as i32 + x0) as usize;
                let r = (y * w) as usize;
                if let Some(x) = (0..w as usize).find(|&x| screen[s + x] != row[r + x]) {
                    let n = (0..w as usize)
                        .filter(|&x| screen[s + x] != row[r + x])
                        .count();
                    panic!(
                        "{what}: row {i} differs from the list layer's at line {y}, x {x} \
                         ({n} pixels on the line): {:?} vs {:?}",
                        screen[s + x],
                        row[r + x]
                    );
                }
            }
            checked += 1;
        }
        println!("         {what}: {checked} rows match the list layer's");
    }

    fn shot(&self, name: &str) {
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !name.contains(f.as_str()))
        {
            return;
        }
        let ms = self.frame().unwrap_or(0.0);
        let buf = self.buffers.borrow()[self.front.get()].clone();
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

    let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
    window.set_size(PhysicalSize::new(W as u32, H as u32));
    let row_window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Platform(std::cell::RefCell::new(vec![
        window.clone(),
        row_window.clone(),
    ]))))
    .expect("platform");
    let ui = AppWindow::new().expect("UI");
    ui.show().expect("show");
    let layout = ui.global::<AlarmListLayout>();
    row_window.set_size(PhysicalSize::new(
        layout.get_width() as u32,
        layout.get_pitch() as u32,
    ));
    let row_view = AppWindow::new().expect("row view");
    row_view.set_page(5);
    row_view.show().expect("show row view");
    let p = Preview {
        window,
        ui,
        row_window,
        row_view,
        buffers: std::cell::RefCell::new([
            vec![Rgb565Pixel::default(); W * H],
            vec![Rgb565Pixel::default(); W * H],
        ]),
        front: std::cell::Cell::new(0),
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
    p.check_rows("still list");
    list.on_delete(|id| println!("delete {id}"));
    list.on_open(|id| println!("open {id}"));
    list.on_toggle(|id, on| println!("toggle {id} {on}"));
    // The first press after opening the page: the row lights up in place
    // (it once slid in from the left, from the open-row offset).
    ui.set_page(0);
    p.render();
    ui.set_page(1);
    p.render();
    p.press(600.0, 207.0);
    p.run(130);
    p.shot("1-list-first-press");
    p.window.dispatch_event(WindowEvent::PointerExited);
    p.run(300);
    // Hold a row: it lights up after 40 ms. Then the touch is taken over
    // (a scroll): the page gets a cancel and the highlight fades.
    p.press(600.0, 207.0);
    p.run(160);
    p.shot("1-list-pressed");
    p.window.dispatch_event(WindowEvent::PointerExited);
    p.run(300);
    p.check_rows("after a cancelled press");
    // Hold a switch: the row does not light up (a toggle).
    p.press(1150.0, 369.0);
    p.run(160);
    p.shot("1-list-switch-held");
    p.window.dispatch_event(WindowEvent::PointerExited);
    p.run(300);
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
    // A press that turns into a scroll, as the firmware hands it over: the
    // list layer takes the touch (the page gets a cancel: pointer left),
    // scrolls, and hands the list back where it stopped.
    p.press(600.0, 600.0);
    p.move_to(600.0, 590.0);
    p.run(50);
    p.window.dispatch_event(WindowEvent::PointerExited);
    list.set_scrolling(true);
    p.run(50);
    list.set_scroll(310.0);
    list.set_scrolling(false);
    p.run(300);
    p.shot("1-list-scrolled");
    p.check_rows("scrolled list");
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
