//! Timing of the list layer for performance work, logged at debug level:
//! the render time per row (every 32 rows), and each scroll's frames when
//! it ends.

use embassy_time::{Duration, Instant};

/// Rows per log line of the row render time.
const ROWS_PER_LOG: u32 = 32;
/// A gap longer than this missed a panel refresh (15.2 ms at 66 Hz).
const LATE: Duration = Duration::from_millis(20);

#[derive(Default)]
pub(super) struct Timing {
    /// Rows rendered since the last log, and the time they took.
    rows: (u32, Duration),
    /// The last frame's time from `start` to the end of `finish`.
    frame_busy: Duration,
    scroll: ScrollTiming,
}

/// One scroll: start-to-start gaps of the layer's frames, and the layer's
/// own time in the frame before the longest one.
#[derive(Default)]
struct ScrollTiming {
    since: Option<Instant>,
    last_start: Option<Instant>,
    frames: u32,
    late: u32,
    /// Longest gap and the busy time of the frame before it.
    worst: (Duration, Duration),
    rows: u32,
    rows_time: Duration,
}

impl Timing {
    pub(super) fn row_rendered(&mut self, took: Duration, scrolling: bool) {
        if scrolling {
            self.scroll.rows += 1;
            self.scroll.rows_time += took;
        }
        self.rows.0 += 1;
        self.rows.1 += took;
        if self.rows.0 == ROWS_PER_LOG {
            log::debug!(
                "list layer: {ROWS_PER_LOG} rows rendered, {} µs each",
                self.rows.1.as_micros() / u64::from(ROWS_PER_LOG)
            );
            self.rows = Default::default();
        }
    }

    /// A frame of a scroll starts at `now`.
    pub(super) fn scroll_frame(&mut self, now: Instant) {
        let s = &mut self.scroll;
        s.since.get_or_insert(now);
        if let Some(last) = s.last_start {
            let gap = now - last;
            if gap > LATE {
                s.late += 1;
            }
            if gap > s.worst.0 {
                s.worst = (gap, self.frame_busy);
            }
        }
        s.last_start = Some(now);
        s.frames += 1;
    }

    /// A frame of the layer took `busy` from `start` to the end of `finish`.
    pub(super) fn frame_ended(&mut self, busy: Duration) {
        self.frame_busy = busy;
    }

    pub(super) fn scroll_ended(&mut self) {
        let s = core::mem::take(&mut self.scroll);
        if let Some(since) = s.since {
            log::debug!(
                "list layer: scroll {} frames in {} ms, {} late (worst {} ms, busy {} ms before), {} rows rendered ({} ms)",
                s.frames,
                since.elapsed().as_millis(),
                s.late,
                s.worst.0.as_millis(),
                s.worst.1.as_millis(),
                s.rows,
                s.rows_time.as_millis()
            );
        }
    }
}
