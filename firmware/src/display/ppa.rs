//! Hardware rotation with the PPA (Pixel Processing Accelerator).
//!
//! UI toolkits render unrotated landscape lines into small strips in
//! internal RAM. Each finished strip is queued to the PPA's
//! scale-rotate-mirror engine, which rotates it by 90° and writes it into the
//! portrait frame buffer by DMA, while the CPU renders the next strip.
//!
//! This replaces two CPU costs: software rotation (cache-unfriendly) and
//! copying rendered lines into PSRAM.
//!
//! The output is a [`Target`]: the frame buffer, or any other portrait
//! picture with the same orientation (e.g. a slot in the scrolling list's
//! layer, see `ui::list_layer`).
//!
//! Rules that keep the caches coherent:
//! - The CPU must not write a target picture while the PPA is in use. The
//!   driver invalidates the output rows before each transaction, which would
//!   discard CPU writes still in the cache.
//! - [`PpaRotator::finish`] must be called before the frame is presented. It
//!   waits for the queue to drain.

use super::{HEIGHT, PIXELS, WIDTH, cache};
use core::ffi::c_void;
use core::ops::Range;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use esp_idf_svc::sys::{self as sys, EspError, esp};

/// Landscape width (= panel height): maximum strip line length.
const LINE_PX: usize = HEIGHT;
/// Lines per strip. Taller strips mean wider writes per panel row in PSRAM
/// (2 bytes per line) and fewer transactions, at the cost of internal RAM.
pub const STRIP_LINES: usize = 16;
/// Strip buffers. Some are open (being filled), the rest are queued in the PPA.
const NUM_BUFS: usize = 4;
/// At most this many strips are filled at the same time. Slint hands out
/// several ranges per line when the dirty region has rectangles side by side.
const MAX_OPEN: usize = 3;

/// Widest block per PPA transaction. A 1280-wide, 5-line block hung the PPA
/// for good on this P4 rev v1.3 (no completion event); blocks up to 640
/// wide have always completed. Wider strips are split.
const MAX_BLOCK_W: usize = 640;

/// Shortest strip handed to the PPA. Rotated by 90°, a strip's line count
/// becomes the output block width; blocks narrower than 8 pixels stall the
/// PPA for good while the panel DMA reads PSRAM (ESP-IDF's DIG-734
/// workaround computes the output width from the unrotated block; see
/// `docs/pitfalls.md`). Shorter strips are padded with lines read back from
/// the frame buffer.
const MIN_LINES: usize = 8;

/// Microseconds the CPU spent waiting for the PPA (diagnostics).
pub static WAIT_US: AtomicU32 = AtomicU32::new(0);

fn now_us() -> u32 {
    unsafe { sys::esp_timer_get_time() as u32 }
}

/// Transactions finished by the PPA since boot (incremented in its ISR).
static DONE: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn on_trans_done(
    _client: sys::ppa_client_handle_t,
    _event: *mut sys::ppa_event_data_t,
    _user_data: *mut c_void,
) -> bool {
    DONE.fetch_add(1, Ordering::Release);
    false
}

/// Where rotated landscape lines go: a portrait RGB565 picture and the
/// mapping of landscape coordinates into it. Landscape line `l` (y) becomes
/// picture column `col_off + l`; landscape pixel `x` becomes picture row
/// `row_ref - x`.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub buf: *mut u16,
    /// Picture width (row stride) and height, pixels.
    pub pic_w: usize,
    pub pic_h: usize,
    pub col_off: usize,
    pub row_ref: usize,
    /// Landscape lines that belong to this target (for padding short strips
    /// with neighbouring lines).
    pub lines: usize,
}

impl Target {
    /// The panel frame buffer: landscape (x, y) → column y, row HEIGHT-1-x.
    pub fn frame_buffer(fb: &mut [u16]) -> Self {
        assert_eq!(fb.len(), PIXELS);
        Self {
            buf: fb.as_mut_ptr(),
            pic_w: WIDTH,
            pic_h: HEIGHT,
            col_off: 0,
            row_ref: HEIGHT - 1,
            lines: WIDTH,
        }
    }
}

#[derive(Clone, Debug)]
struct Open {
    buf: usize,
    range: Range<usize>,
    first_line: usize,
    lines: usize,
}

pub struct PpaRotator {
    client: sys::ppa_client_handle_t,
    bufs: [*mut u16; NUM_BUFS],
    /// Sequence number of the last transaction that read each buffer.
    buf_seq: [u32; NUM_BUFS],
    /// Transactions submitted since boot.
    submitted: u32,
    open: heapless::Vec<Open, MAX_OPEN>,
    /// Geometry of recent submissions (diagnostics for stalls).
    recent: [(u32, Open); 8],
    /// Picture receiving the current render.
    target: Target,
}

impl PpaRotator {
    pub fn new() -> Result<Self, EspError> {
        let mut client: sys::ppa_client_handle_t = ptr::null_mut();
        unsafe {
            let mut cfg: sys::ppa_client_config_t = core::mem::zeroed();
            cfg.oper_type = sys::ppa_operation_t_PPA_OPERATION_SRM;
            // Headroom: a finished transaction's slot may be recycled slightly
            // after its done-callback ran.
            cfg.max_pending_trans_num = (NUM_BUFS * LINE_PX.div_ceil(MAX_BLOCK_W)) as u32 + 2;
            esp!(sys::ppa_register_client(&cfg, &mut client))?;
            let cbs = sys::ppa_event_callbacks_t {
                on_trans_done: Some(on_trans_done),
            };
            esp!(sys::ppa_client_register_event_callbacks(client, &cbs))?;
        }
        let mut bufs = [ptr::null_mut(); NUM_BUFS];
        for b in &mut bufs {
            // Internal, DMA-capable, cache-line aligned.
            let p = unsafe {
                sys::heap_caps_aligned_alloc(
                    128,
                    STRIP_LINES * LINE_PX * 2,
                    sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_DMA | sys::MALLOC_CAP_8BIT,
                )
            };
            assert!(!p.is_null(), "no internal RAM for PPA strip buffers");
            *b = p.cast();
        }
        log::info!(
            "ppa: {NUM_BUFS} strips of {STRIP_LINES}×{LINE_PX} px in internal RAM ({} KiB)",
            NUM_BUFS * STRIP_LINES * LINE_PX * 2 / 1024
        );
        Ok(Self {
            client,
            bufs,
            buf_seq: [0; NUM_BUFS],
            submitted: 0,
            open: heapless::Vec::new(),
            recent: core::array::from_fn(|_| {
                (
                    0,
                    Open {
                        buf: 0,
                        range: 0..0,
                        first_line: 0,
                        lines: 0,
                    },
                )
            }),
            target: Target {
                buf: ptr::null_mut(),
                pic_w: 0,
                pic_h: 0,
                col_off: 0,
                row_ref: 0,
                lines: 0,
            },
        })
    }

    /// Starts a render into `target`. Call [`Self::finish`] when done.
    pub fn begin(&mut self, target: Target) {
        debug_assert!(self.open.is_empty());
        self.target = target;
    }

    fn done() -> u32 {
        DONE.load(Ordering::Acquire)
    }

    fn report_stall(&self, spins: &mut u32) {
        *spins += 1;
        if *spins == 20_000_000 {
            log::error!(
                "ppa: stalled, submitted {} done {}; recent: {:?}",
                self.submitted,
                Self::done(),
                self.recent
                    .iter()
                    .map(|(n, o)| (*n, o.range.clone(), o.first_line, o.lines))
                    .collect::<heapless::Vec<_, 8>>()
            );
        }
    }

    /// A buffer that is neither open nor still read by the PPA.
    fn free_buffer(&mut self) -> usize {
        let mut spins = 0;
        loop {
            self.report_stall(&mut spins);
            for i in 0..NUM_BUFS {
                let in_use = self.open.iter().any(|o| o.buf == i);
                let busy = (Self::done().wrapping_sub(self.buf_seq[i]) as i32) < 0;
                if !in_use && !busy {
                    return i;
                }
            }
            core::hint::spin_loop();
        }
    }

    /// Returns the strip slice for landscape `line`, pixels `range`.
    pub fn line(&mut self, line: usize, range: Range<usize>) -> &mut [u16] {
        let pos = self.open.iter().position(|o| o.range == range);
        let idx = match pos {
            Some(i)
                if self.open[i].first_line + self.open[i].lines == line
                    && self.open[i].lines < STRIP_LINES =>
            {
                i
            }
            other => {
                if let Some(i) = other {
                    self.flush(i);
                } else if self.open.is_full() {
                    self.flush(0);
                }
                let buf = self.free_buffer();
                let _ = self.open.push(Open {
                    buf,
                    range: range.clone(),
                    first_line: line,
                    lines: 0,
                });
                self.open.len() - 1
            }
        };
        let o = &mut self.open[idx];
        let w = o.range.len();
        let start = o.lines * w;
        o.lines += 1;
        // SAFETY: the buffer holds STRIP_LINES × LINE_PX pixels, the PPA does
        // not read it until it is flushed.
        unsafe { core::slice::from_raw_parts_mut(self.bufs[o.buf].add(start), w) }
    }

    /// Queues open strip `i` to the PPA and closes it.
    fn flush(&mut self, i: usize) {
        let mut o = self.open.remove(i);
        if o.lines == 0 {
            return;
        }
        if o.lines < MIN_LINES {
            self.pad(&mut o);
        }
        let w = o.range.len();
        let mut x = 0;
        while x < w {
            let bw = (w - x).min(MAX_BLOCK_W);
            self.submit(&o, x, bw);
            x += bw;
        }
        self.buf_seq[o.buf] = self.submitted;
        self.recent[self.submitted as usize % 8] = (self.submitted, o);
    }

    /// Grows strip `o` to `MIN_LINES` lines. The extra lines are not dirty in
    /// this frame, so their current frame buffer content is correct; it is
    /// read back (after the PPA has finished everything queued, and with the
    /// cache dropped for those rows) and written again unchanged.
    fn pad(&mut self, o: &mut Open) {
        let mut spins = 0;
        while Self::done() != self.submitted {
            self.report_stall(&mut spins);
            core::hint::spin_loop();
        }
        let t = self.target;
        let w = o.range.len();
        let start = o.first_line.min(t.lines - MIN_LINES);
        let shift = o.first_line - start;
        let buf = self.bufs[o.buf];
        unsafe {
            // Picture rows row_ref+1-end..row_ref+1-start hold landscape
            // columns range.start..range.end. Drop any cached copy so the
            // reads see what the PPA wrote.
            let row0 = t.row_ref + 1 - o.range.end;
            cache::invalidate(t.buf.add(row0 * t.pic_w), w * t.pic_w);
            // Move the rendered lines to their place in the padded strip.
            if shift > 0 {
                core::ptr::copy(buf, buf.add(shift * w), o.lines * w);
            }
            let rendered = shift..shift + o.lines;
            for l in 0..MIN_LINES {
                if rendered.contains(&l) {
                    continue;
                }
                let line = start + l;
                let dst = buf.add(l * w);
                for i in 0..w {
                    let x = o.range.start + i;
                    *dst.add(i) = *t.buf.add((t.row_ref - x) * t.pic_w + t.col_off + line);
                }
            }
        }
        o.first_line = start;
        o.lines = MIN_LINES;
    }

    /// One PPA transaction: columns `x..x+bw` of strip `o`.
    ///
    /// Waits for the previous transaction first: with several queued, the
    /// driver starts the next one from the completion ISR, and under load
    /// that path stalled the PPA for good on this chip (every geometry that
    /// stalled completes when submitted alone). One in flight still lets
    /// the CPU render the next strip while the PPA rotates this one.
    fn submit(&mut self, o: &Open, x: usize, bw: usize) {
        let mut spins = 0;
        let t0 = now_us();
        while Self::done() != self.submitted {
            self.report_stall(&mut spins);
            core::hint::spin_loop();
        }
        WAIT_US.fetch_add(now_us().wrapping_sub(t0), Ordering::Relaxed);
        let w = o.range.len() as u32;
        let h = o.lines as u32;
        unsafe {
            let mut cfg: sys::ppa_srm_oper_config_t = core::mem::zeroed();
            cfg.in_.buffer = self.bufs[o.buf] as *const c_void;
            cfg.in_.pic_w = w;
            cfg.in_.pic_h = h;
            cfg.in_.block_w = bw as u32;
            cfg.in_.block_h = h;
            cfg.in_.block_offset_x = x as u32;
            cfg.in_.__bindgen_anon_1.srm_cm = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565;
            let t = self.target;
            cfg.out.buffer = t.buf.cast();
            cfg.out.buffer_size = (t.pic_w * t.pic_h * 2) as u32;
            cfg.out.pic_w = t.pic_w as u32;
            cfg.out.pic_h = t.pic_h as u32;
            // Landscape (x, y) → picture column col_off+y, row row_ref-x. The
            // block's landscape rows first_line.. become columns
            // col_off+first_line.., its landscape columns a..b rows
            // row_ref+1-b..row_ref+1-a.
            let a = o.range.start + x;
            cfg.out.block_offset_x = (t.col_off + o.first_line) as u32;
            cfg.out.block_offset_y = (t.row_ref + 1 - (a + bw)) as u32;
            cfg.out.__bindgen_anon_1.srm_cm = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565;
            // 90° counter-clockwise.
            cfg.rotation_angle = sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_90;
            cfg.scale_x = 1.0;
            cfg.scale_y = 1.0;
            cfg.alpha_update_mode = sys::ppa_alpha_update_mode_t_PPA_ALPHA_NO_CHANGE;
            cfg.mode = sys::ppa_trans_mode_t_PPA_TRANS_MODE_NON_BLOCKING;
            esp!(sys::ppa_do_scale_rotate_mirror(self.client, &cfg)).expect("PPA rotate");
        }
        self.submitted = self.submitted.wrapping_add(1);
    }

    /// Queues the remaining strips and waits until the PPA has written
    /// everything into the frame buffer.
    pub fn finish(&mut self) {
        while !self.open.is_empty() {
            self.flush(0);
        }
        let mut spins = 0;
        let t0 = now_us();
        while (Self::done().wrapping_sub(self.submitted) as i32) < 0 {
            self.report_stall(&mut spins);
            core::hint::spin_loop();
        }
        WAIT_US.fetch_add(now_us().wrapping_sub(t0), Ordering::Relaxed);
    }
}
