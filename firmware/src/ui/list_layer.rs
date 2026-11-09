//! A vertically scrolling list composed by DMA instead of being re-rendered.
//!
//! Rows are rendered by a Slint component in an offscreen window; the PPA
//! rotates each into a slot of a ring buffer ("layer") in PSRAM stored in
//! the panel's orientation. Every frame the visible window of the ring is
//! copied into the frame buffer by the 2D-DMA (~7 ms for 640×720 px) while
//! Slint renders the rest of the screen.
//!
//! The ring holds [`SLOTS`] rows (64 × 96 px rows = 7.9 MB). It is filled
//! from the start before the first frame, then kept filled around the scroll
//! position in the background: up to `AHEAD` rows in the scroll direction
//! and `BEHIND` rows behind, while the frame has time left, and continuously
//! while the screen is still. Visible rows that are missing (a jump) are
//! rendered before the copy.
//!
//! Touch physics (drag, fling, rubber band, spring back) are
//! [`wakey_core::scroll::Scroller`]. The gap past either end while
//! overscrolled is copied from a blank picture in the list's background
//! colour. A tap logs the row.
//!
//! Geometry (landscape): the list occupies `x0..x0+width`, full height. In
//! the panel's orientation the layer is `SLOTS·row_h` pixels wide (content
//! y, ring) and `width` rows tall (landscape x, mirrored). Slot `s` holds
//! the rows `i` with `i % SLOTS == s` at columns `s·row_h..`.
//!
//! The main Slint window must leave the list area alone (a plain background
//! that never changes).

use super::UI_HEIGHT;
use super::input::{Phase, TouchEvent};
use super::platform;
use super::render_loop::FrameHook;
use crate::display::cache;
use crate::display::dma2d::{FbCopy, Picture};
use crate::display::ppa::{PpaRotator, Target};
use crate::display::{HEIGHT, WIDTH};
use embassy_time::{Duration, Instant};
use esp_idf_svc::sys;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use std::rc::Rc;
use wakey_core::scroll::{MAX_OVERSCROLL, Scroller};

/// Rows held in the ring.
pub const SLOTS: usize = 64;
/// Rows kept rendered behind the view and ahead of it (scroll direction).
/// `BEHIND + visible (≤ 9) + AHEAD` must not exceed `SLOTS`, so filling
/// never evicts a visible row.
const BEHIND: usize = 15;
const AHEAD: usize = SLOTS - BEHIND - 9;
/// Background rendering stops once the frame has used this much time.
const FRAME_BUDGET: Duration = Duration::from_millis(9);
/// Width of the blank picture shown past the ends.
const BLANK_W: usize = MAX_OVERSCROLL as usize;

/// One 2D-DMA copy into the frame buffer: source picture, source position,
/// destination position, size.
type Copy = (Picture, (usize, usize), (usize, usize), (usize, usize));

fn now_us() -> u64 {
    Instant::now().as_micros()
}

pub struct ListLayer<F: FnMut(usize)> {
    /// Landscape x of the list's left edge, and its width.
    x0: usize,
    width: usize,
    row_h: usize,
    rows: usize,
    layer: Picture,
    /// Background colour, `BLANK_W × width`, for the gap past an end.
    blank: Picture,
    slot_row: [Option<usize>; SLOTS],
    row_window: Rc<MinimalSoftwareWindow>,
    /// Sets the offscreen row component's properties for a model row.
    set_row: F,
    dma: FbCopy,
    /// Copies of this frame after the first, started in `finish`.
    pending: heapless::Vec<Copy, 4>,
    /// Back buffer of the current frame (destination of `pending`).
    pending_fb: *mut u16,
    frame_start: Instant,
    scroller: Scroller,
    /// Frames left to keep composing after the list stopped, so both frame
    /// buffers end up showing the final position.
    settle: u8,
    /// Offset composed in the previous frame.
    shown: Option<i32>,
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
            buf: alloc(BLANK_W * width),
            w: BLANK_W,
            h: width,
        };
        // Filled once by the CPU, then written back for the DMA.
        unsafe { core::slice::from_raw_parts_mut(blank.buf, blank.w * blank.h) }.fill(background);
        cache::writeback(blank.buf, blank.w * blank.h);
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
            dma: FbCopy::new().expect("2D-DMA"),
            pending: heapless::Vec::new(),
            pending_fb: core::ptr::null_mut(),
            frame_start: Instant::now(),
            scroller: Scroller::new(UI_HEIGHT as f32, (rows * row_h) as f32),
            settle: 2,
            shown: None,
        }
    }

    fn content_h(&self) -> usize {
        self.rows * self.row_h
    }

    fn scroll(&self) -> i32 {
        self.scroller.offset().round() as i32
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
        // width-1..0 (the panel's mirroring); its lines to the slot's columns.
        let target = Target {
            buf: self.layer.buf,
            pic_w: self.layer.w,
            pic_h: self.layer.h,
            col_off: slot * self.row_h,
            row_ref: self.width - 1,
            lines: self.row_h,
        };
        platform::render_offscreen(&self.row_window, rot, target);
        self.slot_row[slot] = Some(i);
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
        let forward = self.scroller.direction() >= 0;
        let (fwd_n, back_n) = if forward {
            (AHEAD, BEHIND)
        } else {
            (BEHIND, AHEAD)
        };
        let hi = (last + fwd_n).min(self.rows - 1);
        let lo = first.saturating_sub(back_n);
        for d in 1..=fwd_n.max(back_n) {
            let fwd = (last + d <= hi).then_some(last + d);
            let back = first.checked_sub(d).filter(|&b| b >= lo);
            let order = if forward { [fwd, back] } else { [back, fwd] };
            for i in order.into_iter().flatten() {
                if self.slot_row[i % SLOTS] != Some(i) {
                    return Some(i);
                }
            }
        }
        None
    }

    /// The copies that show content y `scroll..scroll+view` in the frame
    /// buffer: a blank gap above the first row (scroll < 0), content from
    /// the ring (wrapping at most once), a blank gap below the last row.
    /// View y is the panel column; landscape x0..x0+width are panel rows
    /// `HEIGHT-x0-width..`.
    fn copies(&self, scroll: i32) -> heapless::Vec<Copy, 5> {
        let view_h = UI_HEIGHT as i32;
        let row0 = HEIGHT - self.x0 - self.width;
        let content_h = self.content_h() as i32;
        let mut copies = heapless::Vec::new();
        let top = scroll.max(0);
        let bottom = (scroll + view_h).min(content_h);
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
            let gap = ((-scroll) as usize).min(BLANK_W).min(view_h as usize);
            let _ = copies.push((self.blank, (0, 0), (0, row0), (gap, self.width)));
        }
        if scroll + view_h > content_h {
            let start = (content_h - scroll).max(0) as usize;
            let gap = (view_h as usize - start).min(BLANK_W);
            let _ = copies.push((self.blank, (0, 0), (start, row0), (gap, self.width)));
        }
        copies
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
        self.scroller.is_moving() || self.settle > 0
    }

    fn pointer(&mut self, ev: &TouchEvent) -> bool {
        let at = ev.at.as_micros();
        let consumed = match ev.phase {
            Phase::Down => {
                let inside = ev.x >= self.x0 as f32 && ev.x < (self.x0 + self.width) as f32;
                if inside {
                    self.scroller.press(ev.y, at);
                }
                inside
            }
            Phase::Move => self.scroller.drag_to(ev.y, at),
            Phase::Up => {
                let dragging = self.scroller.is_dragging();
                if let Some(y) = self.scroller.release(ev.y, at) {
                    log::info!("list: tapped row {}", y as usize / self.row_h);
                }
                dragging
            }
        };
        if consumed {
            self.settle = 2;
        }
        consumed
    }

    fn start(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.frame_start = Instant::now();
        self.scroller.tick(now_us());
        let scroll = self.scroll();
        if self.shown == Some(scroll) {
            self.settle = self.settle.saturating_sub(1);
        } else {
            self.settle = 2;
        }
        self.shown = Some(scroll);

        // Visible rows must be in the ring before the copy starts; normally
        // they were rendered in the background already.
        let (first, last) = self.visible_rows(scroll);
        for i in first..=last {
            self.ensure(i, rot);
        }

        // The first copy starts now and runs while Slint renders the rest
        // of the screen (other panel rows); the others follow in `finish`.
        let fbp = Self::fb_picture(fb.buf);
        let mut copies = self.copies(scroll).into_iter();
        if let Some((src, from, to, size)) = copies.next() {
            self.dma.start(src, from, fbp, to, size);
        }
        self.pending.clear();
        self.pending.extend(copies);
        self.pending_fb = fb.buf;
    }

    fn finish(&mut self, rot: &mut PpaRotator) {
        let fbp = Self::fb_picture(self.pending_fb);
        for (src, from, to, size) in core::mem::take(&mut self.pending) {
            self.dma.start(src, from, fbp, to, size);
        }
        // Background fill around the view while the frame has time. Missing
        // rows are never visible ones (those were rendered in `start`), so
        // the running copy is not disturbed.
        let (first, last) = self.visible_rows(self.scroll());
        while self.frame_start.elapsed() < FRAME_BUDGET {
            let Some(i) = self.next_missing(first, last) else {
                break;
            };
            self.ensure(i, rot);
        }
        self.dma.wait();
    }

    fn overlaps(&mut self, x: core::ops::Range<usize>) -> bool {
        x.start < self.x0 + self.width && self.x0 < x.end
    }

    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool {
        // The screen is still (no copy running): keep filling the ring.
        let (first, last) = self.visible_rows(self.scroll());
        match self.next_missing(first, last) {
            Some(i) => {
                self.ensure(i, rot);
                true
            }
            None => false,
        }
    }
}
