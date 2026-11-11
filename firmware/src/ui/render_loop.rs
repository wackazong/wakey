//! The render loop: Slint's main window plus an optional [`FrameHook`],
//! paced by the panel, sleeping while nothing changes.
//!
//! Each frame: route touch events (hook first, then Slint), let the hook
//! start its work (e.g. a DMA copy), render Slint's dirty region through
//! the PPA into the back buffer, let the hook finish, flip. When nothing
//! animates, the hook gets background time, then the loop sleeps until
//! the next Slint timer or touch.

use super::input::{self, Phase, TouchEvent};
use super::platform::PpaLines;
use super::stats::FrameStats;
use crate::display::Display;
use crate::display::ppa::{PpaRotator, Target};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RenderingRotation};
use std::rc::Rc;

/// Where a touch event goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Route {
    /// To the main Slint window.
    Slint,
    /// The hook used it.
    Hook,
    /// The hook takes over the touch Slint has had so far: Slint gets a
    /// cancel (the pointer left the window); this and the following events
    /// of the touch are the hook's.
    TakeOver,
}

/// Work added to every frame, around Slint's own rendering. While active,
/// the hook draws its area of the screen; if Slint renders there in the same
/// frame, the hook draws again on top (`overlaps`).
pub trait FrameHook {
    /// True while the hook changes the screen every frame (keeps the loop
    /// running at the panel rate even when Slint is idle).
    fn active(&mut self) -> bool;
    /// Before Slint renders: start drawing into the back buffer `fb` (e.g.
    /// an asynchronous DMA copy). `rot` is idle and may be used.
    fn start(&mut self, fb: Target, rot: &mut PpaRotator);
    /// Draws the current state again, with no time passing (Slint has just
    /// rendered over it); `finish` follows.
    fn repeat(&mut self, fb: Target, rot: &mut PpaRotator);
    /// After Slint has rendered: optional extra work with the idle `rot`,
    /// then wait until everything started in `start` is in the back buffer.
    fn finish(&mut self, rot: &mut PpaRotator);
    /// A touch event, before Slint sees it.
    fn pointer(&mut self, ev: &TouchEvent) -> Route;
    /// True if the landscape area `x` × `y` that Slint just rendered
    /// overlaps what the hook draws. Then the loop draws the hook again
    /// after Slint, in the same frame.
    fn overlaps(&mut self, x: core::ops::Range<usize>, y: core::ops::Range<usize>) -> bool;
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

/// Where the render loop is (for the stall report): see [`stage`].
pub static STAGE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
/// Loop iterations since boot.
pub static ITERATIONS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn stage(s: u32) {
    STAGE.store(s, core::sync::atomic::Ordering::Relaxed);
}

/// Stage names, indexed by [`STAGE`].
pub const STAGES: [&str; 7] = [
    "start",
    "touch",
    "timers",
    "back buffer",
    "render",
    "present",
    "sleep",
];

/// Renders the main window, plus `hook`, forever.
pub async fn run(
    window: Rc<MinimalSoftwareWindow>,
    mut display: Display,
    rot: &mut PpaRotator,
    mut hook: Option<&mut dyn FrameHook>,
) -> ! {
    let mut stats = FrameStats::new("slint/ppa");
    let mut transitions = super::transition::Transitions::new();
    // An event that woke the idle loop, handled in the next iteration.
    let mut woken_by: Option<TouchEvent> = None;
    // Slint renders into the two frame buffers in turn and repaints only
    // what changed since each one was last drawn. After a change, the other
    // buffer is brought up to date with one more render right away;
    // otherwise that happens with the next change, often a touch, whose
    // first frame would then be slow (a page switch: ~60 ms).
    let mut other_buffer_stale = false;
    loop {
        ITERATIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        stage(1);
        // Touch: route each event to the hook (e.g. list drags), or Slint.
        while let Some(ev) = woken_by.take().or_else(|| input::EVENTS.try_receive().ok()) {
            let route = hook.as_mut().map_or(Route::Slint, |h| h.pointer(&ev));
            if DEBUG_TOUCH {
                log::info!(
                    "touch event {:?} ({:.0}, {:.0}) → {route:?}",
                    ev.phase,
                    ev.x,
                    ev.y
                );
            }
            match route {
                Route::Slint => dispatch(&window, &ev),
                Route::Hook => {}
                // A pressed touch area gets `PointerEventKind.cancel`.
                Route::TakeOver => {
                    window.dispatch_event(slint::platform::WindowEvent::PointerExited)
                }
            }
        }
        stage(2);
        slint::platform::update_timers_and_animations();
        // A page change requested during the touch handling above: shown
        // as a slide, which also renders the new page into both buffers.
        if let Some(direction) = super::transition::take() {
            stage(4);
            transitions.run(direction, &window, &mut display, rot).await;
            // Slint drew the new page once and believes one buffer holds
            // it; both do, but its next render repaints that region again
            // (it assumes the buffers alternate). Do that now, while
            // nothing moves, not at the first touch.
            other_buffer_stale = true;
            continue;
        }
        let hook_active = hook.as_mut().is_some_and(|h| h.active());
        let catching_up = other_buffer_stale;
        if catching_up {
            window.request_redraw();
        }
        if hook_active {
            // Slint's `SwappedBuffers` assumes one render per flip: render
            // (usually nothing) every frame the hook makes us flip.
            window.request_redraw();
        }

        // Waits for the previous flip, so this paces to the panel refresh.
        stage(3);
        let target = Target::frame_buffer(display.back_buffer().await);

        stage(4);
        let t0 = Instant::now();
        #[cfg(feature = "ui-bench")]
        let trace = super::trace::Frame::start();
        let mut hook_started = false;
        if hook_active && let Some(h) = hook.as_mut() {
            h.start(target, rot);
            hook_started = true;
        }
        // Bounding box (x, y, width, height) of what Slint rendered, if it
        // rendered anything.
        let mut slint_box: Option<(i32, i32, u32, u32)> = None;
        // Slint rendered, if only an empty region: the buffers must flip.
        let mut slint_rendered = false;
        window.draw_if_needed(|renderer| {
            slint_rendered = true;
            renderer.set_rendering_rotation(RenderingRotation::NoRotation);
            rot.begin(target);
            let region = renderer.render_by_line(PpaLines { rot: &mut *rot });
            rot.finish();
            let (o, size) = (region.bounding_box_origin(), region.bounding_box_size());
            if size.width > 0 && size.height > 0 {
                slint_box = Some((o.x, o.y, size.width, size.height));
            }
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
        // Landscape area Slint rendered.
        let slint_area = slint_box.map(|(x, y, w, h)| {
            let (x, y) = (x.max(0) as usize, y.max(0) as usize);
            (x..x + w as usize, y..y + h as usize)
        });
        if let Some(h) = hook.as_mut() {
            // Slint painted over the hook's area (e.g. its full repaints in
            // the first frames): its strips may have landed after the hook's
            // copy, so draw the hook again on top, in this frame.
            if let Some((x, y)) = slint_area.clone()
                && h.overlaps(x, y)
            {
                if hook_started {
                    h.finish(rot);
                }
                h.repeat(target, rot);
                hook_started = true;
            }
            if hook_started {
                h.finish(rot);
            }
        }

        other_buffer_stale = slint_box.is_some() && !catching_up;
        if slint_rendered || hook_started {
            stage(5);
            let render_time = t0.elapsed();
            // Always flip after a render, even an empty one (see above). The
            // CPU never wrote the frame buffer (PPA/DMA did): nothing to
            // write back from the cache, so only one row is synced (the
            // driver needs a non-empty area to switch buffers).
            display.present(0..1).expect("present");
            stats.frame(render_time, slint_area.map_or(0, |(x, _)| x.len()));
            #[cfg(feature = "ui-bench")]
            trace.presented(slint_box.unwrap_or_default());
        }

        if !window.has_active_animations() && !hook_active && !other_buffer_stale {
            stats.maybe_log();
            // Background work first (one piece), then sleep until the next
            // Slint timer or a touch.
            if hook.as_mut().is_some_and(|h| h.idle_work(rot)) {
                continue;
            }
            stage(6);
            let wait = slint::platform::duration_until_next_timer_update()
                .map(|d| Duration::from_micros(d.as_micros() as u64))
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100));
            if let Either::Second(ev) = select(Timer::after(wait), input::EVENTS.receive()).await {
                woken_by = Some(ev);
            }
        }
    }
}
