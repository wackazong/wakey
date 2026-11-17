//! Renders the firmware's pages (`firmware/ui/wakey.slint`) on the host,
//! filled with sample data, to PNG files: one per scene. Same Slint, same
//! software renderer, same RGB565 pixels as on the Tab5, so layout, fonts
//! and missing glyphs show up here first.
//!
//! `cargo run --release -- [--by-line] [--language=de] [output dir] [scene name …]`
//!
//! The texts are English or those of `--language` (a code of
//! `wakey-core/lang/`). The output dir defaults to `target/ui-preview`. Without scene names every
//! scene is saved; with names only those (exact names such as `1-list`, or a
//! prefix ending in `*` such as `0-clock-sky-*`). `--by-line` renders line
//! by line through a `LineBufferProvider`, as the firmware does, instead of
//! into one buffer (the timing then follows the device's code path; the
//! pictures are the same but for a few edge pixels of the icon on the
//! clock's sky).
//!
//! Slint's time is the preview's own, a 16 ms step per frame, so a scene
//! caught in the middle of an animation is the same picture on every run.
//!
//! Links Slint, which is GPL-3.0, Slint Royalty-free or commercially
//! licensed; this tool is for development only and not distributed.

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, SharedString, VecModel,
};
use std::cell::{Cell, RefCell};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use wakey_core::alarm::{self, AlarmConfig};
use wakey_core::datetime::DateTime;
use wakey_core::lang::{Language, Texts, fill};
use wakey_core::sky;

slint::include_modules!();

const W: usize = 1280;
const H: usize = 720;

/// One step of the preview's clock: a frame at 60 fps.
const STEP: Duration = Duration::from_millis(16);

/// Hands out the prepared windows in order: the main window, then the
/// alarm list's row window (as the firmware's `ui::platform`). Slint's time
/// is the preview's clock, not the host's: a scene shows the same frame of
/// an animation on every run.
struct Platform {
    windows: RefCell<Vec<Rc<MinimalSoftwareWindow>>>,
    clock: Rc<Cell<Duration>>,
}

impl slint::platform::Platform for Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        let mut w = self.windows.borrow_mut();
        if w.is_empty() {
            return Err(slint::PlatformError::Other("no window prepared".into()));
        }
        Ok(w.remove(0))
    }

    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }
}

struct Preview {
    /// Renders like the firmware: only what changed, into two buffers in
    /// turn (`RepaintBufferType::SwappedBuffers`), once per [`STEP`].
    window: Rc<MinimalSoftwareWindow>,
    buffers: RefCell<[Vec<Rgb565Pixel>; 2]>,
    /// The buffer rendered last (what the panel would show).
    front: Cell<usize>,
    /// Slint's time (see [`Platform`]).
    clock: Rc<Cell<Duration>>,
    ui: AppWindow,
    /// The alarm list's row as the list layer renders it.
    row_window: Rc<MinimalSoftwareWindow>,
    row_view: AppWindow,
    texts: Texts,
    out: PathBuf,
    /// Scenes to save; all if empty.
    names: Vec<String>,
    by_line: bool,
}

impl Preview {
    /// Sets up Slint with the main window and the row view, as the firmware
    /// does.
    fn new(texts: Texts, out: PathBuf, names: Vec<String>, by_line: bool) -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
        window.set_size(PhysicalSize::new(W as u32, H as u32));
        let row_window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clock = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(Platform {
            windows: RefCell::new(vec![window.clone(), row_window.clone()]),
            clock: clock.clone(),
        }))
        .expect("platform");
        let ui = AppWindow::new().expect("UI");
        set_texts(&ui, &texts);
        ui.show().expect("show");
        let layout = ui.global::<AlarmListLayout>();
        row_window.set_size(PhysicalSize::new(
            layout.get_width() as u32,
            layout.get_pitch() as u32,
        ));
        let row_view = AppWindow::new().expect("row view");
        set_texts(&row_view, &texts);
        row_view.set_page(Page::AlarmListRow);
        row_view.show().expect("show row view");
        Self {
            window,
            buffers: RefCell::new([
                vec![Rgb565Pixel::default(); W * H],
                vec![Rgb565Pixel::default(); W * H],
            ]),
            front: Cell::new(0),
            clock,
            ui,
            row_window,
            row_view,
            texts,
            out,
            names,
            by_line,
        }
    }

    /// True if scene `name` is to be saved.
    fn wanted(&self, name: &str) -> bool {
        self.names.is_empty()
            || self.names.iter().any(|n| match n.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == n,
            })
    }

    /// Moves Slint's time on by `by`, without a frame.
    fn advance(&self, by: Duration) {
        self.clock.set(self.clock.get() + by);
    }

    /// Lets timers and animations run for `ms`, a frame every [`STEP`].
    fn run(&self, ms: u64) {
        for _ in 0..ms / STEP.as_millis() as u64 {
            self.advance(STEP);
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
        let t = Instant::now();
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
        self.cancel();
    }

    /// The pointer leaves the window: what the page gets when the list
    /// layer takes a touch over.
    fn cancel(&self) {
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
            let row = self.render_row(rows.row_data(i).unwrap(), 0.0, false, false);
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
        let path = self.save(name, &screen);
        println!("         {}", path.display());
    }

    /// Renders until the running animations (e.g. the tab bar's slide) have
    /// ended, at most 2 s, so the next shot shows the settled page.
    fn settle(&self) {
        let end = self.clock.get() + Duration::from_secs(2);
        loop {
            self.frame();
            if !self.window.has_active_animations() || self.clock.get() >= end {
                break;
            }
            self.advance(STEP);
        }
    }

    /// Saves the screen as scene `name`, if wanted, with its render time.
    fn shot(&self, name: &str) {
        if !self.wanted(name) {
            return;
        }
        let ms = self.frame().unwrap_or(0.0);
        let path = self.save(name, &self.buffers.borrow()[self.front.get()]);
        println!("{ms:6.2} ms {}", path.display());
    }

    fn save(&self, name: &str, screen: &[Rgb565Pixel]) -> PathBuf {
        let path = self.out.join(format!("{name}.png"));
        write_png(&path, screen);
        path
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
/// (`sound_page::rows::set_corner_caps`, `list_layer::corner_alpha`).
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
    let picker = ui.global::<SoundPicker>();
    picker.set_corner_tl(cap(false, false));
    picker.set_corner_tr(cap(true, false));
    picker.set_corner_bl(cap(false, true));
    picker.set_corner_br(cap(true, true));
}

/// Gives `window` the fixed texts, as the firmware does (`app/texts.rs`).
fn set_texts(window: &AppWindow, texts: &Texts) {
    fn strings(texts: &[String]) -> ModelRc<SharedString> {
        let texts: Vec<SharedString> = texts.iter().map(SharedString::from).collect();
        ModelRc::new(VecModel::from(texts))
    }
    fn rows(rows: &[Vec<String>]) -> ModelRc<ModelRc<SharedString>> {
        let rows: Vec<_> = rows.iter().map(|row| strings(row)).collect();
        ModelRc::new(VecModel::from(rows))
    }
    let lang = window.global::<Lang>();
    lang.set_labels(wakey_core::slint_labels!(Labels, texts.labels));
    lang.set_weekdays(strings(&texts.weekdays_short));
    let (lower, upper) = texts.keyboard_rows();
    lang.set_keys_lower(rows(&lower));
    lang.set_keys_upper(rows(&upper));
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let by_line = args.iter().any(|a| a == "--by-line");
    let language = args
        .iter()
        .find_map(|a| a.strip_prefix("--language="))
        .map_or(Language::default(), |code| {
            Language::from_code(code).expect("a language of wakey-core/lang")
        });
    args.retain(|a| !a.starts_with("--"));
    let mut args = args.into_iter();
    let out = PathBuf::from(args.next().unwrap_or_else(|| "target/ui-preview".into()));
    let names: Vec<String> = args.collect();
    std::fs::create_dir_all(&out).expect("output dir");

    let p = Preview::new(language.texts(), out, names, by_line);
    clock_scenes(&p);
    alarm_list_scenes(&p);
    alarm_page_scenes(&p);
    sound_page_scenes(&p);
    name_page_scenes(&p);
    umlaut_scenes(&p);
}

/// A text as the clock takes it: one model entry per character.
fn chars(s: &str) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        s.chars()
            .map(|c| SharedString::from(c.to_string()))
            .collect::<Vec<_>>(),
    ))
}

fn clock_scenes(p: &Preview) {
    let ui = &p.ui;
    let clock = ui.global::<Clock>();
    let time = chars("06:12");
    clock.set_time(time.clone());
    clock.set_seconds(chars("41"));
    let texts = &p.texts;
    let tuesday = DateTime::from_unix(1_794_290_400); // 2026-11-10
    clock.set_date(texts.date(&tuesday).into());
    let next = [
        ("day", &texts.clock.tomorrow as &dyn Display),
        ("time", &"06:30"),
    ];
    clock.set_alarm(fill(&texts.clock.next_alarm, &next).into());
    ui.set_page(Page::Clock);
    p.run(500);
    p.shot("0-clock");
    // The minute changes: its last digit rolls (old up and out, new up
    // from below), the date as a whole.
    time.set_row_data(4, "3".into());
    clock.set_date(texts.date(&tuesday.add_days(1)).into());
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
    // Ringing: the stop button in the alarm line's place, then held half
    // of its 1.5 s.
    clock.set_alarm_phase(AlarmPhase::Ringing);
    clock.set_snoozes_left(2);
    clock.set_snooze_hint(fill(&texts.clock.snooze_hint, &[("n", &2)]).into());
    p.run(500);
    p.shot("0-clock-ringing");
    p.press(640.0, 652.0);
    p.run(750);
    p.shot("0-clock-stop-held");
    p.release(640.0, 652.0);
    clock.set_alarm_phase(AlarmPhase::Idle);
    p.run(500);
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
        let unix = DateTime {
            year: 2026,
            month: 6,
            day: 21,
            hour: h,
            minute: m,
            second: 0,
            weekday: 0,
        }
        .to_unix();
        let zurich = sky::Location::default();
        let elevation = sky::sun_elevation(unix, zurich);
        let gradient = sky::gradient(elevation, sky::sun_rising(unix, zurich));
        let [top, upper, middle, lower, bottom] =
            gradient.map(|c| slint::Color::from_rgb_u8(c.0, c.1, c.2));
        clock.set_sky_top(top);
        clock.set_sky_upper(upper);
        clock.set_sky_middle(middle);
        clock.set_sky_lower(lower);
        clock.set_sky_bottom(bottom);
        clock.set_bottom_light(sky::bottom_is_light(&gradient));
        clock.set_time(chars(time));
        p.run(500);
        println!("         sky {name}: sun at {elevation:.1}°");
        p.shot(&format!("0-clock-sky-{name}"));
    }
}

fn alarm_list_scenes(p: &Preview) {
    let ui = &p.ui;
    let list = ui.global::<AlarmList>();
    let alarms = [
        alarm_cfg(1, 6, 30, "Work", [1, 1, 0, 0, 0, 0, 0], true),
        alarm_cfg(2, 7, 15, "", [0, 0, 1, 1, 1, 0, 0], true),
        alarm_cfg(3, 16, 45, "Yoga", [1, 0, 0, 0, 0, 0, 0], true),
        alarm_cfg(4, 8, 30, "Weekend", [0, 0, 0, 0, 0, 1, 1], false),
        alarm_cfg(5, 22, 0, "Medication", [1; 7], true),
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
    p.cancel();
    p.run(300);
    // Hold a row: it lights up after 40 ms. Then the touch is taken over
    // (a scroll): the page gets a cancel and the highlight fades.
    p.press(600.0, 207.0);
    p.run(160);
    p.shot("1-list-pressed");
    p.cancel();
    p.run(300);
    p.check_rows("after a cancelled press");
    // Hold a switch: the row does not light up (a toggle).
    p.press(1150.0, 369.0);
    p.run(160);
    p.shot("1-list-switch-held");
    p.cancel();
    p.run(300);
    // The second row halfway to deleting, as the list layer draws it.
    p.shot_swipe("1-list-swiped", 1, 500);
    // A press that turns into a scroll, as the firmware hands it over: the
    // list layer takes the touch (the page gets a cancel), scrolls, and
    // hands the list back where it stopped.
    p.press(600.0, 600.0);
    p.move_to(600.0, 590.0);
    p.run(50);
    p.cancel();
    list.set_layer_active(true);
    p.run(50);
    list.set_scroll(310.0);
    list.set_layer_active(false);
    p.run(300);
    p.shot("1-list-scrolled");
    p.check_rows("scrolled list");
    // Tap a switch: `toggle` is printed.
    p.press(1150.0, 207.0);
    p.release(1150.0, 207.0);
    p.run(50);
    ui.set_page(Page::Clock);
    list.set_rows(ModelRc::new(VecModel::from(Vec::<AlarmRow>::new())));
    ui.set_page(Page::AlarmList);
    p.shot("1-list-empty");
}

fn alarm_page_scenes(p: &Preview) {
    let ui = &p.ui;
    let editor = ui.global::<AlarmEditor>();
    editor.set_hour(16);
    editor.set_minute(45);
    editor.set_name("Yoga".into());
    editor.set_days(days([1, 0, 0, 0, 0, 0, 0]));
    editor.set_any_day(true);
    let texts = &p.texts;
    let rings_in = texts.duration(6 * 1440 + 4 * 60 + 33);
    editor.set_relative(fill(&texts.alarm.rings_in, &[("time", &rings_in)]).into());
    editor.set_sound("Morning light".into());
    editor.set_volume_bars(3);
    editor.set_fade("10 s".into());
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
    // A new alarm. The page is left for a frame, as on the device: a page
    // still shown would keep its wheels where they were dragged to.
    ui.set_page(Page::Clock);
    p.render();
    editor.set_name("".into());
    editor.set_hour(7);
    editor.set_minute(0);
    editor.set_days(days([0; 7]));
    editor.set_any_day(false);
    editor.set_sound(texts.sound.built_in.as_str().into());
    ui.set_page(Page::Alarm);
    p.shot("2-alarm-new-no-days");
}

fn sound_page_scenes(p: &Preview) {
    let ui = &p.ui;
    let picker = ui.global::<SoundPicker>();
    let texts = &p.texts;
    let random = || {
        sound(
            &texts.sound.random,
            &texts.sound.random_note,
            false,
            SoundRowKind::Random,
        )
    };
    set_corner_caps(ui);
    picker.set_rows(ModelRc::new(VecModel::from(vec![
        random(),
        sound("Morning light", "", true, SoundRowKind::File),
        sound("Birdsong", "", false, SoundRowKind::File),
        sound("Church bells", "", false, SoundRowKind::File),
        sound("Ocean waves", "", false, SoundRowKind::File),
    ])));
    picker.set_volume(60);
    picker.set_can_save(true);
    picker.set_fade(1);
    ui.set_page(Page::Sound);
    p.shot("3-sound");
    picker.set_previewing(true);
    p.shot("3-sound-previewing");
    picker.set_previewing(false);
    // The tab bar's highlight dragged from the files towards TIDAL.
    p.press(205.0, 176.0);
    p.move_to(290.0, 176.0);
    p.move_to(380.0, 176.0);
    p.shot("3-sound-tab-dragging");
    p.release(380.0, 176.0);
    // Back to the files (nothing here switches the tab), animation done.
    p.advance(Duration::from_millis(300));
    // No sound files: only AirDrop's note (the built-in tone is no choice).
    picker.set_rows(ModelRc::new(VecModel::from(vec![sound(
        &texts.airdrop.ready,
        "",
        false,
        SoundRowKind::AirdropNote,
    )])));
    p.shot("3-sound-no-files");
    picker.set_tab(SoundTab::Tidal);
    picker.set_shown_tab(SoundTab::Tidal);
    picker.set_tidal_rows(ModelRc::new(VecModel::from(Vec::<SoundRow>::new())));
    picker.set_tidal_empty(texts.tidal.no_credentials.as_str().into());
    p.settle();
    p.shot("3-sound-tidal");

    let coffee = |selected| {
        track(
            "Coffee by the Window",
            "The Early Risers",
            selected,
            [240, 200, 80],
            [200, 90, 40],
        )
    };
    let queued = |artist: &str| fill(&texts.tidal.queued, &[("artist", &artist)]);
    let removed = |artist: &str| fill(&texts.tidal.removed, &[("artist", &artist)]);
    let golden = |sub: &str| track("Golden Fields", sub, false, [250, 170, 60], [90, 40, 120]);
    let failed = |sub: &str, selected| {
        let mut row = track("Morning Tide", sub, selected, [120, 90, 60], [30, 50, 40]);
        row.failed = true;
        row
    };
    // TIDAL tracks: one selected, one failed, one downloading, one selected
    // and downloading (the selection must show next to the progress bar).
    let mut loading = track(
        "Paper Lanterns",
        &queued("Juno Hale"),
        false,
        [40, 40, 40],
        [200, 60, 50],
    );
    loading.progress = 0.45;
    let mut both = sound(
        "Slow Sunrise",
        &queued("Ada Lindqvist"),
        true,
        SoundRowKind::TidalTrack,
    );
    both.progress = 0.7;
    picker.set_tidal_empty("".into());
    picker.set_tidal_rows(ModelRc::new(VecModel::from(vec![
        random(),
        coffee(true),
        failed(&removed("Elin Moor"), false),
        loading,
        both,
        golden(&removed("The Harbour Lights")),
    ])));
    // The scrolling second lines at their start, as when the tab opens.
    picker.set_marquee_ms(0);
    p.shot("3-sound-tidal-list");
    // The alarm's track failed to download: still selected (the alarm rings
    // the built-in tone), shown failed.
    picker.set_tidal_rows(ModelRc::new(VecModel::from(vec![
        random(),
        coffee(false),
        failed("Elin Moor", true),
        golden("The Harbour Lights"),
    ])));
    p.shot("3-sound-tidal-picked-failed");
    // The selected row at the top of the card: its corners stay round.
    picker.set_tidal_rows(ModelRc::new(VecModel::from(vec![
        coffee(true),
        golden("The Harbour Lights"),
    ])));
    p.shot("3-sound-tidal-corners");
}

fn name_page_scenes(p: &Preview) {
    let ui = &p.ui;
    let editor = ui.global::<NameEditor>();
    editor.set_text("Work".into());
    ui.set_page(Page::Name);
    p.shot("4-name");
    editor.set_text("".into());
    editor.set_shift(true);
    p.shot("4-name-empty-shift");
    editor.set_numbers(true);
    p.shot("4-name-numbers");
    editor.set_numbers(false);
    editor.set_text("Übung Ärger Öl äöü ß".into());
    p.shot("4-name-umlauts");
}

/// Umlauts at 40 px on the other pages: a glyph not embedded shows here.
fn umlaut_scenes(p: &Preview) {
    let ui = &p.ui;
    ui.global::<AlarmEditor>().set_name("Übung Ärger Öl".into());
    ui.set_page(Page::Alarm);
    p.shot("2-alarm-umlauts");
    let clock = ui.global::<Clock>();
    clock.set_alarm("40px: ÜÄÖ üäö".into());
    clock.set_date("40px: ÜÄÖ üäö".into());
    ui.set_page(Page::Clock);
    p.shot("0-clock-umlauts");
}
