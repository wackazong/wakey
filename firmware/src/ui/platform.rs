//! Slint on the Tab5: the `Platform`, its windows, and rendering a window
//! through the PPA into any portrait picture.
//!
//! Slint renders unrotated landscape lines into small strips in internal
//! RAM ([`PpaLines`]); the PPA rotates each strip into the target picture
//! (`display::ppa`). Components get their window in the order they are
//! created: first the full-screen main window ([`init`]), then any
//! offscreen windows ([`offscreen_window`]) rendered on demand (list rows).

// `PENDING` below already uses a `const` initializer; clippy's lint misfires
// on the ESP-IDF target.
#![allow(clippy::missing_const_for_thread_local)]

use super::{UI_HEIGHT, UI_WIDTH};
use crate::display::ppa::{PpaRotator, Target};
use embassy_time::Instant;
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
/// the next component created will use. Call once, first. The window
/// repaints only what changed in the last two frames
/// (`RepaintBufferType::SwappedBuffers`), matching the two frame buffers.
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

/// Slint line output → PPA strips.
pub(super) struct PpaLines<'a> {
    pub(super) rot: &'a mut PpaRotator,
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
