//! Slint on the Tab5: a `Platform`, and an async render loop that renders
//! into the DPI back buffer through the PPA.
//!
//! - `RepaintBufferType::SwappedBuffers`: Slint repaints only what changed in
//!   the last two frames, matching the two frame buffers.
//! - Slint renders unrotated landscape lines into strips in internal RAM; the
//!   PPA rotates them into the frame buffer (`display::ppa`).
//! - A [`FrameHook`] can add to each frame after Slint, e.g. the scrolling
//!   list layer (`ui::list_layer`), which is composed by DMA, not by Slint.
//! - Besides the main window, components can get offscreen windows
//!   ([`offscreen_window`]) that are rendered on demand into any PPA target.
//! - The loop sleeps until the next Slint timer when nothing animates.

// `PENDING` below already uses a `const` initializer; clippy's lint misfires
// on the ESP-IDF target.
#![allow(clippy::missing_const_for_thread_local)]

use super::touch::{self, Phase, TouchEvent};
use super::{UI_HEIGHT, UI_WIDTH, stats::FrameStats};
use crate::display::Display;
use crate::display::ppa::{PpaRotator, Target};
use embassy_futures::select::{Either, select};

use embassy_time::{Duration, Instant, Timer};
use slint::PlatformError;
use slint::platform::software_renderer::{
    LineBufferProvider, MinimalSoftwareWindow, RenderingRotation, RepaintBufferType, Rgb565Pixel,
};
use slint::platform::{Platform, WindowAdapter};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

thread_local! {
    /// Windows handed out, in order, to the next components created.
    static PENDING: RefCell<VecDeque<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(VecDeque::new()) };
}

struct TabPlatform {
    start: Instant,
}

impl Platform for TabPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        PENDING
            .with(|p| p.borrow_mut().pop_front())
            .map(|w| w as Rc<dyn WindowAdapter>)
            .ok_or_else(|| PlatformError::Other("no window prepared for this component".into()))
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros(self.start.elapsed().as_micros())
    }

    fn debug_log(&self, arguments: core::fmt::Arguments) {
        log::info!("slint: {arguments}");
    }
}

/// Installs the platform and prepares the main (full-screen) window, which
/// the next component created will use. Call once, first.
pub fn init() -> Rc<MinimalSoftwareWindow> {
    slint::platform::set_platform(Box::new(TabPlatform {
        start: Instant::now(),
    }))
    .expect("Slint platform already set");
    let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
    window.set_size(slint::PhysicalSize::new(UI_WIDTH, UI_HEIGHT));
    PENDING.with(|p| p.borrow_mut().push_back(window.clone()));
    window
}

/// Prepares an offscreen window of `width × height` landscape pixels for
/// the next component created. Render it with [`render_offscreen`].
pub fn offscreen_window(width: u32, height: u32) -> Rc<MinimalSoftwareWindow> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    window.set_size(slint::PhysicalSize::new(width, height));
    PENDING.with(|p| p.borrow_mut().push_back(window.clone()));
    window
}

/// Renders all of an offscreen window into `target` and waits until the PPA
/// has written it.
pub fn render_offscreen(window: &MinimalSoftwareWindow, rot: &mut PpaRotator, target: Target) {
    window.request_redraw();
    rot.begin(target);
    window.draw_if_needed(|renderer| {
        renderer.set_rendering_rotation(RenderingRotation::NoRotation);
        renderer.render_by_line(PpaLines { rot: &mut *rot });
    });
    rot.finish();
}

/// Work added to every frame, around Slint's own rendering. The hook must
/// draw only into screen areas Slint leaves alone, since both write the back
/// buffer at the same time.
pub trait FrameHook {
    /// True while the hook changes the screen every frame (keeps the loop
    /// running at the panel rate even when Slint is idle).
    fn active(&self) -> bool;
    /// Before Slint renders: start drawing into the back buffer `fb` (e.g.
    /// an asynchronous DMA copy). `rot` is idle and may be used.
    fn start(&mut self, fb: Target, rot: &mut PpaRotator);
    /// After Slint has rendered: optional extra work with the idle `rot`,
    /// then wait until everything started in `start` is in the back buffer.
    fn finish(&mut self, rot: &mut PpaRotator);
    /// A touch event; return true if the hook consumed it (otherwise it goes
    /// to the main Slint window).
    fn pointer(&mut self, ev: &TouchEvent) -> bool;
    /// True if the landscape columns `x` overlap the hook's area. Then the
    /// loop draws the hook again after Slint, in the same frame.
    fn overlaps(&mut self, x: core::ops::Range<usize>) -> bool;
    /// Called when nothing is drawn; do a small piece of background work
    /// (e.g. pre-render list rows). Return true if there is more to do.
    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool;
}

/// Sends a touch event to a Slint window.
fn dispatch(window: &MinimalSoftwareWindow, ev: &TouchEvent) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let position = slint::LogicalPosition::new(ev.x, ev.y);
    let button = PointerEventButton::Left;
    match ev.phase {
        Phase::Down => window.dispatch_event(WindowEvent::PointerPressed { position, button }),
        Phase::Move => window.dispatch_event(WindowEvent::PointerMoved { position }),
        Phase::Up => {
            window.dispatch_event(WindowEvent::PointerReleased { position, button });
            window.dispatch_event(WindowEvent::PointerExited);
        }
    }
}

/// Log every touch event and where it went.
const DEBUG_TOUCH: bool = false;

/// Log the dirty rectangles of frames that take longer than 35 ms.
const DEBUG_SLOW_FRAMES: bool = true;

struct PpaLines<'a> {
    rot: &'a mut PpaRotator,
}

impl LineBufferProvider for PpaLines<'_> {
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        let px = self.rot.line(line, range);
        // SAFETY: Rgb565Pixel is #[repr(transparent)] over u16.
        render_fn(unsafe { core::slice::from_raw_parts_mut(px.as_mut_ptr().cast(), px.len()) });
    }
}

/// Renders the main window, plus `hook`, forever.
pub async fn run(
    window: Rc<MinimalSoftwareWindow>,
    mut display: Display,
    rot: &mut PpaRotator,
    mut hook: Option<&mut dyn FrameHook>,
) -> ! {
    let mut stats = FrameStats::new("slint/ppa");
    // An event that woke the idle loop, handled in the next iteration.
    let mut woken_by: Option<TouchEvent> = None;
    loop {
        // Touch: route each event to the hook (e.g. list drags), or Slint.
        while let Some(ev) = woken_by.take().or_else(|| touch::EVENTS.try_receive().ok()) {
            let consumed = hook.as_mut().is_some_and(|h| h.pointer(&ev));
            if DEBUG_TOUCH {
                log::info!(
                    "touch event {:?} ({:.0}, {:.0}) → {}",
                    ev.phase,
                    ev.x,
                    ev.y,
                    if consumed { "hook" } else { "slint" }
                );
            }
            if !consumed {
                dispatch(&window, &ev);
            }
        }
        slint::platform::update_timers_and_animations();
        let hook_active = hook.as_ref().is_some_and(|h| h.active());
        if hook_active {
            // Slint's `SwappedBuffers` assumes one render per flip: render
            // (usually nothing) every frame the hook makes us flip.
            window.request_redraw();
        }

        // Waits for the previous flip, so this paces to the panel refresh.
        let target = Target::frame_buffer(display.back_buffer().await);

        let t0 = Instant::now();
        let mut hook_started = false;
        if hook_active && let Some(h) = hook.as_mut() {
            h.start(target, rot);
            hook_started = true;
        }
        let mut slint_drawn = false;
        let mut rows = 0;
        let mut slint_x = 0..0;
        window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(RenderingRotation::NoRotation);
            rot.begin(target);
            let region = renderer.render_by_line(PpaLines { rot: &mut *rot });
            rot.finish();
            slint_drawn = true;
            rows = region.bounding_box_size().width as usize;
            let x0 = region.bounding_box_origin().x.max(0) as usize;
            slint_x = x0..x0 + rows;
            if DEBUG_SLOW_FRAMES && t0.elapsed() > Duration::from_millis(35) {
                let rects: Vec<_> = region
                    .iter()
                    .map(|(p, s)| (p.x, p.y, s.width, s.height))
                    .collect();
                log::warn!(
                    "slint: slow frame {} ms, dirty {:?}",
                    t0.elapsed().as_millis(),
                    rects
                );
            }
        });
        if let Some(h) = hook.as_mut() {
            // Slint painted over the hook's area (e.g. its full repaints in
            // the first frames): its strips may have landed after the hook's
            // copy, so draw the hook again on top, in this frame.
            if !slint_x.is_empty() && h.overlaps(slint_x.clone()) {
                if hook_started {
                    h.finish(rot);
                }
                h.start(target, rot);
                hook_started = true;
            }
            if hook_started {
                h.finish(rot);
            }
        }

        if slint_drawn || hook_started {
            let render_time = t0.elapsed();
            // Always flip after a render, even an empty one (see above). The
            // CPU never wrote the frame buffer (PPA/DMA did): nothing to
            // write back from the cache, so only one row is synced (the
            // driver needs a non-empty area to switch buffers).
            display.present(0..1).expect("present");
            stats.frame(render_time, rows);
        }

        if !window.has_active_animations() && !hook_active {
            stats.maybe_log();
            // Background work first (one piece), then sleep until the next
            // Slint timer or a touch.
            if hook.as_mut().is_some_and(|h| h.idle_work(rot)) {
                continue;
            }
            let wait = slint::platform::duration_until_next_timer_update()
                .map(|d| Duration::from_micros(d.as_micros() as u64))
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100));
            if let Either::Second(ev) = select(Timer::after(wait), touch::EVENTS.receive()).await {
                woken_by = Some(ev);
            }
        }
    }
}
