//! Rectangle copies between RGB565 pictures with the 2D-DMA.
//!
//! Wraps ESP-IDF's `esp_async_fbcpy` (a private `esp_lcd` API; the DPI
//! driver uses it for its own copies). Measured on the Tab5 with the display
//! streaming: 720×640 px PSRAM → PSRAM in 7.2 ms (64 Mpx/s), three times the
//! PPA's rate. No pixel processing, no cache maintenance: use it only for
//! memory the CPU does not write (layers filled by the PPA, frame buffers).

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use esp_idf_svc::sys::{self as sys, EspError, esp};

/// `esp_async_fbcpy_trans_desc_t` (esp_lcd/priv_include/esp_async_fbcpy.h).
#[repr(C)]
struct TransDesc {
    src_buffer: *const c_void,
    dst_buffer: *mut c_void,
    src_buffer_size_x: usize,
    src_buffer_size_y: usize,
    dst_buffer_size_x: usize,
    dst_buffer_size_y: usize,
    src_offset_x: usize,
    src_offset_y: usize,
    dst_offset_x: usize,
    dst_offset_y: usize,
    copy_size_x: usize,
    copy_size_y: usize,
    pixel_format_unique_id: u32,
}

type Handle = *mut c_void;
type DoneCb = unsafe extern "C" fn(Handle, *mut c_void, *mut c_void) -> bool;

unsafe extern "C" {
    fn esp_async_fbcpy_install(config: *const c_void, mcp: *mut Handle) -> sys::esp_err_t;
    fn esp_async_fbcpy(
        mcp: Handle,
        transaction: *mut TransDesc,
        done: Option<DoneCb>,
        cb_args: *mut c_void,
    ) -> sys::esp_err_t;
}

static DONE: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn on_done(_m: Handle, _e: *mut c_void, _a: *mut c_void) -> bool {
    DONE.fetch_add(1, Ordering::Release);
    false
}

/// An RGB565 picture in memory: `w × h` pixels, rows packed.
#[derive(Clone, Copy, Debug)]
pub struct Picture {
    pub buf: *mut u16,
    pub w: usize,
    pub h: usize,
}

/// The 2D-DMA copy engine. One copy at a time.
pub struct FbCopy {
    handle: Handle,
    submitted: u32,
}

impl FbCopy {
    pub fn new() -> Result<Self, EspError> {
        let mut handle: Handle = core::ptr::null_mut();
        // The config struct is empty in C; the driver only checks for null.
        let config = 0u32;
        unsafe {
            esp!(esp_async_fbcpy_install(
                (&raw const config).cast(),
                &mut handle
            ))?
        };
        Ok(Self {
            handle,
            submitted: DONE.load(Ordering::Acquire),
        })
    }

    /// Starts copying `size` (w, h) pixels from `src` at `from` to `dst` at
    /// `to`. Waits for the previous copy first.
    pub fn start(
        &mut self,
        src: Picture,
        from: (usize, usize),
        dst: Picture,
        to: (usize, usize),
        size: (usize, usize),
    ) {
        debug_assert!(from.0 + size.0 <= src.w && from.1 + size.1 <= src.h);
        debug_assert!(to.0 + size.0 <= dst.w && to.1 + size.1 <= dst.h);
        self.wait();
        let mut d = TransDesc {
            src_buffer: src.buf as *const c_void,
            dst_buffer: dst.buf.cast(),
            src_buffer_size_x: src.w,
            src_buffer_size_y: src.h,
            dst_buffer_size_x: dst.w,
            dst_buffer_size_y: dst.h,
            src_offset_x: from.0,
            src_offset_y: from.1,
            dst_offset_x: to.0,
            dst_offset_y: to.1,
            copy_size_x: size.0,
            copy_size_y: size.1,
            pixel_format_unique_id: sys::lcd_color_format_t_LCD_COLOR_FMT_RGB565,
        };
        unsafe {
            esp!(esp_async_fbcpy(
                self.handle,
                &mut d,
                Some(on_done),
                core::ptr::null_mut()
            ))
            .expect("2D-DMA copy");
        }
        self.submitted = self.submitted.wrapping_add(1);
    }

    /// Waits until the last copy has finished.
    pub fn wait(&self) {
        while (DONE.load(Ordering::Acquire).wrapping_sub(self.submitted) as i32) < 0 {
            core::hint::spin_loop();
        }
    }
}
