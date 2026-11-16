//! Page transitions: the new page slides in (sideways, or up from the
//! bottom for the alarm list over the clock), pushing the old one out,
//! composed by the 2D-DMA. Slint renders a page change in 40–60 ms, too slow
//! to animate it frame by frame; instead the new page is rendered once into
//! a picture of its own and the two pictures are copied into the frame
//! buffer at moving offsets (~14 ms of DMA per frame, the panel's rate).
//!
//! The slide starts at once, while Slint still renders the new page: the
//! frames are driven from the renderer's line callback ([`Slide::poll`],
//! every ~70 µs; the DMA does the copying, the CPU only starts copies and
//! flips). Until the new page is rendered, its part of the screen shows the
//! background colour; the slide starts slowly (ease in and out), so by then
//! only the page's margin has come in.
//!
//! The app calls [`request`] right before it switches the page; the render
//! loop then runs [`Transitions::run`] instead of its next frame:
//! 1. the screen as shown is copied into the "old" picture (the first DMA
//!    job, before any frame);
//! 2. Slint renders its dirty region (the new page) into "new" through the
//!    PPA while the slide runs; anything outside that region is copied
//!    from "old";
//! 3. after [`DURATION`] both frame buffers get "new", so they hold what
//!    Slint believes it drew.
//!
//! Touches during the transition are dropped.

use super::input;
use super::platform::PpaLines;
use crate::display::cache;
use crate::display::dma2d::{Blit, FbCopy, OwnedPicture, Picture};
use crate::display::ppa::{PpaRotator, Target};
use crate::display::{Display, HEIGHT, WIDTH};
use core::sync::atomic::{AtomicU8, AtomicU16, Ordering};
use embassy_time::{Duration, Instant};
use slint::platform::software_renderer::{
    LineBufferProvider, MinimalSoftwareWindow, RenderingRotation, Rgb565Pixel,
};
use std::collections::VecDeque;

/// Length of the slide.
const DURATION: Duration = Duration::from_millis(300);
/// Landscape width (sideways slides) and height (vertical slides) of the
/// background pictures shown for the part of the new page not rendered yet;
/// the slide waits rather than show more.
const BLANK_W: usize = 256;
const BLANK_H: usize = 160;

/// Where the new page comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Deeper (list → alarm → sound): from the right.
    Forward,
    /// Back up: from the left.
    Back,
    /// Up from the bottom (the alarm list over the clock).
    Up,
    /// Down, uncovering the page below (back to the clock).
    Down,
}

/// 0 none, 1 forward, 2 back, 3 up, 4 down.
static PENDING: AtomicU8 = AtomicU8::new(0);
/// The pages' background colour (RGB565), for the not yet rendered part.
static BACKGROUND: AtomicU16 = AtomicU16::new(0);

/// The page is about to change: animate it. Call from the UI thread, right
/// before the change.
pub fn request(direction: Direction) {
    let v = match direction {
        Direction::Forward => 1,
        Direction::Back => 2,
        Direction::Up => 3,
        Direction::Down => 4,
    };
    PENDING.store(v, Ordering::Relaxed);
}

/// The new page's background colour (RGB565), set before each request.
pub fn set_background(rgb565: u16) {
    BACKGROUND.store(rgb565, Ordering::Relaxed);
}

/// A transition is requested and not taken yet.
pub(super) fn pending() -> bool {
    PENDING.load(Ordering::Relaxed) != 0
}

/// The transition requested since the last call, if any.
pub(super) fn take() -> Option<Direction> {
    match PENDING.swap(0, Ordering::Relaxed) {
        1 => Some(Direction::Forward),
        2 => Some(Direction::Back),
        3 => Some(Direction::Up),
        4 => Some(Direction::Down),
        _ => None,
    }
}

/// The pictures and the DMA for transitions.
pub(super) struct Transitions {
    old: OwnedPicture,
    new: OwnedPicture,
    /// Background colour: `BLANK_W` landscape columns (panel rows), and
    /// `BLANK_H` landscape rows (panel columns).
    blank: OwnedPicture,
    blank_v: OwnedPicture,
    blank_colour: Option<u16>,
    dma: FbCopy,
}

/// A picture of `cols × rows` (panel orientation) in PSRAM.
fn alloc(cols: usize, rows: usize) -> OwnedPicture {
    OwnedPicture::new(cols, rows).expect("no PSRAM for page transitions")
}

impl Transitions {
    pub fn new() -> Self {
        Self {
            old: alloc(WIDTH, HEIGHT),
            new: alloc(WIDTH, HEIGHT),
            blank: alloc(WIDTH, BLANK_W),
            blank_v: alloc(BLANK_H, HEIGHT),
            blank_colour: None,
            dma: FbCopy::new().expect("2D-DMA"),
        }
    }

    /// Shows the page change Slint has pending as a slide.
    pub async fn run(
        &mut self,
        direction: Direction,
        window: &MinimalSoftwareWindow,
        display: &mut Display,
        rot: &mut PpaRotator,
    ) {
        self.fill_blank();
        let started = Instant::now();
        let mut slide = Slide {
            direction,
            old: self.old.picture(),
            new: self.new.picture(),
            blank: self.blank.picture(),
            blank_v: self.blank_v.picture(),
            dma: &mut self.dma,
            jobs: VecDeque::new(),
            start: None,
            new_ready: false,
            composed: false,
            finals: 0,
            frames: 0,
        };
        // 1. The screen as shown, before any frame overwrites it.
        let front = Picture::frame_buffer(display.front_buffer());
        slide.jobs.push_back(whole(front, slide.old));

        // 2. The new page, while the slide runs (only the PPA writes "new").
        let mut region = None;
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(RenderingRotation::NoRotation);
            rot.begin(Target::frame_buffer(slide.new));
            let r = renderer.render_by_line(Polling {
                lines: PpaLines { rot: &mut *rot },
                slide: &mut slide,
                display: &mut *display,
            });
            rot.finish();
            let (o, s) = (r.bounding_box_origin(), r.bounding_box_size());
            region = Some((
                o.x.max(0) as usize,
                o.y.max(0) as usize,
                s.width as usize,
                s.height as usize,
            ));
        });
        let rendered = started.elapsed();
        // Whatever Slint did not repaint is as it was on screen.
        for (x, y, w, h) in outside(region.unwrap_or_default()) {
            slide
                .jobs
                .push_back(landscape_copy(slide.old, slide.new, x, y, w, h));
        }
        slide.new_ready = true;

        // 3. The rest of the slide, then "new" in both buffers.
        // Polled without pause: a copy takes ~14 ms of the 15 ms frame, so
        // any delay between its steps costs a whole frame (sleeping 0.5 ms
        // between polls halved the frame rate). Other tasks still run.
        while !slide.finished() {
            slide.poll(display);
            embassy_futures::yield_now().await;
        }
        slide.dma.wait();
        while input::EVENTS.try_receive().is_ok() {}
        log::info!(
            "transition: {direction:?}, first frame after {} ms, new page rendered in {} ms, {} frames in {} ms",
            slide.start.map_or(0, |t| (t - started).as_millis()),
            rendered.as_millis(),
            slide.frames,
            started.elapsed().as_millis()
        );
    }

    /// Fills the background picture once (and again if the colour changed).
    fn fill_blank(&mut self) {
        let colour = BACKGROUND.load(Ordering::Relaxed);
        if self.blank_colour == Some(colour) {
            return;
        }
        for p in [self.blank.picture(), self.blank_v.picture()] {
            let px = p.w * p.h;
            // SAFETY: the picture holds `px` pixels; no DMA uses it now.
            unsafe { core::slice::from_raw_parts_mut(p.buf, px) }.fill(colour);
            cache::writeback(p.buf, px);
        }
        self.blank_colour = Some(colour);
    }
}

/// The running slide: DMA jobs queued and started one at a time, frames
/// composed and flipped as the panel takes them, all without blocking.
struct Slide<'a> {
    direction: Direction,
    old: Picture,
    new: Picture,
    blank: Picture,
    blank_v: Picture,
    dma: &'a mut FbCopy,
    jobs: VecDeque<Blit>,
    /// When the first frame was composed (the slide's time 0).
    start: Option<Instant>,
    /// "new" holds the new page.
    new_ready: bool,
    /// The queued jobs compose a frame: flip once they are done.
    composed: bool,
    /// Frames that showed all of "new" (two: one per frame buffer).
    finals: u8,
    frames: u32,
}

impl Slide<'_> {
    fn finished(&self) -> bool {
        self.finals >= 2 && !self.composed && self.jobs.is_empty() && self.dma.is_done()
    }

    /// One step: start the next copy if the DMA is idle; flip a composed
    /// frame once its copies are done; compose the next frame once a
    /// buffer is free.
    fn poll(&mut self, display: &mut Display) {
        if !self.dma.is_done() {
            return;
        }
        if let Some(blit) = self.jobs.pop_front() {
            self.dma.start(blit);
            return;
        }
        if self.composed {
            display.present(0..1).expect("present");
            self.composed = false;
            self.frames += 1;
            return;
        }
        if self.finals >= 2 {
            return;
        }
        let Some(fb) = display.try_back_buffer() else {
            return;
        };
        let fb = Picture::frame_buffer(fb.as_mut_ptr());
        let start = *self.start.get_or_insert_with(Instant::now);
        let t = start.elapsed().as_micros() as f32 / DURATION.as_micros() as f32;
        if t >= 1.0 && self.new_ready {
            self.jobs.push_back(whole(self.new, fb));
            self.finals += 1;
        } else {
            let eased = if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            };
            let (length, blank) = if self.vertical() {
                (WIDTH, BLANK_H)
            } else {
                (HEIGHT, BLANK_W)
            };
            let mut d = ((eased.min(1.0) * length as f32) as usize).min(length);
            if !self.new_ready {
                // Show no more of the new page than the background covers.
                d = d.min(blank);
            }
            self.compose(fb, d);
        }
        self.composed = true;
    }

    fn vertical(&self) -> bool {
        matches!(self.direction, Direction::Up | Direction::Down)
    }

    /// Queues one frame, `d` landscape pixels in: forward, the old page
    /// moves left and the new one follows from the right; back, the other
    /// way round; up, the new page comes up from the bottom; down, it comes
    /// down from the top. Before the new page is rendered, its part comes
    /// from a background picture.
    fn compose(&mut self, fb: Picture, d: usize) {
        let ready = self.new_ready;
        let (w, h) = (HEIGHT, WIDTH);
        // Where the visible part of the new page starts in it.
        let new_at = |from_start: bool, length: usize| if from_start { 0 } else { length - d };
        match self.direction {
            Direction::Forward | Direction::Back => {
                let forward = self.direction == Direction::Forward;
                let (src, from) = if ready {
                    (self.new, new_at(forward, w))
                } else {
                    (self.blank, 0)
                };
                if forward {
                    self.strip(self.old, d, fb, 0, w - d);
                    self.strip(src, from, fb, w - d, d);
                } else {
                    self.strip(src, from, fb, 0, d);
                    self.strip(self.old, 0, fb, d, w - d);
                }
            }
            Direction::Up | Direction::Down => {
                let up = self.direction == Direction::Up;
                let (src, from) = if ready {
                    (self.new, new_at(up, h))
                } else {
                    (self.blank_v, 0)
                };
                if up {
                    self.band(self.old, d, fb, 0, h - d);
                    self.band(src, from, fb, h - d, d);
                } else {
                    self.band(src, from, fb, 0, d);
                    self.band(self.old, 0, fb, d, h - d);
                }
            }
        }
    }

    /// Queues a copy of landscape rows `from..from+n` of `src` to `to..to+n`
    /// of `dst`. Landscape y is the panel column, so a row range is a block
    /// of whole columns.
    fn band(&mut self, src: Picture, from: usize, dst: Picture, to: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.jobs.push_back(Blit {
            src,
            from: (from, 0),
            dst,
            to: (to, 0),
            size: (n, HEIGHT),
        });
    }

    /// Queues a copy of landscape columns `from..from+n` of `src` to
    /// `to..to+n` of `dst`. Landscape x is the panel row `h-1-x` of a
    /// picture `h` rows tall, so a column range is a block of whole rows.
    fn strip(&mut self, src: Picture, from: usize, dst: Picture, to: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.jobs.push_back(Blit {
            src,
            from: (0, src.h - from - n),
            dst,
            to: (0, dst.h - to - n),
            size: (WIDTH, n),
        });
    }
}

/// Slint's line output to the PPA, driving the slide in between.
struct Polling<'a, 'b> {
    lines: PpaLines<'a>,
    slide: &'a mut Slide<'b>,
    display: &'a mut Display,
}

impl LineBufferProvider for Polling<'_, '_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        self.slide.poll(self.display);
        self.lines.process_line(line, range, render_fn);
    }
}

/// The parts of the landscape screen outside `(x, y, w, h)`, as up to four
/// rectangles.
fn outside((x, y, w, h): (usize, usize, usize, usize)) -> Vec<(usize, usize, usize, usize)> {
    let (sw, sh) = (HEIGHT, WIDTH);
    let (x1, y1) = ((x + w).min(sw), (y + h).min(sh));
    let mut parts = Vec::new();
    if w == 0 || h == 0 {
        parts.push((0, 0, sw, sh));
        return parts;
    }
    if x > 0 {
        parts.push((0, 0, x, sh));
    }
    if x1 < sw {
        parts.push((x1, 0, sw - x1, sh));
    }
    if y > 0 {
        parts.push((x, 0, x1 - x, y));
    }
    if y1 < sh {
        parts.push((x, y1, x1 - x, sh - y1));
    }
    parts
}

/// A copy of the landscape rectangle `(x, y, w, h)` between two full-screen
/// pictures: landscape (x, y) is panel column y, row `HEIGHT-1-x`.
fn landscape_copy(src: Picture, dst: Picture, x: usize, y: usize, w: usize, h: usize) -> Blit {
    let row = HEIGHT - x - w;
    Blit {
        src,
        from: (y, row),
        dst,
        to: (y, row),
        size: (h, w),
    }
}

/// A copy of all of one full-screen picture into another.
fn whole(src: Picture, dst: Picture) -> Blit {
    Blit {
        src,
        from: (0, 0),
        dst,
        to: (0, 0),
        size: (WIDTH, HEIGHT),
    }
}
