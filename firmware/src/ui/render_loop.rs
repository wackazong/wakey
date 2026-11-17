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
use super::power;
use super::stats::{FrameStats, FreeHeap};
use super::transition::{self, Transitions};
use crate::display::Display;
use crate::display::dma2d::Picture;
use crate::display::ppa::{PpaRotator, Target};
use core::ops::Range;
use core::sync::atomic::{AtomicU32, Ordering};
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, RenderingRotation, RepaintBufferType,
};
use slint::platform::{PointerEventButton, WindowEvent};
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
    /// A touch event, before Slint sees it.
    fn pointer(&mut self, ev: &TouchEvent) -> Route;
    /// Before Slint renders: start drawing into the back buffer `fb` (e.g.
    /// an asynchronous DMA copy). `rot` is idle and may be used.
    fn start(&mut self, fb: Target, rot: &mut PpaRotator);
    /// Draws the current state again, with no time passing (Slint has just
    /// rendered over it); `finish` follows.
    fn repeat(&mut self, fb: Target, rot: &mut PpaRotator);
    /// After Slint has rendered: optional extra work with the idle `rot`,
    /// then wait until everything started in `start` is in the back buffer.
    fn finish(&mut self, rot: &mut PpaRotator);
    /// True if the landscape area `x` × `y` that Slint just rendered
    /// overlaps what the hook draws. Then the loop draws the hook again
    /// after Slint, in the same frame.
    fn overlaps(&mut self, x: Range<usize>, y: Range<usize>) -> bool;
    /// Called when nothing is drawn; do a small piece of background work
    /// (e.g. pre-render list rows). Return true if there is more to do.
    fn idle_work(&mut self, rot: &mut PpaRotator) -> bool;
}

/// Sends a touch event to a Slint window.
fn dispatch(window: &MinimalSoftwareWindow, ev: &TouchEvent) {
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

/// A Slint render longer than this logs its dirty rectangles.
const SLOW_FRAME: Duration = Duration::from_millis(35);
/// Pixel clock while the display sleeps: 33 Hz instead of 66 Hz.
const SLEEP_PIXEL_CLOCK_MHZ: f32 = 40.0;
/// The longest the loop sleeps. While the display is asleep it waits this
/// long each time and then runs Slint's timers (the alarm tick, the power
/// timer that wakes the display): nothing else would run them.
const MAX_SLEEP: Duration = Duration::from_millis(100);

/// Declares [`Stage`] with its names and the list of all stages in one
/// place, so a new stage cannot be missing from the list (and be reported
/// as "start").
macro_rules! stages {
    ($($stage:ident => $name:literal),* $(,)?) => {
        /// Where the render loop is, for the stall report ([`stage`]).
        #[derive(Clone, Copy, Debug, PartialEq)]
        #[repr(u32)]
        pub enum Stage {
            $($stage),*
        }

        impl Stage {
            const ALL: &[Self] = &[$(Self::$stage),*];

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$stage => $name),*
                }
            }
        }
    };
}

stages! {
    Start => "start",
    Touch => "touch",
    Timers => "timers",
    BackBuffer => "back buffer",
    Render => "render",
    Present => "present",
    Sleep => "sleep",
}

static STAGE: AtomicU32 = AtomicU32::new(Stage::Start as u32);
/// Loop iterations since boot.
pub static ITERATIONS: AtomicU32 = AtomicU32::new(0);

fn enter(stage: Stage) {
    STAGE.store(stage as u32, Ordering::Relaxed);
}

/// Where the render loop is now (any thread).
pub fn stage() -> Stage {
    let v = STAGE.load(Ordering::Relaxed);
    Stage::ALL
        .iter()
        .copied()
        .find(|s| *s as u32 == v)
        .unwrap_or(Stage::Start)
}

/// Feature `sleep-cycle-test`: the free memory around each link rebuild
/// (one leaking or fragmenting PSRAM would show over many cycles).
fn log_heap_for_test(when: &str) {
    if cfg!(feature = "sleep-cycle-test") {
        log::info!("display: {when} link rebuild: {}", FreeHeap::now());
    }
}

/// Puts the display to sleep, or starts waking it (awake once the next
/// frame is presented).
///
/// The panel itself stays on: its touch half (the same chip) stops scanning
/// in sleep mode, and nothing could wake it. The backlight off is dark
/// enough.
///
/// Asleep, the panel refreshes at half rate (33 Hz): half the PSRAM reads,
/// and its touch still scans. It shows nothing at that rate, which does not
/// matter with the backlight off. A rate change rebuilds the whole display
/// link (a new stream alone leaves the panel black), which clears the frame
/// buffers.
async fn switch_refresh(display: &mut Display, sleep: bool) {
    log_heap_for_test("before");
    if sleep {
        power::set_asleep(true);
        // The light meter thread turns the backlight off first.
        Timer::after(Duration::from_millis(150)).await;
        let t = Instant::now();
        if let Err(e) = display.set_pixel_clock(Some(SLEEP_PIXEL_CLOCK_MHZ)) {
            log::warn!("display: sleep refresh not set: {e}");
        }
        log::info!(
            "display: asleep, link rebuilt in {} ms",
            t.elapsed().as_millis()
        );
    } else {
        let t = Instant::now();
        if let Err(e) = display.set_pixel_clock(None) {
            log::warn!("display: refresh not restored: {e}");
        }
        log::info!(
            "display: waking, link rebuilt in {} ms",
            t.elapsed().as_millis()
        );
    }
    log_heap_for_test("after");
}

/// Renders the main window, plus `hook`, forever.
pub async fn run(
    window: Rc<MinimalSoftwareWindow>,
    mut display: Display,
    rot: &mut PpaRotator,
    mut hook: Option<&mut dyn FrameHook>,
) -> ! {
    let mut stats = FrameStats::new("slint/ppa");
    let mut transitions = Transitions::new();
    // An event that woke the idle loop, handled in the next iteration.
    let mut woken_by: Option<TouchEvent> = None;
    // Slint renders into the two frame buffers in turn and repaints only
    // what changed since each one was last drawn. After a change, the other
    // buffer is brought up to date with one more render right away;
    // otherwise that happens with the next change, often a touch, whose
    // first frame would then be slow (a page switch: ~60 ms).
    let mut other_buffer_stale = false;
    // The touch that is down changed the page (buttons that do so act on
    // press): the rest of it is dropped, or its release would act on the
    // new page (a tap on whatever lies under the finger there).
    let mut drop_touch = false;
    // Woken: the next frame presented ends the sleep (the backlight comes
    // on only then, over a fresh picture).
    let mut waking = false;
    // Full repaints still due (the link rebuild on waking cleared both
    // frame buffers).
    let mut full_repaints = 0u8;
    loop {
        ITERATIONS.fetch_add(1, Ordering::Relaxed);
        if let Some(sleep) = power::take_request().map(|r| r == power::Request::Sleep)
            && sleep != (power::asleep() && !waking)
        {
            switch_refresh(&mut display, sleep).await;
            if !sleep {
                waking = true;
                full_repaints = 2;
                window.request_redraw();
                other_buffer_stale = true;
            }
        }
        if power::asleep() && !waking {
            // Nothing is drawn; a touch only wakes (the rest of it is
            // dropped, it must not press anything).
            slint::platform::update_timers_and_animations();
            let ev = woken_by.take().or_else(|| input::EVENTS.try_receive().ok());
            if let Some(ev) = ev {
                if ev.phase == Phase::Down {
                    log::info!("power: woken by a touch at ({:.0}, {:.0})", ev.x, ev.y);
                    power::wake();
                    drop_touch = true;
                }
                continue;
            }
            if let Either::Second(ev) =
                select(Timer::after(MAX_SLEEP), input::EVENTS.receive()).await
            {
                woken_by = Some(ev);
            }
            continue;
        }
        enter(Stage::Touch);
        // Touch: route each event to the hook (e.g. list drags), or Slint.
        while let Some(ev) = woken_by.take().or_else(|| input::EVENTS.try_receive().ok()) {
            if ev.phase == Phase::Down {
                drop_touch = false;
            } else if drop_touch {
                drop_touch = ev.phase != Phase::Up;
                continue;
            }
            let route = hook.as_mut().map_or(Route::Slint, |h| h.pointer(&ev));
            log::trace!(
                "touch: {:?} ({:.0}, {:.0}) → {route:?}",
                ev.phase,
                ev.x,
                ev.y
            );
            match route {
                Route::Slint => {
                    dispatch(&window, &ev);
                    if ev.phase == Phase::Down && transition::pending() {
                        drop_touch = true;
                        // Whatever the old page's touch area holds ends.
                        window.dispatch_event(WindowEvent::PointerExited);
                    }
                }
                Route::Hook => {}
                // A pressed touch area gets `PointerEventKind.cancel`.
                Route::TakeOver => window.dispatch_event(WindowEvent::PointerExited),
            }
        }
        enter(Stage::Timers);
        slint::platform::update_timers_and_animations();
        // A page change requested during the touch handling above: shown
        // as a slide, which also renders the new page into both buffers.
        if let Some(direction) = transition::take() {
            enter(Stage::Render);
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
        enter(Stage::BackBuffer);
        let fb = display.back_buffer().await.as_mut_ptr();
        let target = Target::frame_buffer(Picture::frame_buffer(fb));

        enter(Stage::Render);
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
        if full_repaints > 0 {
            window.request_redraw();
        }
        window.draw_if_needed(|renderer| {
            slint_rendered = true;
            // Both buffers are blank after waking: repaint all of each once.
            renderer.set_repaint_buffer_type(if full_repaints > 0 {
                RepaintBufferType::NewBuffer
            } else {
                RepaintBufferType::SwappedBuffers
            });
            full_repaints = full_repaints.saturating_sub(1);
            renderer.set_rendering_rotation(RenderingRotation::NoRotation);
            rot.begin(target);
            let region = renderer.render_by_line(PpaLines { rot: &mut *rot });
            rot.finish();
            let (o, size) = (region.bounding_box_origin(), region.bounding_box_size());
            if size.width > 0 && size.height > 0 {
                slint_box = Some((o.x, o.y, size.width, size.height));
            }
            if t0.elapsed() > SLOW_FRAME {
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
            enter(Stage::Present);
            let render_time = t0.elapsed();
            // Always flip after a render, even an empty one (see above). The
            // CPU never wrote the frame buffer (PPA/DMA did): nothing to
            // write back from the cache, so only one row is synced (the
            // driver needs a non-empty area to switch buffers).
            display.present(0..1).expect("present");
            if waking {
                waking = false;
                power::set_asleep(false);
            }
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
            enter(Stage::Sleep);
            let wait = slint::platform::duration_until_next_timer_update().map_or(MAX_SLEEP, |d| {
                Duration::from_micros(d.as_micros() as u64).min(MAX_SLEEP)
            });
            if let Either::Second(ev) = select(Timer::after(wait), input::EVENTS.receive()).await {
                woken_by = Some(ev);
            }
        }
    }
}
