//! Page transitions: the new page slides in, pushing the old one out,
//! composed by the 2D-DMA. Slint renders a page change in 40–60 ms, too slow
//! to animate it frame by frame; instead the new page is rendered once into
//! a picture of its own and the two pictures are copied into the frame
//! buffer at moving offsets (~14 ms of DMA per frame, the panel's rate).
//!
//! The app calls [`request`] right before it switches the page; the render
//! loop then runs [`Transitions::run`] instead of its next frame:
//! 1. the screen as shown is copied into the "old" picture, and into the
//!    "new" one;
//! 2. Slint renders its dirty region (the new page) into "new" through the
//!    PPA, as it would into a frame buffer;
//! 3. for [`DURATION`] each frame shows "new" sliding in over "old"
//!    (forward: from the right; back: from the left);
//! 4. both frame buffers get "new", so they hold what Slint believes it
//!    drew, and it carries on with partial repaints.
//!
//! Touches during the transition are dropped.

use super::input;
use super::platform::PpaLines;
use crate::display::dma2d::{FbCopy, Picture};
use crate::display::ppa::{PpaRotator, Target};
use crate::display::{Display, HEIGHT, PIXELS, WIDTH};
use core::sync::atomic::{AtomicU8, Ordering};
use embassy_time::{Duration, Instant};
use esp_idf_svc::sys;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RenderingRotation};

/// Length of the slide.
const DURATION: Duration = Duration::from_millis(280);

/// Where the new page comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Deeper (list → alarm → sound): from the right.
    Forward,
    /// Back up: from the left.
    Back,
}

/// 0 none, 1 forward, 2 back.
static PENDING: AtomicU8 = AtomicU8::new(0);

/// The page is about to change: animate it. Call from the UI thread, right
/// before the change.
pub fn request(direction: Direction) {
    let v = match direction {
        Direction::Forward => 1,
        Direction::Back => 2,
    };
    PENDING.store(v, Ordering::Relaxed);
}

/// The transition requested since the last call, if any.
pub(super) fn take() -> Option<Direction> {
    match PENDING.swap(0, Ordering::Relaxed) {
        1 => Some(Direction::Forward),
        2 => Some(Direction::Back),
        _ => None,
    }
}

/// The pictures and the DMA for transitions.
pub(super) struct Transitions {
    old: Picture,
    new: Picture,
    dma: FbCopy,
}

impl Transitions {
    pub fn new() -> Self {
        let alloc = || {
            let p = unsafe {
                sys::heap_caps_aligned_alloc(
                    128,
                    PIXELS * 2,
                    sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_DMA,
                )
            };
            assert!(!p.is_null(), "no PSRAM for page transitions");
            Picture {
                buf: p.cast(),
                w: WIDTH,
                h: HEIGHT,
            }
        };
        Self {
            old: alloc(),
            new: alloc(),
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
        let started = Instant::now();
        let front = picture(display.front_buffer());
        self.copy(front, self.old);
        self.copy(front, self.new);
        self.dma.wait();

        // SAFETY: `new` holds PIXELS pixels; only the PPA writes it now.
        let new = unsafe { core::slice::from_raw_parts_mut(self.new.buf, PIXELS) };
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(RenderingRotation::NoRotation);
            rot.begin(Target::frame_buffer(new));
            renderer.render_by_line(PpaLines { rot: &mut *rot });
            rot.finish();
        });
        let rendered = started.elapsed();

        let slide = Instant::now();
        let mut frames = 0;
        loop {
            let t = slide.elapsed().as_micros() as f32 / DURATION.as_micros() as f32;
            if t >= 1.0 {
                break;
            }
            // Ease out (cubic): fast start, gentle stop.
            let eased = 1.0 - (1.0 - t).powi(3);
            let d = ((eased * HEIGHT as f32) as usize).min(HEIGHT);
            let fb = picture(display.back_buffer().await.as_mut_ptr());
            self.compose(fb, d, direction);
            display.present(0..1).expect("present");
            frames += 1;
        }
        // Both buffers show the new page.
        for _ in 0..2 {
            let fb = picture(display.back_buffer().await.as_mut_ptr());
            self.copy(self.new, fb);
            self.dma.wait();
            display.present(0..1).expect("present");
        }
        while input::EVENTS.try_receive().is_ok() {}
        log::info!(
            "transition: {direction:?}, new page rendered in {} ms, {frames} frames in {} ms",
            rendered.as_millis(),
            slide.elapsed().as_millis()
        );
    }

    /// One frame of the slide, `d` landscape pixels in: forward, the old
    /// page moves left and the new one follows from the right; back, the
    /// other way round.
    fn compose(&mut self, fb: Picture, d: usize, direction: Direction) {
        let w = HEIGHT;
        match direction {
            Direction::Forward => {
                self.strip(self.old, d, fb, 0, w - d);
                self.strip(self.new, 0, fb, w - d, d);
            }
            Direction::Back => {
                self.strip(self.new, w - d, fb, 0, d);
                self.strip(self.old, 0, fb, d, w - d);
            }
        }
        self.dma.wait();
    }

    /// Copies landscape columns `from..from+n` of `src` to `to..to+n` of
    /// `dst`. Landscape x is the panel row `HEIGHT-1-x`, so a column range
    /// is a block of whole panel rows.
    fn strip(&mut self, src: Picture, from: usize, dst: Picture, to: usize, n: usize) {
        if n == 0 {
            return;
        }
        self.dma.start(
            src,
            (0, HEIGHT - from - n),
            dst,
            (0, HEIGHT - to - n),
            (WIDTH, n),
        );
    }

    fn copy(&mut self, src: Picture, dst: Picture) {
        self.dma.start(src, (0, 0), dst, (0, 0), (WIDTH, HEIGHT));
    }
}

fn picture(buf: *mut u16) -> Picture {
    Picture {
        buf,
        w: WIDTH,
        h: HEIGHT,
    }
}
