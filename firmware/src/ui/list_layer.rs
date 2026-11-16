//! A vertically scrolling list composed by DMA while it moves; Slint draws
//! it while it is still.
//!
//! **Rows.** A Slint component in an offscreen window renders one row at a
//! time; the PPA rotates it into a slot of a ring buffer ("layer") in PSRAM,
//! stored in the panel's orientation. A row is rendered again when its
//! [`ListSource::stamp`] changes. Rows are rendered ahead in idle time, so
//! a scroll can start at once.
//!
//! **Scrolling.** While the list moves, every frame the 2D-DMA copies the
//! visible part of the ring into the frame buffer (~11 ms for 1200×588 px),
//! and Slint only renders the rest of the screen. Physics (drag, fling,
//! rubber band, spring back) are [`wakey_core::scroll::Scroller`]; past
//! either end a blank picture in the background colour is copied.
//!
//! **Still.** When the list stops, the layer hands the position to the page
//! ([`ListSource::scroll_ended`]) and Slint draws the list there, with
//! the same component, so the pixels are the same. Slint then owns the
//! touches: taps and switches.
//!
//! **Swiping.** A touch that moves sideways on a still row is the layer's
//! too: it renders that row (highlighted, its corners on the delete
//! button's red), the bare delete button and, for the first
//! [`Layout::row_radius`] px where the red must not show in the corners, the
//! highlighted row as the page draws it into pictures, and moves the row
//! with the finger by DMA (two copies of one row band per frame).
//! Released past [`DELETE_FRACTION`] of the width, the row is deleted;
//! otherwise it springs back, then the page takes over again
//! ([`ListSource::swipe_ended`]) and draws the same pixels.
//!
//! **Touch.** The layer sees every touch event first:
//! - On the still list, events go to Slint until the finger has moved more
//!   than [`Layout::slop`]. Mostly vertical: the layer takes the touch
//!   ([`Route::TakeOver`], Slint gets a cancel) and scrolls with it, unless
//!   the list fits the view (then nothing scrolls). Mostly sideways on a
//!   row: the layer takes it and swipes that row.
//! - On the moving list a touch stops it and stays the layer's: lifting
//!   does not tap the row that slid under the finger, moving scrolls on.
//!
//! **Geometry** (landscape): the list occupies `x..x+width`, `y..y+height`.
//! In the panel's orientation the ring is `slots·row_h` pixels wide
//! (content y) and `width` rows tall (landscape x, mirrored). Slot `s` holds
//! the rows `i` with `i % slots == s` at columns `s·row_h..`. Rows include
//! the gap below them.

use super::input::{Phase, TouchEvent};
use super::platform;
use super::render_loop::{FrameHook, Route};
use crate::display::cache;
use crate::display::dma2d::{Blit, FbCopy, OwnedPicture, Picture};
use crate::display::ppa::{PpaRotator, Target};
use crate::display::{HEIGHT, WIDTH};
use core::ops::Range;
use embassy_time::{Duration, Instant};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use std::rc::Rc;
use wakey_core::scroll::Scroller;

/// Background rendering stops once the frame has used this much time.
const FRAME_BUDGET: Duration = Duration::from_millis(9);
/// Frames the layer keeps composing after the list stopped, so both frame
/// buffers show the final position before Slint takes over.
const SETTLE_FRAMES: u8 = 2;
/// A swipe released past this part of the width deletes the row; a
/// shorter one springs back.
const DELETE_FRACTION: f32 = 2.0 / 3.0;
/// The spring back of a released row.
const SNAP: Duration = Duration::from_millis(180);

/// What [`ListSource::prepare_swipe`] puts into the row component.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwipePart {
    /// The row at offset 0, lit up or not. With `underlay` its rounded
    /// corners are on the delete button's red (what shows behind them once
    /// it is swiped further than the corner radius); without, on the page
    /// background, as the page draws it.
    Content { highlighted: bool, underlay: bool },
    /// The delete button alone (the row moved out).
    Underlay,
}

/// How a swipe ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SwipeEnd {
    /// Sprang back.
    Closed,
    /// Swiped past [`DELETE_FRACTION`]: delete the row.
    Delete,
}

/// The list's content and the page around it.
pub trait ListSource {
    /// The list is on screen (its page is shown).
    fn shown(&self) -> bool;
    /// Number of rows.
    fn rows(&self) -> usize;
    /// A value that changes whenever row `i` would look different.
    fn stamp(&self, i: usize) -> u64;
    /// Puts row `i` into the offscreen row component (rendered next).
    fn prepare(&mut self, i: usize);
    /// The same for the pictures of a swipe of row `i`.
    fn prepare_swipe(&mut self, i: usize, part: SwipePart);
    /// Scroll position of the still list (content y at the top of the view).
    fn offset(&self) -> f32;
    /// The layer took over (a scroll or a swipe): drop any touch feedback
    /// on the page (a highlight).
    fn layer_started(&mut self);
    /// When a scroll starts, render visible row `i` again: it shows
    /// something that moved on the page meanwhile (e.g. scrolling text).
    /// Each such row costs the scroll's first frame its render time.
    fn changes_on_scroll(&mut self, _i: usize) -> bool {
        false
    }
    /// The list stopped at `offset` (whole pixels): the page draws it there.
    fn scroll_ended(&mut self, offset: f32);
    /// The swipe of row `i` ended: the page shows it again, or deletes it.
    fn swipe_ended(&mut self, i: usize, end: SwipeEnd);
}

/// Where the list is on screen (landscape px) and how its touches work.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    /// Row pitch: row height plus the gap below it.
    pub row_h: usize,
    /// Rows the ring holds.
    pub slots: usize,
    /// How far a finger moves before the touch counts as a scroll or a
    /// swipe (the page's rule).
    pub slop: f32,
    /// RGB565 colour behind the rows (shown past the ends).
    pub background: u16,
    /// The page draws the list while it is still, and the layer only while
    /// it moves. Otherwise the layer always shows it (the page leaves the
    /// area blank) and has all its touches.
    pub page_draws_still: bool,
    /// A sideways drag on a row swipes it (to delete); otherwise it is the
    /// page's.
    pub swipe: bool,
    /// Corner radius of a row: a swiped row moved less than this is drawn
    /// as the page draws it (its corners on the page background).
    pub row_radius: usize,
    /// The list sits in a card with rounded corners: radius and the colour
    /// outside them (RGB565). Slint clips rectangles only, so the page
    /// draws caps over the corners ([`corner_alpha`]); the layer blends the
    /// same caps into what it copied.
    pub corners: Option<(usize, u16)>,
}

/// How much of each pixel of a `radius` × `radius` top-left corner lies
/// outside the rounding, 0..=16 (4 × 4 samples), row by row (landscape y,
/// then x). The caps over a card's corners: the page's images and the
/// layer's blend use the same values.
pub fn corner_alpha(radius: usize) -> Vec<u8> {
    let r = radius as f32;
    let mut out = Vec::with_capacity(radius * radius);
    for y in 0..radius {
        for x in 0..radius {
            let mut n = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                    if (r - px).powi(2) + (r - py).powi(2) > r * r {
                        n += 1;
                    }
                }
            }
            out.push(n);
        }
    }
    out
}

/// `bg` over `px` (RGB565) with weight `a` of 16.
fn blend565(px: u16, bg: u16, a: u8) -> u16 {
    let a = u32::from(a);
    let mix = |shift: u32, mask: u32| {
        let p = (u32::from(px) >> shift) & mask;
        let b = (u32::from(bg) >> shift) & mask;
        ((b * a + p * (16 - a) + 8) / 16) << shift
    };
    (mix(11, 0x1f) | mix(5, 0x3f) | mix(0, 0x1f)) as u16
}

/// The PSRAM a layer draws into: the ring, the blank picture and the swipe
/// pictures. Two layers whose pages are never on screen together can share
/// it ([`ListLayer::new_in`], [`SharedLayers`]), each with its own picture
/// sizes.
pub struct Buffers {
    ring: OwnedPicture,
    blank: OwnedPicture,
    swipe: OwnedPicture,
}

/// Who has the current touch.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Touch {
    /// No finger, or one that went down outside the list.
    None,
    /// Down on the still list at (x, y); Slint has it so far.
    Pending(f32, f32),
    /// A sideways touch: Slint's to the end.
    Slint,
    /// The layer scrolls with it.
    Scroll,
    /// The layer swipes a row with it.
    Swipe,
    /// Down while a swipe snaps: ignored to the end.
    Ignored,
}

/// A row being swiped (by the finger, or snapping after release).
#[derive(Clone, Copy, Debug)]
struct Swipe {
    row: usize,
    /// How far the row is moved left, px.
    d: f32,
    /// Finger x when the swipe started.
    finger_x0: f32,
    /// The content picture (on red) is rendered for this row and
    /// `highlighted`.
    content: bool,
    /// The lit plain picture is rendered (only needed while lit: unlit, the
    /// ring slot has it).
    plain: bool,
    /// The row is lit up (while the finger is down).
    highlighted: bool,
    /// After release: springing back from this `d`, since then.
    snap: Option<(f32, Instant)>,
}

/// Slots of the swipe pictures.
const CONTENT: usize = 0;
const UNDERLAY: usize = 1;
const PLAIN: usize = 2;

pub struct ListLayer<S: ListSource> {
    layout: Layout,
    /// The memory of `layer`, `blank` and `swipe_pics`.
    buffers: Rc<Buffers>,
    /// Rows kept rendered behind the view and ahead of it (in the scroll
    /// direction), so filling never evicts a visible row.
    behind: usize,
    ahead: usize,
    layer: Picture,
    blank: Picture,
    /// The page background (undimmed), and the [`crate::ui::dim::epoch`]
    /// `blank` was filled in: it is refilled, dimmed, when that changes.
    background: u16,
    blank_epoch: u32,
    /// Row and stamp held by each slot.
    slot_row: Vec<Option<(usize, u64)>>,
    row_window: Rc<MinimalSoftwareWindow>,
    source: S,
    dma: FbCopy,
    /// Copies of this frame after the first, started in `finish`.
    pending: heapless::Vec<Blit, 4>,
    /// Back buffer of the current frame.
    fb: Picture,
    frame_start: Instant,
    scroller: Scroller,
    touch: Touch,
    /// The layer shows the list (from the start of a scroll until both
    /// frame buffers show where it stopped).
    composing: bool,
    /// A row being swiped (never together with `composing`).
    swipe: Option<Swipe>,
    /// The swipe pictures: content, underlay and plain, three row slots.
    swipe_pics: Picture,
    /// The underlay is rendered (it is the same for every row).
    underlay_ready: bool,
    /// [`crate::ui::dim::epoch`] the underlay was rendered in.
    underlay_epoch: u32,
    settle: u8,
    /// Offset composed in the previous frame.
    shown: Option<i32>,
    /// The corner caps as runs in the frame buffer: start pixel and the
    /// weights of its pixels (see [`Layout::corners`]).
    caps: Vec<(usize, Vec<u8>)>,
    /// This frame's copies came from the layer (the caps go on them).
    drew: bool,
    /// Rows rendered and the time it took (logged every 32 rows).
    rendered: (u32, Duration),
    /// This scroll's frame timing (logged when it ends).
    scroll_log: ScrollLog,
}

/// Frame timing of one scroll: start-to-start gaps of the layer's frames
/// and the render loop's own time in the frame before each late one.
#[derive(Default)]
struct ScrollLog {
    since: Option<Instant>,
    last_start: Option<Instant>,
    /// The previous frame's time from `start` to the end of `finish`.
    last_busy: Duration,
    frames: u32,
    late: u32,
    /// Longest gap and the busy time of the frame before it.
    worst: (Duration, Duration),
    rows: u32,
    rows_time: Duration,
}

/// A gap longer than this missed a panel refresh (15.2 ms at 66 Hz).
const LATE: Duration = Duration::from_millis(20);

impl<S: ListSource> ListLayer<S> {
    /// `row_window` must be the offscreen window of the row component
    /// (`width × row_h`).
    pub fn new(layout: Layout, row_window: Rc<MinimalSoftwareWindow>, source: S) -> Self {
        Self::new_in(layout, row_window, source, None)
    }

    /// [`Self::new`], drawing into `shared` (another layer's buffers, see
    /// [`Self::buffers`]) instead of its own.
    pub fn new_in(
        layout: Layout,
        row_window: Rc<MinimalSoftwareWindow>,
        source: S,
        shared: Option<Rc<Buffers>>,
    ) -> Self {
        let Layout {
            x,
            y,
            width,
            height,
            row_h,
            slots,
            background,
            ..
        } = layout;
        assert!(x + width <= HEIGHT && y + height <= WIDTH);
        let visible = height.div_ceil(row_h) + 1;
        assert!(slots > visible, "ring smaller than the view");
        let behind = (slots - visible) / 4;
        // In the panel's orientation: the ring and the swipe pictures are
        // rows side by side; the blank is as tall as the view, so it covers
        // it even when the list is shorter.
        let ring_w = slots * row_h;
        let (swipe_w, blank_w) = (3 * row_h, height);
        let buffers = match shared {
            Some(b) => {
                log::info!("list layer: ring of {slots} rows ({ring_w}×{width} px), shared");
                b
            }
            None => {
                log::info!(
                    "list layer: ring of {slots} rows ({ring_w}×{width} px, {} KiB PSRAM)",
                    ring_w * width * 2 / 1024
                );
                let alloc = |w| OwnedPicture::new(w, width).expect("no PSRAM for the list layer");
                Rc::new(Buffers {
                    ring: alloc(ring_w),
                    blank: alloc(blank_w),
                    swipe: alloc(swipe_w),
                })
            }
        };
        let too_small = "shared list layer buffers too small";
        let layer = buffers.ring.reshaped(ring_w, width).expect(too_small);
        let blank = buffers.blank.reshaped(blank_w, width).expect(too_small);
        let swipe_pics = buffers.swipe.reshaped(swipe_w, width).expect(too_small);
        Self {
            layout,
            buffers,
            behind,
            ahead: slots - visible - behind,
            layer,
            blank,
            background,
            // Not filled dimmed yet: the first frame refills it.
            blank_epoch: crate::ui::dim::epoch().wrapping_sub(1),
            slot_row: vec![None; slots],
            row_window,
            source,
            dma: FbCopy::new().expect("2D-DMA"),
            pending: heapless::Vec::new(),
            fb: Picture::frame_buffer(core::ptr::null_mut()),
            frame_start: Instant::now(),
            scroller: Scroller::new(height as f32, 0.0),
            touch: Touch::None,
            composing: !layout.page_draws_still,
            swipe: None,
            swipe_pics,
            underlay_ready: false,
            underlay_epoch: 0,
            settle: 0,
            shown: None,
            caps: layout.corners.map_or(Vec::new(), |(r, _)| caps(&layout, r)),
            drew: false,
            rendered: (0, Duration::from_ticks(0)),
            scroll_log: ScrollLog::default(),
        }
    }

    /// This layer's buffers, for a layer that shares them
    /// ([`Self::new_in`]).
    pub fn buffers(&self) -> Rc<Buffers> {
        self.buffers.clone()
    }

    /// The list's page is on screen.
    pub fn is_shown(&self) -> bool {
        self.source.shown()
    }

    /// Another layer drew into the shared buffers: every row, the blank and
    /// the swipe pictures are rendered again.
    pub fn invalidate(&mut self) {
        self.slot_row.fill(None);
        self.blank_epoch = crate::ui::dim::epoch().wrapping_sub(1);
        self.underlay_ready = false;
    }

    /// Renders the rows around the current position now (otherwise idle
    /// time does it), e.g. before the first frame.
    pub fn prerender(&mut self, rot: &mut PpaRotator) {
        let t = Instant::now();
        let mut n = 0;
        while self.fill_one(rot) {
            n += 1;
        }
        log::info!(
            "list layer: pre-rendered {n} rows in {} ms",
            t.elapsed().as_millis()
        );
    }

    fn content_h(&self) -> usize {
        self.source.rows() * self.layout.row_h
    }

    /// The scroll position to show: the scroller's while composing, else
    /// the page's.
    fn offset(&self) -> i32 {
        if self.composing {
            self.scroller.offset().round() as i32
        } else {
            self.source.offset().round() as i32
        }
    }

    /// Rows visible at scroll position `offset` (clamped to the content);
    /// `None` without rows.
    fn visible_rows(&self, offset: i32) -> Option<(usize, usize)> {
        let rows = self.source.rows();
        if rows == 0 {
            return None;
        }
        let row_h = self.layout.row_h;
        let top = offset.max(0) as usize;
        let bottom = ((offset + self.layout.height as i32).max(1) as usize).min(self.content_h());
        let first = (top / row_h).min(rows - 1);
        let last = (bottom.saturating_sub(1) / row_h).clamp(first, rows - 1);
        Some((first, last))
    }

    /// Renders row `i` into its slot, unless it is there and current.
    /// Row `i`'s stamp, including the dimming ([`crate::ui::dim`]): a row
    /// drawn before it changed is stale.
    fn stamp(&self, i: usize) -> u64 {
        self.source.stamp(i) ^ u64::from(crate::ui::dim::epoch()).rotate_left(48)
    }

    fn ensure(&mut self, i: usize, rot: &mut PpaRotator) {
        let slots = self.layout.slots;
        let stamp = self.stamp(i);
        if self.slot_row[i % slots] == Some((i, stamp)) {
            return;
        }
        self.source.prepare(i);
        // The row window's landscape x 0..width maps to layer rows
        // width-1..0 (the panel's mirroring); its lines to the slot's columns.
        let target = Target {
            buf: self.layer.buf,
            pic_w: self.layer.w,
            pic_h: self.layer.h,
            col_off: i % slots * self.layout.row_h,
            row_ref: self.layout.width - 1,
            lines: self.layout.row_h,
        };
        let t = Instant::now();
        platform::render_offscreen(&self.row_window, rot, target);
        self.slot_row[i % slots] = Some((i, stamp));
        self.rendered.0 += 1;
        self.rendered.1 += t.elapsed();
        if self.composing {
            self.scroll_log.rows += 1;
            self.scroll_log.rows_time += t.elapsed();
        }
        if self.rendered.0 == 32 {
            log::info!(
                "list layer: 32 rows rendered, {} µs each",
                self.rendered.1.as_micros() / 32
            );
            self.rendered = (0, Duration::from_ticks(0));
        }
    }

    /// Next row near the view that is missing or stale, nearest first,
    /// alternating between the scroll direction (first) and the other.
    fn next_missing(&self, first: usize, last: usize) -> Option<usize> {
        let rows = self.source.rows();
        let current = |i: usize| self.slot_row[i % self.layout.slots] == Some((i, self.stamp(i)));
        if let Some(i) = (first..=last).find(|&i| !current(i)) {
            return Some(i);
        }
        let forward = self.scroller.direction() >= 0;
        let (fwd_n, back_n) = if forward {
            (self.ahead, self.behind)
        } else {
            (self.behind, self.ahead)
        };
        let hi = (last + fwd_n).min(rows - 1);
        let lo = first.saturating_sub(back_n);
        for d in 1..=fwd_n.max(back_n) {
            let fwd = (last + d <= hi).then_some(last + d);
            let back = first.checked_sub(d).filter(|&b| b >= lo);
            let order = if forward { [fwd, back] } else { [back, fwd] };
            if let Some(i) = order.into_iter().flatten().find(|&i| !current(i)) {
                return Some(i);
            }
        }
        None
    }

    /// Renders the next missing or stale row; false if there is none.
    fn fill_one(&mut self, rot: &mut PpaRotator) -> bool {
        let Some((first, last)) = self.visible_rows(self.offset()) else {
            return false;
        };
        match self.next_missing(first, last) {
            Some(i) => {
                self.ensure(i, rot);
                true
            }
            None => false,
        }
    }

    /// Refills the blank picture with the background as currently dimmed
    /// (the pixel dimming never touches it: it is not drawn by Slint).
    fn refresh_blank(&mut self) {
        if crate::ui::dim::changed_since(&mut self.blank_epoch) {
            let colour = crate::ui::dim::scale(self.background);
            // SAFETY: the layer's picture; its copies have finished (`finish`
            // waits for them), the next start after this.
            unsafe { self.blank.fill(colour) };
        }
    }

    /// The copies that show content y `scroll..scroll+height` in the frame
    /// buffer: a blank gap above the first row (scroll < 0), content from
    /// the ring (wrapping at most once), a blank gap below the last row.
    /// View y is the panel column; landscape x..x+width are panel rows
    /// `HEIGHT-x-width..`.
    fn copies(&self, scroll: i32) -> heapless::Vec<Blit, 4> {
        let Layout {
            x,
            y,
            width,
            height,
            ..
        } = self.layout;
        let view_h = height as i32;
        let row0 = HEIGHT - x - width;
        let content_h = self.content_h() as i32;
        let mut copies = heapless::Vec::new();
        let mut copy = |src, from, to, size| {
            let dst = self.fb;
            let _ = copies.push(Blit {
                src,
                from,
                dst,
                to,
                size,
            });
        };
        let top = scroll.max(0);
        let bottom = (scroll + view_h).min(content_h);
        if bottom > top {
            let dst = y + (top - scroll) as usize;
            let p = top as usize % self.layer.w;
            let n = (bottom - top) as usize;
            let n1 = n.min(self.layer.w - p);
            copy(self.layer, (p, 0), (dst, row0), (n1, width));
            if n1 < n {
                copy(self.layer, (0, 0), (dst + n1, row0), (n - n1, width));
            }
        }
        if scroll < 0 {
            let gap = ((-scroll) as usize).min(height);
            copy(self.blank, (0, 0), (y, row0), (gap, width));
        }
        if scroll + view_h > content_h {
            let start = (content_h - scroll).max(0) as usize;
            let gap = height - start;
            copy(self.blank, (0, 0), (y + start, row0), (gap, width));
        }
        copies
    }

    fn contains(&self, ev: &TouchEvent) -> bool {
        let l = &self.layout;
        (l.x as f32..(l.x + l.width) as f32).contains(&ev.x)
            && (l.y as f32..(l.y + l.height) as f32).contains(&ev.y)
    }

    /// Takes over from the page: the scroller starts where the page's list
    /// is, grabbed at view position `y`.
    fn start_scroll(&mut self, y: f32, at: u64) {
        self.scroller.set_content(self.content_h() as f32);
        if !self.composing {
            self.scroller.set_offset(self.source.offset());
            self.composing = true;
            self.shown = None;
            self.source.layer_started();
            if let Some((first, last)) = self.visible_rows(self.offset()) {
                for i in first..=last {
                    if self.source.changes_on_scroll(i) {
                        self.slot_row[i % self.layout.slots] = None;
                    }
                }
            }
        }
        self.scroller.press(y, at);
        self.settle = SETTLE_FRAMES;
    }

    /// Starts the copies that show the list at the current position, after
    /// rendering any visible row that is missing or stale.
    fn draw(&mut self, fb: Target, rot: &mut PpaRotator) {
        let scroll = self.offset();
        if let Some((first, last)) = self.visible_rows(scroll) {
            for i in first..=last {
                self.ensure(i, rot);
            }
        }
        self.refresh_blank();
        self.fb = Picture::frame_buffer(fb.buf);
        let copies = self.copies(scroll);
        self.start_copies(copies);
        self.drew = true;
    }

    /// Starts the first of `copies` now, to run while Slint renders the
    /// rest of the screen; the others follow in `finish`.
    fn start_copies(&mut self, copies: impl IntoIterator<Item = Blit>) {
        let mut copies = copies.into_iter();
        if let Some(first) = copies.next() {
            self.dma.start(first);
        }
        self.pending.clear();
        self.pending.extend(copies);
    }

    /// Blends the corner caps into the frame buffer `fb` (after the copies
    /// arrived).
    fn apply_caps(&self, fb: *mut u16) {
        let Some((_, outside)) = self.layout.corners else {
            return;
        };
        let bg = crate::ui::dim::scale(outside);
        for (start, weights) in &self.caps {
            let n = weights.len();
            // SAFETY: the runs lie inside the frame buffer (`caps`), whose
            // copies have arrived and which no DMA writes until the next
            // frame. The CPU writes it only here and writes each run back
            // at once, so the invalidated lines hold no other CPU writes.
            let run = unsafe {
                let px = fb.add(*start);
                cache::invalidate(px, n);
                core::slice::from_raw_parts_mut(px, n)
            };
            for (p, &a) in run.iter_mut().zip(weights) {
                if a > 0 {
                    *p = blend565(*p, bg, a);
                }
            }
            // SAFETY: the same run.
            unsafe { cache::writeback(run.as_mut_ptr(), n) };
        }
    }

    /// Hands the list back to the page where it is.
    fn stop_composing(&mut self) {
        let l = core::mem::take(&mut self.scroll_log);
        if let Some(since) = l.since {
            log::info!(
                "list layer: scroll {} frames in {} ms, {} late (worst {} ms, busy {} ms before), {} rows rendered ({} ms)",
                l.frames,
                since.elapsed().as_millis(),
                l.late,
                l.worst.0.as_millis(),
                l.worst.1.as_millis(),
                l.rows,
                l.rows_time.as_millis()
            );
        }
        self.composing = false;
        self.touch = Touch::None;
        let offset = self
            .scroller
            .offset()
            .round()
            .clamp(0.0, self.scroller.max_offset());
        self.source.scroll_ended(offset);
    }

    /// The row at view position `y` on the still list.
    fn row_at(&self, y: f32) -> Option<usize> {
        let y = y + self.source.offset();
        if y < 0.0 {
            return None;
        }
        let i = (y / self.layout.row_h as f32) as usize;
        (i < self.source.rows()).then_some(i)
    }

    /// Takes over a sideways touch on `row` at finger x `finger_x`.
    fn start_swipe(&mut self, row: usize, finger_x: f32) {
        self.source.layer_started();
        self.swipe = Some(Swipe {
            row,
            d: 0.0,
            finger_x0: finger_x,
            content: false,
            plain: false,
            highlighted: true,
            snap: None,
        });
        self.shown = None;
        self.settle = SETTLE_FRAMES;
    }

    /// The finger left a swiped row: delete it, or let it spring back.
    fn release_swipe(&mut self) {
        let width = self.layout.width as f32;
        let Some(s) = self.swipe.as_mut() else {
            return;
        };
        if s.d > width * DELETE_FRACTION {
            let row = s.row;
            self.swipe = None;
            self.source.swipe_ended(row, SwipeEnd::Delete);
            return;
        }
        s.snap = Some((s.d, Instant::now()));
        // The page shows the row plain when it takes over: so does the snap
        // (its content picture is rendered again, unlit).
        s.highlighted = false;
        s.content = false;
    }

    /// Renders `part` of row `row` into swipe picture slot `slot`.
    fn render_swipe_part(
        &mut self,
        row: usize,
        part: SwipePart,
        slot: usize,
        rot: &mut PpaRotator,
    ) {
        self.source.prepare_swipe(row, part);
        let target = Target {
            buf: self.swipe_pics.buf,
            pic_w: self.swipe_pics.w,
            pic_h: self.swipe_pics.h,
            col_off: slot * self.layout.row_h,
            row_ref: self.layout.width - 1,
            lines: self.layout.row_h,
        };
        platform::render_offscreen(&self.row_window, rot, target);
    }

    /// One frame of a swipe: renders missing pictures, advances a snap, and
    /// hands back once it has ended (returns false then).
    fn step_swipe(&mut self, rot: &mut PpaRotator) -> bool {
        let Some(mut s) = self.swipe else {
            return false;
        };
        let dim_changed = crate::ui::dim::changed_since(&mut self.underlay_epoch);
        if !self.underlay_ready || dim_changed {
            self.render_swipe_part(s.row, SwipePart::Underlay, UNDERLAY, rot);
            self.underlay_ready = true;
        }
        // One of the two content pictures per frame, the one shown first:
        // the first frame stays as short as possible.
        let want_plain = s.highlighted && !s.plain;
        let near = s.d.round() < self.layout.row_radius as f32;
        if want_plain && (near || s.content) {
            let part = SwipePart::Content {
                highlighted: true,
                underlay: false,
            };
            self.render_swipe_part(s.row, part, PLAIN, rot);
            s.plain = true;
        } else if !s.content {
            let part = SwipePart::Content {
                highlighted: s.highlighted,
                underlay: true,
            };
            self.render_swipe_part(s.row, part, CONTENT, rot);
            s.content = true;
        }
        let mut ended = false;
        if let Some((from, at)) = s.snap {
            let t = at.elapsed().as_micros() as f32 / SNAP.as_micros() as f32;
            ended = t >= 1.0;
            s.d = from * (1.0 - t).max(0.0).powi(3);
        }
        let d = s.d.round() as i32;
        if self.touch == Touch::Swipe || (s.snap.is_some() && !ended) || self.shown != Some(d) {
            self.settle = SETTLE_FRAMES;
        } else {
            self.settle = self.settle.saturating_sub(1);
        }
        self.shown = Some(d);
        self.swipe = Some(s);
        if ended && self.settle == 0 {
            // Both frame buffers show the row back in place.
            self.swipe = None;
            self.source.swipe_ended(s.row, SwipeEnd::Closed);
            return false;
        }
        true
    }

    /// Starts the copies that show the swiped row: its content moved `d`
    /// px left and the delete button in the gap. Moved less than the
    /// corner radius, the row is drawn plain (corners on the page
    /// background): lit from its picture, unlit from the ring.
    fn draw_swipe(&mut self, fb: Target, rot: &mut PpaRotator) {
        let Some(s) = self.swipe else {
            return;
        };
        let Layout {
            x,
            width,
            row_h,
            slots,
            ..
        } = self.layout;
        let d = (s.d.round().max(0.0) as usize).min(width);
        // The content picture has the red underlay behind its rounded
        // corners. Nearly closed, the left corners are on screen and the
        // right ones are over the underlay's own corners: draw the row
        // plain.
        let plain = d < self.layout.row_radius || !s.content;
        let ring = plain && !(s.highlighted && s.plain);
        if ring {
            self.ensure(s.row, rot);
        }
        self.fb = Picture::frame_buffer(fb.buf);
        self.pending.clear();
        let Some(band) = self.swipe_band() else {
            return;
        };
        // The band's first line in the row, its height and its panel column.
        let ry0 = (band.start as i32 - self.row_top(s.row)) as usize;
        let (rh, col) = (band.len(), band.start);
        let (content, content_col) = if ring {
            (self.layer, s.row % slots * row_h)
        } else if plain {
            (self.swipe_pics, PLAIN * row_h)
        } else {
            (self.swipe_pics, CONTENT * row_h)
        };
        // Row x `d..width` (picture rows `0..width-d`) to screen x
        // `x..x+width-d`; row x `width-d..width` of the delete button to
        // the rest. Landscape x is panel row `HEIGHT-1-x`.
        let content = (d < width).then_some(Blit {
            src: content,
            from: (content_col + ry0, 0),
            dst: self.fb,
            to: (col, HEIGHT - x - width + d),
            size: (rh, width - d),
        });
        let underlay = (d > 0).then_some(Blit {
            src: self.swipe_pics,
            from: (UNDERLAY * row_h + ry0, 0),
            dst: self.fb,
            to: (col, HEIGHT - x - width),
            size: (rh, d),
        });
        self.start_copies(content.into_iter().chain(underlay));
    }

    /// Landscape y of row `row`'s top on screen (outside the view if it is
    /// scrolled out).
    fn row_top(&self, row: usize) -> i32 {
        let Layout { y, row_h, .. } = self.layout;
        y as i32 + (row * row_h) as i32 - self.source.offset().round() as i32
    }

    /// Landscape rows of the swiped row on screen.
    fn swipe_band(&self) -> Option<Range<usize>> {
        let s = self.swipe?;
        let Layout {
            y, height, row_h, ..
        } = self.layout;
        let top = self.row_top(s.row);
        let a = top.max(y as i32);
        let b = (top + row_h as i32).min((y + height) as i32);
        (b > a).then_some(a as usize..b as usize)
    }
}

impl<S: ListSource> FrameHook for ListLayer<S> {
    fn active(&mut self) -> bool {
        if !self.layout.page_draws_still {
            return self.scroller.is_moving() || self.settle > 0;
        }
        if self.composing && !self.source.shown() {
            // The page went away mid-scroll.
            self.stop_composing();
        }
        if let Some(s) = self.swipe
            && !self.source.shown()
        {
            self.swipe = None;
            self.source.swipe_ended(s.row, SwipeEnd::Closed);
        }
        // Until it hands back (in `start`).
        self.composing || self.swipe.is_some()
    }

    fn pointer(&mut self, ev: &TouchEvent) -> Route {
        if !self.source.shown() {
            self.touch = Touch::None;
            return Route::Slint;
        }
        let at = ev.at.as_micros();
        let y = ev.y - self.layout.y as f32;
        match (ev.phase, self.touch) {
            (Phase::Down, _) if !self.contains(ev) => {
                self.touch = Touch::None;
                Route::Slint
            }
            (Phase::Down, _) if self.swipe.is_some() => {
                // A swiped row still snapping: this touch is ignored.
                self.touch = Touch::Ignored;
                Route::Hook
            }
            (Phase::Down, _) if self.composing => {
                // Stops the moving list; this touch only scrolls.
                self.start_scroll(y, at);
                self.touch = Touch::Scroll;
                Route::Hook
            }
            (Phase::Down, _) => {
                self.touch = Touch::Pending(ev.x, ev.y);
                Route::Slint
            }
            (Phase::Move, Touch::Pending(x0, y0)) => {
                let (dx, dy) = ((ev.x - x0).abs(), (ev.y - y0).abs());
                if dy > self.layout.slop && dy >= dx && self.content_h() <= self.layout.height {
                    // Nothing to scroll: the page ignores the drag.
                    self.touch = Touch::Slint;
                    Route::Slint
                } else if dy > self.layout.slop && dy >= dx {
                    self.start_scroll(y, at);
                    self.touch = Touch::Scroll;
                    Route::TakeOver
                } else if dx > self.layout.slop && !self.layout.swipe {
                    // No swiping here: the page has sideways drags.
                    self.touch = Touch::Slint;
                    Route::Slint
                } else if dx > self.layout.slop {
                    match self.row_at(y0 - self.layout.y as f32) {
                        Some(row) => {
                            self.start_swipe(row, x0);
                            self.touch = Touch::Swipe;
                            Route::TakeOver
                        }
                        None => {
                            self.touch = Touch::Slint;
                            Route::Slint
                        }
                    }
                } else {
                    Route::Slint
                }
            }
            (Phase::Move, Touch::Swipe) => {
                if let Some(s) = self.swipe.as_mut() {
                    let width = self.layout.width as f32;
                    s.d = (s.finger_x0 - ev.x).clamp(0.0, width);
                }
                Route::Hook
            }
            (Phase::Up, Touch::Swipe) => {
                self.touch = Touch::None;
                self.release_swipe();
                Route::Hook
            }
            (Phase::Move, Touch::Ignored) => Route::Hook,
            (Phase::Up, Touch::Ignored) => {
                self.touch = Touch::None;
                Route::Hook
            }
            (Phase::Move, Touch::Scroll) => {
                self.scroller.drag_to(y, at);
                Route::Hook
            }
            (Phase::Up, Touch::Scroll) => {
                self.touch = Touch::None;
                // A tap only stopped the list.
                let _ = self.scroller.release(y, at);
                self.settle = SETTLE_FRAMES;
                Route::Hook
            }
            (Phase::Up, _) => {
                self.touch = Touch::None;
                Route::Slint
            }
            (Phase::Move, _) => Route::Slint,
        }
    }

    fn start(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.frame_start = Instant::now();
        if self.composing {
            let l = &mut self.scroll_log;
            let now = self.frame_start;
            l.since.get_or_insert(now);
            if let Some(last) = l.last_start {
                let gap = now - last;
                if gap > LATE {
                    l.late += 1;
                }
                if gap > l.worst.0 {
                    l.worst = (gap, l.last_busy);
                }
            }
            l.last_start = Some(now);
            l.frames += 1;
        }
        self.pending.clear();
        self.drew = false;
        if self.swipe.is_some() {
            if self.step_swipe(rot) {
                self.draw_swipe(fb, rot);
            }
            return;
        }
        self.scroller.tick(Instant::now().as_micros());
        let scroll = self.offset();
        if self.scroller.is_moving() || self.shown != Some(scroll) {
            self.settle = SETTLE_FRAMES;
        } else {
            self.settle = self.settle.saturating_sub(1);
        }
        self.shown = Some(scroll);
        if self.layout.page_draws_still && self.settle == 0 && self.touch != Touch::Scroll {
            // Both frame buffers show the final position: the page takes
            // over and draws the same rows from this frame on.
            self.stop_composing();
            return;
        }
        self.draw(fb, rot);
    }

    fn repeat(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.dma.wait();
        if self.swipe.is_some() {
            self.draw_swipe(fb, rot);
        } else {
            self.draw(fb, rot);
        }
    }

    fn finish(&mut self, rot: &mut PpaRotator) {
        for blit in core::mem::take(&mut self.pending) {
            self.dma.start(blit);
        }
        // Fill the ring while the frame has time. Missing rows are never
        // visible ones (those were rendered in `start`), so the running copy
        // is not disturbed.
        while self.frame_start.elapsed() < FRAME_BUDGET && self.fill_one(rot) {}
        self.dma.wait();
        if self.drew && self.swipe.is_none() {
            self.apply_caps(self.fb.buf);
        }
        self.scroll_log.last_busy = self.frame_start.elapsed();
    }

    fn overlaps(&mut self, x: Range<usize>, y: Range<usize>) -> bool {
        let l = &self.layout;
        let in_x = x.start < l.x + l.width && l.x < x.end;
        if let Some(band) = self.swipe_band() {
            // Swiping: only the row's band is the layer's.
            return in_x && y.start < band.end && band.start < y.end;
        }
        self.composing && in_x && y.start < l.y + l.height && l.y < y.end
    }

    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool {
        // Nothing moves: render rows ahead, so the next scroll starts at
        // once.
        self.fill_one(rot)
    }
}

/// The runs of the four corner caps of `layout`'s area in the frame buffer:
/// each landscape column of a corner is one panel row (landscape x maps to
/// panel row `HEIGHT - 1 - x`, landscape y to the panel column).
fn caps(layout: &Layout, radius: usize) -> Vec<(usize, Vec<u8>)> {
    let alpha = corner_alpha(radius);
    let (x0, y0, w, h) = (layout.x, layout.y, layout.width, layout.height);
    let mut runs = Vec::new();
    // Landscape column `cx` (0..radius from the corner's side), the
    // corner's y offset and whether it is at the top, bottom / left, right.
    for (left, top) in [(true, true), (false, true), (true, false), (false, false)] {
        for i in 0..radius {
            let lx = if left { x0 + i } else { x0 + w - 1 - i };
            let ly = if top { y0 } else { y0 + h - radius };
            // Weights along y in the corner, top to bottom on screen.
            let weights: Vec<u8> = (0..radius)
                .map(|k| {
                    let j = if top { k } else { radius - 1 - k };
                    alpha[j * radius + i]
                })
                .collect();
            runs.push(((HEIGHT - 1 - lx) * WIDTH + ly, weights));
        }
    }
    runs
}

/// Two list layers whose pages are never on screen together, sharing one
/// set of buffers ([`ListLayer::new_in`]): the one shown owns them (with
/// neither shown, `a`, so its rows are ready when its page opens). A new
/// owner renders its rows again ([`ListLayer::invalidate`]).
pub struct SharedLayers<A: ListSource, B: ListSource> {
    a: ListLayer<A>,
    b: ListLayer<B>,
    /// `b` owns the buffers.
    b_owns: bool,
}

impl<A: ListSource, B: ListSource> SharedLayers<A, B> {
    pub fn new(a: ListLayer<A>, b: ListLayer<B>) -> Self {
        Self {
            a,
            b,
            b_owns: false,
        }
    }

    /// Hands the buffers to the layer shown (else `a`).
    fn claim(&mut self) {
        let b = self.b.is_shown();
        if b != self.b_owns {
            self.b_owns = b;
            if b {
                self.b.invalidate();
            } else {
                self.a.invalidate();
            }
        }
    }

    fn owner(&mut self) -> &mut dyn FrameHook {
        if self.b_owns {
            &mut self.b
        } else {
            &mut self.a
        }
    }
}

impl<A: ListSource, B: ListSource> FrameHook for SharedLayers<A, B> {
    fn active(&mut self) -> bool {
        // Both: one whose page went away mid-scroll hands back.
        let (a, b) = (self.a.active(), self.b.active());
        self.claim();
        if self.b_owns { b } else { a }
    }

    fn pointer(&mut self, ev: &TouchEvent) -> Route {
        self.claim();
        self.owner().pointer(ev)
    }

    fn start(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.owner().start(fb, rot);
    }

    fn repeat(&mut self, fb: Target, rot: &mut PpaRotator) {
        self.owner().repeat(fb, rot);
    }

    fn finish(&mut self, rot: &mut PpaRotator) {
        self.owner().finish(rot);
    }

    fn overlaps(&mut self, x: Range<usize>, y: Range<usize>) -> bool {
        self.owner().overlaps(x, y)
    }

    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool {
        self.claim();
        self.owner().idle_work(rot)
    }
}
