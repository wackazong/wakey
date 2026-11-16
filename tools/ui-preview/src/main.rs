//! Renders the firmware's pages (`firmware/ui/wakey.slint`) on the host,
//! filled with sample data, to PNG files: one per scene. Same Slint, same
//! software renderer, same RGB565 pixels as on the Tab5, so layout, fonts
//! and missing glyphs show up here first.
//!
//! `cargo run --release -- [--by-line] [output dir] [scene name …]`
//!
//! The output dir defaults to `target/ui-preview`. Without scene names every
//! scene is saved; with names only those (exact names such as `1-list`, or a
//! prefix ending in `*` such as `0-clock-sky-*`). `--by-line` renders line
//! by line through a `LineBufferProvider`, as the firmware does, instead of
//! into one buffer (same pixels; the timing then follows the device's code
//! path).
//!
//! Links Slint, which is GPL-3.0, Slint Royalty-free or commercially
//! licensed; this tool is for development only and not distributed.

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, SharedString, VecModel,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use wakey_core::alarm::{self, AlarmConfig};

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
    /// Scenes to save; all if empty.
    names: Vec<String>,
    by_line: bool,
}

impl Preview {
    /// True if scene `name` is to be saved.
    fn wanted(&self, name: &str) -> bool {
        self.names.is_empty()
            || self.names.iter().any(|n| match n.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == n,
            })
    }

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

    /// Renders a frame; returns the screen.
    fn render(&self) -> Vec<Rgb565Pixel> {
        self.frame();
        self.buffers.borrow()[self.front.get()].clone()
    }

    /// Checks that every row fully in view on the list page has the same
    /// pixels as the list layer's rendering of it (a second `AppWindow`
    /// showing one row), so the hand-over between them does not show.
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
        if !self.wanted(name) {
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
        if !self.wanted(name) {
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

fn alarm_cfg(
    id: u32,
    hour: u8,
    minute: u8,
    name: &str,
    weekdays: [u8; 7],
    enabled: bool,
) -> AlarmConfig {
    AlarmConfig {
        id,
        name: name.into(),
        enabled,
        hour,
        minute,
        weekdays,
        ..AlarmConfig::default()
    }
}

fn row(a: &AlarmConfig, next: bool) -> AlarmRow {
    AlarmRow {
        id: a.id as i32,
        time: format!("{:02}:{:02}", a.hour, a.minute).into(),
        name: a.name.as_str().into(),
        days: i32::from(a.weekday_bits()),
        enabled: a.enabled,
        next,
    }
}

fn sound(title: &str, sub: &str, selected: bool, kind: SoundRowKind) -> SoundRow {
    SoundRow {
        title: title.into(),
        sub: sub.into(),
        selected,
        kind,
        progress: -1.0,
        failed: false,
        cover: slint::Image::default(),
    }
}

/// A TIDAL track row with a sample cover (a two-colour gradient).
fn track(title: &str, artist: &str, selected: bool, a: [u8; 3], b: [u8; 3]) -> SoundRow {
    let px = wakey_core::tidal::sync::COVER_PX;
    let mut buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(px, px);
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i as u32 % px, i as u32 / px);
        let f = (x + y) as f32 / (2 * px) as f32;
        let mix = |c0: u8, c1: u8| (f32::from(c0) * (1.0 - f) + f32::from(c1) * f) as u8;
        *p = slint::Rgb8Pixel {
            r: mix(a[0], b[0]),
            g: mix(a[1], b[1]),
            b: mix(a[2], b[2]),
        };
    }
    let mut row = sound(title, artist, selected, SoundRowKind::TidalTrack);
    row.cover = slint::Image::from_rgb8(buf);
    row
}

/// The list card's corner caps, as the firmware makes them
/// (`sound_page::set_corner_caps`, `list_layer::corner_alpha`).
fn set_corner_caps(ui: &AppWindow) {
    let r = ui.global::<SoundListLayout>().get_corner_radius().round() as usize;
    let rf = r as f32;
    let bg = ui.global::<Theme>().get_background();
    let alpha: Vec<u8> = (0..r * r)
        .map(|k| {
            let (x, y) = (k % r, k / r);
            let mut n = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    if (rf - px).powi(2) + (rf - py).powi(2) > rf * rf {
                        n += 1;
                    }
                }
            }
            n
        })
        .collect();
    let cap = |mx: bool, my: bool| {
        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(r as u32, r as u32);
        for (k, p) in buf.make_mut_slice().iter_mut().enumerate() {
            let (x, y) = (k % r, k / r);
            let (i, j) = (
                if mx { r - 1 - x } else { x },
                if my { r - 1 - y } else { y },
            );
            let a = alpha[j * r + i];
            *p = slint::Rgba8Pixel {
                r: bg.red(),
                g: bg.green(),
                b: bg.blue(),
                a: ((u32::from(a) * 255 + 8) / 16) as u8,
            };
        }
        slint::Image::from_rgba8(buf)
    };
    let sp = ui.global::<SoundPicker>();
    sp.set_corner_tl(cap(false, false));
    sp.set_corner_tr(cap(true, false));
    sp.set_corner_bl(cap(false, true));
    sp.set_corner_br(cap(true, true));
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let by_line = args.iter().any(|a| a == "--by-line");
    args.retain(|a| a != "--by-line");
    let mut args = args.into_iter();
    let out = PathBuf::from(args.next().unwrap_or_else(|| "target/ui-preview".into()));
    let names: Vec<String> = args.collect();
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
    row_view.set_page(Page::AlarmListRow);
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
        names,
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
    ui.set_page(Page::Clock);
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
    let alarms = [
        alarm_cfg(1, 6, 30, "Arbeit", [1, 1, 0, 0, 0, 0, 0], true),
        alarm_cfg(2, 7, 15, "", [0, 0, 1, 1, 1, 0, 0], true),
        alarm_cfg(3, 16, 45, "Yoga", [1, 0, 0, 0, 0, 0, 0], true),
        alarm_cfg(4, 8, 30, "Wochenende", [0, 0, 0, 0, 0, 1, 1], false),
        alarm_cfg(5, 22, 0, "Medikamente", [1; 7], true),
    ];
    // In the list's order, as the firmware shows it.
    let rows: Vec<AlarmRow> = alarm::sorted(&alarms)
        .iter()
        .map(|a| row(a, a.id == 1))
        .collect();
    list.set_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_page(Page::AlarmList);
    p.shot("1-list");
    p.check_rows("still list");
    list.on_delete(|id| println!("delete {id}"));
    list.on_open(|id| println!("open {id}"));
    list.on_toggle(|id, on| println!("toggle {id} {on}"));
    // The first press after opening the page: the row lights up in place.
    ui.set_page(Page::Clock);
    p.render();
    ui.set_page(Page::AlarmList);
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
    ui.set_page(Page::Clock);
    list.set_rows(ModelRc::new(VecModel::from(Vec::<AlarmRow>::new())));
    ui.set_page(Page::AlarmList);
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
    ui.set_page(Page::Alarm);
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
    ui.set_page(Page::Clock);
    ed.set_name("".into());
    ed.set_hour(7);
    ed.set_minute(0);
    ed.set_days(days([0; 7]));
    ed.set_any_day(false);
    ed.set_sound("Eingebauter Ton".into());
    ui.set_page(Page::Alarm);
    p.shot("2-alarm-new-no-days");

    // Sound page.
    let sp = ui.global::<SoundPicker>();
    set_corner_caps(ui);
    sp.set_rows(ModelRc::new(VecModel::from(vec![
        sound(
            "Zufällig",
            "wechselt jeden Tag",
            false,
            SoundRowKind::Random,
        ),
        sound("Morgenlicht", "", true, SoundRowKind::File),
        sound("Vogelstimmen", "", false, SoundRowKind::File),
        sound("Kirchenglocken", "", false, SoundRowKind::File),
        sound("Meeresrauschen", "", false, SoundRowKind::File),
    ])));
    sp.set_volume(60);
    sp.set_can_save(true);
    sp.set_fade(1);
    ui.set_page(Page::Sound);
    p.shot("3-sound");
    sp.set_previewing(true);
    p.shot("3-sound-previewing");
    sp.set_previewing(false);
    // The tab bar's highlight dragged from „Datei“ towards „TIDAL“.
    p.press(205.0, 176.0);
    p.move_to(290.0, 176.0);
    p.move_to(380.0, 176.0);
    p.shot("3-sound-tab-dragging");
    p.release(380.0, 176.0);
    // Back to „Datei“ (nothing here switches the tab), animation done.
    std::thread::sleep(Duration::from_millis(300));
    // No sound files: only the note (the built-in tone is no choice).
    sp.set_rows(ModelRc::new(VecModel::from(Vec::<SoundRow>::new())));
    sp.set_empty("Keine Klänge auf der Speicherkarte (Ordner „sounds“).".into());
    p.shot("3-sound-no-files");
    sp.set_tab(SoundTab::Tidal);
    sp.set_shown_tab(SoundTab::Tidal);
    sp.set_tidal_rows(ModelRc::new(VecModel::from(Vec::<SoundRow>::new())));
    sp.set_tidal_empty("TIDAL ist noch nicht eingerichtet.".into());
    p.shot("3-sound-tidal");
    // TIDAL tracks: one selected, one downloading, one selected and
    // downloading (the selection must show next to the progress bar).
    let mut loading = track(
        "Paper Lanterns",
        "Juno Hale · wird geladen",
        false,
        [40, 40, 40],
        [200, 60, 50],
    );
    loading.progress = 0.45;
    let mut both = sound(
        "Slow Sunrise",
        "Ada Lindqvist · wird geladen",
        true,
        SoundRowKind::TidalTrack,
    );
    both.progress = 0.7;
    let mut failed_row = track(
        "Morning Tide",
        "Nicht mehr in der Playlist · Elin Moor",
        false,
        [120, 90, 60],
        [30, 50, 40],
    );
    failed_row.failed = true;
    sp.set_tidal_empty("".into());
    sp.set_tidal_rows(ModelRc::new(VecModel::from(vec![
        sound(
            "Zufällig",
            "wechselt jeden Tag",
            false,
            SoundRowKind::Random,
        ),
        track(
            "Kaffee am Fenster",
            "Die Frühaufsteher",
            true,
            [240, 200, 80],
            [200, 90, 40],
        ),
        failed_row,
        loading,
        both,
        track(
            "Golden Fields",
            "Nicht mehr in der Playlist · The Harbour Lights",
            false,
            [250, 170, 60],
            [90, 40, 120],
        ),
    ])));
    p.shot("3-sound-tidal-list");
    // The alarm's track failed to download: still selected (the alarm rings
    // the built-in tone), shown failed.
    let mut picked_failed = track(
        "Morning Tide",
        "Elin Moor",
        true,
        [120, 90, 60],
        [30, 50, 40],
    );
    picked_failed.failed = true;
    sp.set_tidal_rows(ModelRc::new(VecModel::from(vec![
        sound(
            "Zufällig",
            "wechselt jeden Tag",
            false,
            SoundRowKind::Random,
        ),
        track(
            "Kaffee am Fenster",
            "Die Frühaufsteher",
            false,
            [240, 200, 80],
            [200, 90, 40],
        ),
        picked_failed,
        track(
            "Golden Fields",
            "The Harbour Lights",
            false,
            [250, 170, 60],
            [90, 40, 120],
        ),
    ])));
    p.shot("3-sound-tidal-picked-failed");
    // The selected row at the top of the card: its corners stay round.
    sp.set_tidal_rows(ModelRc::new(VecModel::from(vec![
        track(
            "Kaffee am Fenster",
            "Die Frühaufsteher",
            true,
            [240, 200, 80],
            [200, 90, 40],
        ),
        track(
            "Golden Fields",
            "The Harbour Lights",
            false,
            [250, 170, 60],
            [90, 40, 120],
        ),
    ])));
    p.shot("3-sound-tidal-corners");

    // Name.
    let ne = ui.global::<NameEditor>();
    ne.set_text("Arbeit".into());
    ui.set_page(Page::Name);
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
    ui.set_page(Page::Alarm);
    p.shot("2-alarm-umlauts");
    clock.set_alarm("40px: ÜÄÖ üäö".into());
    clock.set_date("40px: ÜÄÖ üäö".into());
    ui.set_page(Page::Clock);
    p.shot("0-clock-umlauts");
}
