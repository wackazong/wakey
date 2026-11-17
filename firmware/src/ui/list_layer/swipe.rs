//! Swiping a row sideways to delete it: the layer moves pre-rendered
//! pictures of the row and of the delete button behind it by DMA (see the
//! module docs of [`super`]).

use super::{Layout, ListLayer, ListSource, SETTLE_FRAMES, Touch};
use crate::display::HEIGHT;
use crate::display::dma2d::{Blit, Picture};
use crate::display::ppa::{PpaRotator, Target};
use crate::ui::{dim, platform};
use core::ops::Range;
use embassy_time::{Duration, Instant};

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
    /// Released past `DELETE_FRACTION` of the width: delete the row.
    Delete,
}

/// A row being swiped (by the finger, or snapping after release).
#[derive(Clone, Copy, Debug)]
pub(super) struct Swipe {
    pub(super) row: usize,
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

/// The swipe pictures, one row slot each: the row on the delete button's
/// red, the delete button, and the lit row as the page draws it.
pub(super) const PICTURES: usize = 3;
const CONTENT: usize = 0;
const UNDERLAY: usize = 1;
const PLAIN: usize = 2;

impl<S: ListSource> ListLayer<S> {
    /// Takes over a sideways touch on `row` at finger x `finger_x`.
    pub(super) fn start_swipe(&mut self, row: usize, finger_x: f32) {
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

    /// The finger moved to x `finger_x`: the row follows it.
    pub(super) fn drag_swipe(&mut self, finger_x: f32) {
        if let Some(s) = self.swipe.as_mut() {
            s.d = (s.finger_x0 - finger_x).clamp(0.0, self.layout.width as f32);
        }
    }

    /// The finger left a swiped row: delete it, or let it spring back.
    pub(super) fn release_swipe(&mut self) {
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
        let target = self.row_target(self.swipe_pics, slot);
        platform::render_offscreen(&self.row_window, rot, target);
    }

    /// One frame of a swipe: renders missing pictures, advances a snap, and
    /// hands back once it has ended (returns false then).
    pub(super) fn step_swipe(&mut self, rot: &mut PpaRotator) -> bool {
        let Some(mut s) = self.swipe else {
            return false;
        };
        let dim_changed = dim::changed_since(&mut self.underlay_epoch);
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
    pub(super) fn draw_swipe(&mut self, fb: Target, rot: &mut PpaRotator) {
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
    pub(super) fn swipe_band(&self) -> Option<Range<usize>> {
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
