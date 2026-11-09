//! A vertically scrolling list composed by DMA instead of being re-rendered.
//!
//! Rows are rendered by a Slint component in an offscreen window; the PPA
//! rotates each into a slot of a ring buffer ("layer") in PSRAM that is
//! stored in the panel's orientation. Every frame the visible window of the
//! ring is copied into the frame buffer by the 2D-DMA (~7 ms for 640×720
//! px), while Slint renders the rest of the screen.
//!
//! The ring holds [`SLOTS`] rows (64 × 96 px rows = 7.9 MB). All 300 rows of
//! the test list would need 37 MB, more than the 32 MB PSRAM, so the ring is
//! pre-filled at start and then kept filled around the scroll position in the
//! background: up to `AHEAD` rows in the scroll direction and `BEHIND` rows
//! behind, while the frame has time left, and continuously while the screen
//! is still. Rows that are needed but missing (a jump) are rendered before
//! the copy.
//!
//! Touch: drag to scroll, release to fling (exponential deceleration). Past
//! either end the list overscrolls: dragging meets rubber-band resistance,
//! and a damped spring pulls it back after release or after a fling hits
//! the end. The gap beyond the end is copied from a blank picture in the
//! list's background colour. A tap logs the row.
//!
//! Geometry (landscape): the list occupies `x0..x0+width`, full height. In
//! the panel's orientation the layer is `SLOTS·row_h` pixels wide (content
//! y, ring) and `width` rows tall (landscape x, mirrored). Slot `s` holds
//! row `i` with `i % SLOTS == s` at columns `s·row_h..`.
//!
//! The main Slint window must leave the list area alone (plain background
//! that never changes).

use super::UI_HEIGHT;
use super::slint_platform::{self, FrameHook};
use super::touch::{Phase, TouchEvent};
use crate::display::dma2d::{FbCopy, Picture};
use crate::display::ppa::{self, PpaRotator, Target};
use crate::display::{HEIGHT, WIDTH};
use embassy_time::{Duration, Instant};
use esp_idf_svc::sys;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use std::rc::Rc;

/// Rows held in the ring.
pub const SLOTS: usize = 64;
/// Rows kept rendered behind the view and ahead of it (scroll direction).
/// `BEHIND + visible (≤ 9) + AHEAD` must not exceed `SLOTS`, so filling
/// never evicts a visible row.
const BEHIND: usize = 15;
const AHEAD: usize = SLOTS - BEHIND - 9;
/// Background rendering stops once the frame has used this much time.
const FRAME_BUDGET: Duration = Duration::from_millis(9);
/// Fling: velocity decays by e every `FLING_TAU` seconds; stops below
/// `FLING_STOP` px/s.
const FLING_TAU: f32 = 0.35;
const FLING_STOP: f32 = 20.0;
/// A touch that moves less than this (px) is a tap.
const TAP_SLOP: f32 = 12.0;
/// Overscroll: rubber-band constant (as on iOS) and the farthest the list
/// may be pulled past an end, in pixels (also the blank picture's width).
const RUBBER: f32 = 0.55;
const MAX_OVERSCROLL: usize = 360;
/// Spring that pulls an overscrolled list back: critically damped with this
/// angular frequency (1/s); settles in about 4/SPRING_W seconds.
const SPRING_W: f32 = 12.0;

/// Log the offset while overscrolled (tuning the spring).
const DEBUG_OVERSCROLL: bool = false;

/// One queued 2D-DMA copy: source picture, source position, destination
/// position (in the frame buffer), size.
type PendingCopy = (Picture, (usize, usize), (usize, usize), (usize, usize));

pub struct ListLayer<F: FnMut(usize)> {
    /// Landscape x of the list's left edge, and its width.
    x0: usize,
    width: usize,
    row_h: usize,
    rows: usize,
    layer: Picture,
    /// Background colour, `MAX_OVERSCROLL × width`, for the gap past an end.
    blank: Picture,
    slot_row: [Option<usize>; SLOTS],
    row_window: Rc<MinimalSoftwareWindow>,
    /// Sets the offscreen row component's properties for a model row.
    set_row: F,
    copy: FbCopy,
    /// Copies of this frame after the first, started in `finish`.
    pending: heapless::Vec<PendingCopy, 4>,
    /// Back buffer of the current frame (destination of `pending`).
    pending_fb: *mut u16,
    frame_start: Instant,

    /// Scroll position in content pixels (negative or beyond the end while
    /// overscrolled), and its velocity (px/s).
    offset: f32,
    velocity: f32,
    last_tick: Instant,
    /// Scroll direction of the last movement (+1 down the list, -1 up).
    direction: i32,
    drag: Option<Drag>,
    /// Frames left to keep composing after the list stopped, so both frame
    /// buffers end up with the final position.
    settle: u8,
    /// Offset composed in the previous frame.
    shown: Option<i32>,
    /// Rows rendered, for the stats.
    pub rendered: u32,
}

struct Drag {
    /// Finger position and the list position it grabbed, in "raw" units
    /// (before the rubber band).
    start_y: f32,
    start_raw: f32,
    last_y: f32,
    last_at: Instant,
    moved: bool,
}

/// Visible stretch for a pull of `d` px past an end (rubber band).
fn band(d: f32) -> f32 {
    let dim = UI_HEIGHT as f32;
    ((1.0 - 1.0 / (d * RUBBER / dim + 1.0)) * dim).min(MAX_OVERSCROLL as f32)
}

/// Inverse of [`band`]: the pull that shows a stretch of `x` px.
fn unband(x: f32) -> f32 {
    let dim = UI_HEIGHT as f32;
    let x = x.min(dim * 0.99);
    dim / RUBBER * (1.0 / (1.0 - x / dim) - 1.0)
}

impl<F: FnMut(usize)> ListLayer<F> {
    /// `row_window` must be the offscreen window of the row component
    /// (`width × row_h`); `rows` is the model length; `background` the
    /// RGB565 colour shown past the ends.
    pub fn new(
        x0: usize,
        width: usize,
        row_h: usize,
        rows: usize,
        row_window: Rc<MinimalSoftwareWindow>,
        set_row: F,
        background: u16,
    ) -> Self {
        assert!(x0 + width <= HEIGHT && rows > 0);
        assert!(UI_HEIGHT as usize <= 8 * row_h, "view taller than 8 rows");
        let alloc = |px: usize| {
            let p = unsafe {
                sys::heap_caps_aligned_alloc(
                    128,
                    (px * 2).next_multiple_of(128),
                    sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_DMA,
                )
            };
            assert!(!p.is_null(), "no PSRAM for the list layer");
            p.cast::<u16>()
        };
        let w = SLOTS * row_h;
        let layer = Picture {
            buf: alloc(w * width),
            w,
            h: width,
        };
        let blank = Picture {
            buf: alloc(MAX_OVERSCROLL * width),
            w: MAX_OVERSCROLL,
            h: width,
        };
        // Filled once by the CPU, then written back for the DMA.
        unsafe { core::slice::from_raw_parts_mut(blank.buf, blank.w * blank.h) }.fill(background);
        ppa::cache_writeback(blank.buf, blank.w * blank.h);
        log::info!(
            "list layer: {rows} rows, ring of {SLOTS} ({w}×{width} px, {} KiB PSRAM)",
            w * width * 2 / 1024
        );
        Self {
            x0,
            width,
            row_h,
            rows,
            layer,
            blank,
            slot_row: [None; SLOTS],
            row_window,
            set_row,
            copy: FbCopy::new().expect("2D-DMA"),
            pending: heapless::Vec::new(),
            pending_fb: core::ptr::null_mut(),
            frame_start: Instant::now(),
            offset: 0.0,
            velocity: 0.0,
            last_tick: Instant::now(),
            direction: 1,
            drag: None,
            settle: 2,
            shown: None,
            rendered: 0,
        }
    }

    fn content_h(&self) -> usize {
        self.rows * self.row_h
    }

    fn max_offset(&self) -> f32 {
        self.content_h().saturating_sub(UI_HEIGHT as usize) as f32
    }

    /// Distance past the nearest end (negative above the top), 0 in range.
    fn overshoot(&self) -> f32 {
        let max = self.max_offset();
        if self.offset < 0.0 {
            self.offset
        } else if self.offset > max {
            self.offset - max
        } else {
            0.0
        }
    }

    /// Rows visible at scroll position `offset` (clamped to the content).
    fn visible_rows(&self, offset: i32) -> (usize, usize) {
        let top = offset.max(0) as usize;
        let bottom = ((offset + UI_HEIGHT as i32).max(1) as usize).min(self.content_h());
        let first = (top / self.row_h).min(self.rows - 1);
        let last = ((bottom - 1) / self.row_h).clamp(first, self.rows - 1);
        (first, last)
    }

    /// Renders row `i` into its slot, if missing.
    fn ensure(&mut self, i: usize, rot: &mut PpaRotator) {
        let slot = i % SLOTS;
        if i >= self.rows || self.slot_row[slot] == Some(i) {
            return;
        }
        (self.set_row)(i);
        // The row window's landscape x 0..width maps to layer rows
        // width-1..0 (same mirroring as the panel); its lines to the slot's
        // columns.
        let target = Target {
            buf: self.layer.buf,
            pic_w: self.layer.w,
            pic_h: self.layer.h,
            col_off: slot * self.row_h,
            row_ref: self.width - 1,
            lines: self.row_h,
        };
        slint_platform::render_offscreen(&self.row_window, rot, target);
        self.slot_row[slot] = Some(i);
        self.rendered += 1;
    }

    /// Renders the first `SLOTS` rows, so the start of the list is ready
    /// before the first frame.
    pub fn prerender(&mut self, rot: &mut PpaRotator) {
        let t = Instant::now();
        let n = SLOTS.min(self.rows);
        for i in 0..n {
            self.ensure(i, rot);
        }
        log::info!(
            "list layer: pre-rendered {n} rows in {} ms",
            t.elapsed().as_millis()
        );
    }

    /// Next missing row near the view, nearest first, alternating between
    /// the scroll direction (first) and the other; `None` if the window of
    /// `AHEAD`/`BEHIND` rows around the view is complete.
    fn next_missing(&self, first: usize, last: usize) -> Option<usize> {
        let (fwd_n, back_n) = if self.direction >= 0 {
            (AHEAD, BEHIND)
        } else {
            (BEHIND, AHEAD)
        };
        let hi = (last + fwd_n).min(self.rows - 1);
        let lo = first.saturating_sub(back_n);
        for d in 1..=fwd_n.max(back_n) {
            let fwd = (last + d <= hi).then_some(last + d);
            let back = first.checked_sub(d).filter(|&b| b >= lo);
            let order = if self.direction >= 0 {
                [fwd, back]
            } else {
                [back, fwd]
            };
            for i in order.into_iter().flatten() {
                if self.slot_row[i % SLOTS] != Some(i) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Advances fling and spring by the time since the last call, in steps
    /// of at most 4 ms (the spring is stiff).
    fn tick(&mut self) {
        let now = Instant::now();
        let mut left = (now - self.last_tick).as_micros() as f32 / 1e6;
        self.last_tick = now;
        if self.drag.is_some() {
            return;
        }
        let max = self.max_offset();
        while left > 0.0 && (self.velocity != 0.0 || self.overshoot() != 0.0) {
            let dt = left.min(0.004);
            left -= dt;
            let x = self.overshoot();
            if x != 0.0 {
                // Spring towards the end, critically damped.
                let a = -SPRING_W * SPRING_W * x - 2.0 * SPRING_W * self.velocity;
                self.velocity += a * dt;
                self.offset += self.velocity * dt;
                let x_new = self.overshoot();
                let settled = x_new.abs() < 0.5 && self.velocity.abs() < 10.0;
                if settled || x_new * x < 0.0 || (x_new == 0.0 && x != 0.0) {
                    // Back at (or across) the end: stop exactly there.
                    self.offset = if x < 0.0 { 0.0 } else { max };
                    self.velocity = 0.0;
                }
            } else {
                // Free fling; the spring takes over if it runs past an end.
                self.offset += self.velocity * dt;
                self.velocity *= (-dt / FLING_TAU).exp();
                if self.velocity.abs() < FLING_STOP {
                    self.velocity = 0.0;
                }
            }
            let limit = MAX_OVERSCROLL as f32;
            self.offset = self.offset.clamp(-limit, max + limit);
        }
    }

    /// Visible offset for a raw (finger) position: rubber band past the ends.
    fn rubber(&self, raw: f32) -> f32 {
        let max = self.max_offset();
        if raw < 0.0 {
            -band(-raw)
        } else if raw > max {
            max + band(raw - max)
        } else {
            raw
        }
    }

    /// Inverse of [`Self::rubber`].
    fn unrubber(&self, offset: f32) -> f32 {
        let max = self.max_offset();
        if offset < 0.0 {
            -unband(-offset)
        } else if offset > max {
            max + unband(offset - max)
        } else {
            offset
        }
    }

    fn fb_picture(fb: *mut u16) -> Picture {
        Picture {
            buf: fb,
            w: WIDTH,
            h: HEIGHT,
        }
    }
}

impl<F: FnMut(usize)> FrameHook for ListLayer<F> {
    fn active(&self) -> bool {
        self.drag.is_some() || self.velocity != 0.0 || self.overshoot() != 0.0 || self.settle > 0
    }

    fn pointer(&mut self, ev: &TouchEvent) -> bool {
        let inside = ev.x >= self.x0 as f32 && ev.x < (self.x0 + self.width) as f32;
        match ev.phase {
            Phase::Down if inside => {
                // Grabbing also catches a running fling or spring.
                self.velocity = 0.0;
                self.drag = Some(Drag {
                    start_y: ev.y,
                    start_raw: self.unrubber(self.offset),
                    last_y: ev.y,
                    last_at: ev.at,
                    moved: false,
                });
                self.settle = 2;
                true
            }
            Phase::Move => {
                let Some(d) = self.drag.as_mut() else {
                    return false;
                };
                if (ev.y - d.start_y).abs() > TAP_SLOP {
                    d.moved = true;
                }
                let dt = (ev.at - d.last_at).as_micros() as f32 / 1e6;
                if dt > 0.0 {
                    // Finger velocity, smoothed; the list moves the other way.
                    let v = -(ev.y - d.last_y) / dt;
                    self.velocity = 0.6 * v + 0.4 * self.velocity;
                }
                d.last_y = ev.y;
                d.last_at = ev.at;
                let raw = d.start_raw - (ev.y - d.start_y);
                self.offset = self.rubber(raw);
                if self.velocity != 0.0 {
                    self.direction = self.velocity.signum() as i32;
                }
                self.settle = 2;
                true
            }
            Phase::Up => {
                let Some(d) = self.drag.take() else {
                    return false;
                };
                // A finger that rested before lifting does not fling.
                if (ev.at - d.last_at) > Duration::from_millis(80) {
                    self.velocity = 0.0;
                }
                if !d.moved {
                    self.velocity = 0.0;
                    let row = (self.offset + ev.y).max(0.0) as usize / self.row_h;
                    log::info!("list: tapped row {row}");
                }
                // Released while stretched: the spring returns it; finger
                // velocity into the stretch would only overshoot further.
                if self.overshoot() * self.velocity > 0.0 {
                    self.velocity = 0.0;
                }
                self.last_tick = Instant::now();
                self.settle = 2;
                true
            }
            _ => false,
        }
    }

    fn start(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.frame_start = Instant::now();
        self.tick();
        let view_h = UI_HEIGHT as i32;
        let scroll = self.offset.round() as i32;
        if self.shown == Some(scroll) {
            self.settle = self.settle.saturating_sub(1);
        } else {
            self.settle = 2;
        }
        self.shown = Some(scroll);
        if DEBUG_OVERSCROLL && self.overshoot() != 0.0 {
            log::info!("list: offset {scroll} v {:.0}", self.velocity);
        }

        // Visible rows must be in the ring before the copy starts; normally
        // they were pre-rendered in the background.
        let (first, last) = self.visible_rows(scroll);
        for i in first..=last {
            self.ensure(i, rot);
        }

        // The view shows content y scroll..scroll+view_h: a blank gap above
        // the first row (scroll < 0), content, a blank gap below the last
        // row. Landscape x0..x0+width = panel rows HEIGHT-x0-width..; view y
        // = panel column. Content comes from ring column y % ring width and
        // may wrap once. The copies run while Slint renders the rest of the
        // screen (other panel rows).
        let fbp = Self::fb_picture(fb.buf);
        let row0 = HEIGHT - self.x0 - self.width;
        let content_h = self.content_h() as i32;
        let top = scroll.max(0);
        let bottom = (scroll + view_h).min(content_h);
        let mut copies: heapless::Vec<PendingCopy, 5> = heapless::Vec::new();
        if bottom > top {
            let dst = (top - scroll) as usize;
            let p = top as usize % self.layer.w;
            let n = (bottom - top) as usize;
            let n1 = n.min(self.layer.w - p);
            let _ = copies.push((self.layer, (p, 0), (dst, row0), (n1, self.width)));
            if n1 < n {
                let _ = copies.push((self.layer, (0, 0), (dst + n1, row0), (n - n1, self.width)));
            }
        }
        if scroll < 0 {
            let gap = ((-scroll) as usize)
                .min(MAX_OVERSCROLL)
                .min(view_h as usize);
            let _ = copies.push((self.blank, (0, 0), (0, row0), (gap, self.width)));
        }
        if scroll + view_h > content_h {
            let start = (content_h - scroll).max(0) as usize;
            let gap = (view_h as usize - start).min(MAX_OVERSCROLL);
            let _ = copies.push((self.blank, (0, 0), (start, row0), (gap, self.width)));
        }
        let mut it = copies.into_iter();
        if let Some((src, from, to, size)) = it.next() {
            self.copy.start(src, from, fbp, to, size);
        }
        self.pending.clear();
        for (src, from, to, size) in it {
            // The destination is always the frame buffer; remember it.
            let _ = self.pending.push((src, from, to, size));
        }
        self.pending_fb = fb.buf;
    }

    fn finish(&mut self, rot: &mut PpaRotator) {
        let fbp = Self::fb_picture(self.pending_fb);
        for (src, from, to, size) in core::mem::take(&mut self.pending) {
            self.copy.start(src, from, fbp, to, size);
        }
        // Background fill around the view while the frame has time. Missing
        // rows are never visible ones (those were rendered in `start`), so
        // the running copy is not disturbed.
        let (first, last) = self.visible_rows(self.offset.round() as i32);
        while self.frame_start.elapsed() < FRAME_BUDGET {
            let Some(i) = self.next_missing(first, last) else {
                break;
            };
            self.ensure(i, rot);
        }
        self.copy.wait();
    }

    fn overlaps(&mut self, x: core::ops::Range<usize>) -> bool {
        x.start < self.x0 + self.width && self.x0 < x.end
    }

    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool {
        // The screen is still (no copy running): keep filling the ring.
        let (first, last) = self.visible_rows(self.offset.round() as i32);
        match self.next_missing(first, last) {
            Some(i) => {
                self.ensure(i, rot);
                true
            }
            None => false,
        }
    }
}
