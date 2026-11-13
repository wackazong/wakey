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
use slint::{
    ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, SharedString, VecModel,
};
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

    /// Renders the offscreen row view (the list layer's pictures).
    fn render_row(
        &self,
        alarm: AlarmRow,
        dx: f32,
        highlighted: bool,
        underlay: bool,
    ) -> Vec<Rgb565Pixel> {
        let layout = self.ui.global::<AlarmListLayout>();
        let (w, h) = (layout.get_width() as usize, layout.get_pitch() as usize);
        self.row_view.set_list_row(alarm);
        self.row_view.set_list_row_dx(dx);
        self.row_view.set_list_row_highlighted(highlighted);
        self.row_view.set_list_row_underlay(underlay);
        let mut row = vec![Rgb565Pixel::default(); w * h];
        self.row_window.request_redraw();
        self.row_window.draw_if_needed(|r| {
            r.render(&mut row, w);
        });
        self.row_view.set_list_row_dx(0.0);
        self.row_view.set_list_row_underlay(false);
        row
    }

    /// Saves the screen with row `i` as the firmware's list layer shows it
    /// swiped `d` px left: its content picture (corners on red) moved left,
    /// the delete button picture in the gap.
    fn shot_swipe(&self, name: &str, i: usize, d: usize) {
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !name.contains(f.as_str()))
        {
            return;
        }
        let mut screen = self.render();
        let layout = self.ui.global::<AlarmListLayout>();
        let list = self.ui.global::<AlarmList>();
        let px = |v: f32| v.round() as usize;
        let (x0, y0, w) = (
            px(layout.get_x()),
            px(layout.get_y()),
            px(layout.get_width()),
        );
        let pitch = px(layout.get_pitch());
        let top = y0 + i * pitch - px(list.get_scroll());
        let alarm = list.get_rows().row_data(i).unwrap();
        let content = self.render_row(alarm.clone(), 0.0, true, true);
        let underlay = self.render_row(alarm, -10_000.0, false, false);
        for y in 0..pitch {
            for x in 0..w {
                screen[(top + y) * W + x0 + x] = if x < w - d {
                    content[y * w + x + d]
                } else {
                    underlay[y * w + x]
                };
            }
        }
        let path = self.out.join(format!("{name}.png"));
        write_png(&path, &screen);
        println!("         {}", path.display());
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
        progress: -1.0,
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
    let chars = |s: &str| {
        ModelRc::new(VecModel::from(
            s.chars()
                .map(|c| SharedString::from(c.to_string()))
                .collect::<Vec<_>>(),
        ))
    };
    let time = chars("06:12");
    clock.set_time(time.clone());
    clock.set_seconds(chars("41"));
    clock.set_date("Dienstag, 10. November 2026".into());
    clock.set_alarm("Morgen 06:30".into());
    ui.set_page(0);
    p.run(500);
    p.shot("0-clock");
    // The minute changes: its last digit rolls (old up and out, new up
    // from below), the date as a whole.
    time.set_row_data(4, "3".into());
    clock.set_date("Mittwoch, 11. November 2026".into());
    p.run(160);
    p.shot("0-clock-rolling");
    p.run(400);
    clock.set_no_speaker(true);
    clock.set_no_network(true);
    clock.set_time(chars("10:11"));
    p.run(500);
    p.shot("0-clock-missing");
    clock.set_no_speaker(false);
    clock.set_no_network(false);
    // The sky through 21 June in Zurich (UTC times; CEST is +2 h).
    for (name, h, m, time) in [
        ("night", 23, 0, "01:00"),
        ("dawn", 2, 40, "04:40"),
        ("twilight", 3, 10, "05:10"),
        ("sunrise", 3, 35, "05:35"),
        ("early", 4, 30, "06:30"),
        ("morning", 6, 0, "08:00"),
        ("noon", 11, 25, "13:25"),
        ("evening", 17, 30, "19:30"),
        ("sunset", 19, 20, "21:20"),
        ("dusk", 20, 0, "22:00"),
        ("late", 20, 40, "22:40"),
    ] {
        use wakey_core::sky;
        let unix = wakey_core::datetime::DateTime {
            year: 2026,
            month: 6,
            day: 21,
            hour: h,
            minute: m,
            second: 0,
            weekday: 0,
        }
        .to_unix();
        let e = sky::sun_elevation(unix, sky::LATITUDE, sky::LONGITUDE);
        let g = sky::gradient(e, sky::sun_rising(unix, sky::LATITUDE, sky::LONGITUDE));
        let [a, b, c, d, f] = g.map(|c| slint::Color::from_rgb_u8(c.0, c.1, c.2));
        clock.set_sky_top(a);
        clock.set_sky_upper(b);
        clock.set_sky_middle(c);
        clock.set_sky_lower(d);
        clock.set_sky_bottom(f);
        clock.set_bottom_light(sky::bottom_is_light(&g));
        clock.set_time(chars(time));
        p.run(500);
        println!("         sky {name}: sun at {e:.1}°");
        p.shot(&format!("0-clock-sky-{name}"));
    }

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
    // The second row halfway to deleting, as the list layer draws it.
    p.shot_swipe("1-list-swiped", 1, 500);
    // A press that turns into a scroll, as the firmware hands it over: the
    // list layer takes the touch (the page gets a cancel: pointer left),
    // scrolls, and hands the list back where it stopped.
    p.press(600.0, 600.0);
    p.move_to(600.0, 590.0);
    p.run(50);
    p.window.dispatch_event(WindowEvent::PointerExited);
    list.set_layer_active(true);
    p.run(50);
    list.set_scroll(310.0);
    list.set_layer_active(false);
    p.run(300);
    p.shot("1-list-scrolled");
    p.check_rows("scrolled list");
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
    // No sound files: the built-in tone and a note, no "Zufällig".
    sp.set_rows(ModelRc::new(VecModel::from(vec![sound(
        "Eingebauter Ton",
        "Piepen",
        true,
        0,
    )])));
    sp.set_empty("Keine Klänge auf der Speicherkarte (Ordner „sounds“).".into());
    p.shot("3-sound-no-files");
    sp.set_tab(1);
    sp.set_rows(ModelRc::new(VecModel::from(Vec::<SoundRow>::new())));
    sp.set_empty("TIDAL ist noch nicht eingerichtet.".into());
    p.shot("3-sound-tidal");
    // TIDAL tracks: one selected, one downloading, one selected and
    // downloading (the selection must show next to the progress bar).
    let mut loading = sound("Point and Kill", "Little Simz · wird geladen", false, 2);
    loading.progress = 0.45;
    let mut both = sound("Sunrise", "Norah Jones · wird geladen", true, 2);
    both.progress = 0.7;
    sp.set_empty("".into());
    sp.set_rows(ModelRc::new(VecModel::from(vec![
        sound("Zufällig", "wechselt jeden Tag", false, 1),
        sound("Here Comes the Sun", "The Beatles", true, 2),
        loading,
        both,
        sound("Golden Hour", "JVKE", false, 2),
        sound("Morning Song", "The Lumineers · wird geladen", false, 2),
    ])));
    p.shot("3-sound-tidal-list");

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
    clock.set_alarm("40px: ÜÄÖ üäö".into());
    clock.set_date("40px: ÜÄÖ üäö".into());
    ui.set_page(0);
    p.shot("0-clock-umlauts");
}
