//! Frame statistics, logged once per second.

use crate::display::Display;
use core::sync::atomic::{AtomicU32, Ordering};
use embassy_time::{Duration, Instant};

pub struct FrameStats {
    name: &'static str,
    window_start: Instant,
    scanned_at_start: u32,
    frames: u32,
    render_total: Duration,
    render_max: Duration,
    rows_total: u64,
}

impl FrameStats {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            window_start: Instant::now(),
            scanned_at_start: Display::frames_scanned(),
            frames: 0,
            render_total: Duration::from_ticks(0),
            render_max: Duration::from_ticks(0),
            rows_total: 0,
        }
    }

    /// Records one presented frame: CPU time spent rendering it and the
    /// number of panel rows it touched.
    pub fn frame(&mut self, render: Duration, rows: usize) {
        self.frames += 1;
        self.render_total += render;
        self.render_max = self.render_max.max(render);
        self.rows_total += rows as u64;
        self.maybe_log();
    }

    /// Call when idle too, so the log keeps coming while nothing is drawn.
    pub fn maybe_log(&mut self) {
        let elapsed = self.window_start.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let secs = elapsed.as_micros() as f32 / 1e6;
        let scanned = Display::frames_scanned().wrapping_sub(self.scanned_at_start);
        let n = self.frames.max(1);
        let ppa_wait_ms =
            crate::display::ppa::WAIT_US.swap(0, Ordering::Relaxed) as f32 / 1000.0 / n as f32;
        log::info!(
            "{}: {:.1} fps (panel {:.1} Hz), render avg {:.2} ms max {:.2} ms (PPA wait {ppa_wait_ms:.2}), {} rows/frame, heap free {} KiB (internal {} KiB)",
            self.name,
            self.frames as f32 / secs,
            scanned as f32 / secs,
            self.render_total.as_micros() as f32 / n as f32 / 1000.0,
            self.render_max.as_micros() as f32 / 1000.0,
            self.rows_total / u64::from(n),
            unsafe { esp_idf_svc::sys::esp_get_free_heap_size() } / 1024,
            unsafe {
                esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_INTERNAL)
            } / 1024,
        );
        LAST_FPS_X10.store((self.frames as f32 / secs * 10.0) as u32, Ordering::Relaxed);
        LAST_RENDER_US.store(
            (self.render_total.as_micros() / u64::from(n)) as u32,
            Ordering::Relaxed,
        );
        *self = Self::new(self.name);
    }
}

static LAST_FPS_X10: AtomicU32 = AtomicU32::new(0);
static LAST_RENDER_US: AtomicU32 = AtomicU32::new(0);

/// Frames per second and average render time (ms) of the last logged window,
/// for showing on screen.
pub fn last() -> (f32, f32) {
    (
        LAST_FPS_X10.load(Ordering::Relaxed) as f32 / 10.0,
        LAST_RENDER_US.load(Ordering::Relaxed) as f32 / 1000.0,
    )
}
