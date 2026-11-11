//! Per-frame trace for performance work (feature `ui-bench`): render time,
//! time since the previous frame, dirty bounding box, CPU cycles and
//! retired instructions. Cycles per instruction show whether a frame is
//! slow from work (≈ 1.4 on this core) or from memory stalls (≈ 5).
//!
//! Off until [`ENABLED`] is set (the scripted bench in `app::bench` does).

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use embassy_time::Instant;

/// Log every frame.
pub static ENABLED: AtomicBool = AtomicBool::new(false);

/// When the previous traced frame was presented (µs since boot).
static LAST_PRESENT_US: AtomicU32 = AtomicU32::new(0);

/// A frame being measured: from before rendering to after presenting.
pub struct Frame {
    start: Instant,
    cycles: u32,
    instructions: u32,
}

/// The core's cycle and retired-instruction counters (machine mode, where
/// ESP-IDF tasks run).
fn counters() -> (u32, u32) {
    let (cycles, instructions): (u32, u32);
    // SAFETY: reading the standard RISC-V counters has no side effects.
    unsafe {
        core::arch::asm!("csrr {0}, mcycle", out(reg) cycles);
        core::arch::asm!("csrr {0}, minstret", out(reg) instructions);
    }
    (cycles, instructions)
}

impl Frame {
    pub fn start() -> Self {
        let (cycles, instructions) = counters();
        Self {
            start: Instant::now(),
            cycles,
            instructions,
        }
    }

    /// Logs the frame (if enabled); `dirty` is the rendered bounding box
    /// (x, y, width, height).
    pub fn presented(self, dirty: (i32, i32, u32, u32)) {
        let now = Instant::now();
        let now_us = now.as_micros() as u32;
        let last_us = LAST_PRESENT_US.swap(now_us, Relaxed);
        if !ENABLED.load(Relaxed) {
            return;
        }
        let (cycles, instructions) = counters();
        log::info!(
            "frame {:.1} ms, gap {:.1} ms, dirty {dirty:?}, {:.1} Mcycles, {:.1} Minstr",
            (now - self.start).as_micros() as f32 / 1000.0,
            now_us.wrapping_sub(last_us) as f32 / 1000.0,
            cycles.wrapping_sub(self.cycles) as f32 / 1e6,
            instructions.wrapping_sub(self.instructions) as f32 / 1e6,
        );
    }
}
