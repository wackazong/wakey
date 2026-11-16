//! Rectangle copies between RGB565 pictures with the 2D-DMA.
//!
//! Wraps ESP-IDF's `esp_async_fbcpy` (a private `esp_lcd` API; the DPI
//! driver uses it for its own copies). Measured on the Tab5 with the display
//! streaming: 720×640 px PSRAM → PSRAM in 7.2 ms (64 Mpx/s), three times the
//! PPA's rate. No pixel processing, no cache maintenance: use it only for
//! memory the CPU does not write (layers filled by the PPA, frame buffers).

use super::{HEIGHT, WIDTH, cache};
use crate::psram::PsramBuffer;
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

/// Counts the finished copies of one [`FbCopy`] (`cb_args`). Each instance
/// has its own counter: with a shared one, an instance would take another's
/// finished copy for its own.
unsafe extern "C" fn on_done(_m: Handle, _e: *mut c_void, done: *mut c_void) -> bool {
    // SAFETY: `done` is the `AtomicU32` boxed in the `FbCopy`, which lives
    // as long as its copies (it waits for them before dropping).
    unsafe { &*done.cast::<AtomicU32>() }.fetch_add(1, Ordering::Release);
    false
}

/// An RGB565 picture in memory: `w × h` pixels, rows packed.
#[derive(Clone, Copy, Debug)]
pub struct Picture {
    pub buf: *mut u16,
    pub w: usize,
    pub h: usize,
}

impl Picture {
    /// A panel frame buffer (portrait, `WIDTH × HEIGHT`).
    pub fn frame_buffer(buf: *mut u16) -> Self {
        Self {
            buf,
            w: WIDTH,
            h: HEIGHT,
        }
    }

    /// Fills the picture with `colour` by the CPU, written back for the DMA
    /// engines.
    ///
    /// # Safety
    ///
    /// `buf` must hold `w × h` pixels in PSRAM that no DMA engine uses
    /// meanwhile.
    pub unsafe fn fill(self, colour: u16) {
        let n = self.w * self.h;
        // SAFETY: the caller's pixels, not shared with a DMA engine now.
        unsafe {
            core::slice::from_raw_parts_mut(self.buf, n).fill(colour);
            cache::writeback(self.buf, n);
        }
    }
}

/// A picture in memory of its own, freed on drop: PSRAM the DMA engines
/// can use, cache-line aligned and sized in whole lines (the PPA's output
/// must be).
pub struct OwnedPicture {
    picture: Picture,
    _memory: PsramBuffer,
}

impl OwnedPicture {
    /// `w × h` pixels; `None` if there is no room.
    pub fn new(w: usize, h: usize) -> Option<Self> {
        let memory = PsramBuffer::new(w * h * 2, sys::MALLOC_CAP_DMA)?;
        let picture = Picture {
            buf: memory.as_mut_ptr().cast(),
            w,
            h,
        };
        Some(Self {
            picture,
            _memory: memory,
        })
    }

    pub fn picture(&self) -> Picture {
        self.picture
    }

    /// The same memory as a `w × h` picture, if it fits.
    pub fn reshaped(&self, w: usize, h: usize) -> Option<Picture> {
        (w * h <= self.picture.w * self.picture.h).then_some(Picture {
            buf: self.picture.buf,
            w,
            h,
        })
    }
}

/// One copy: `size` (w, h) pixels from `src` at `from` to `dst` at `to`.
#[derive(Clone, Copy, Debug)]
pub struct Blit {
    pub src: Picture,
    pub from: (usize, usize),
    pub dst: Picture,
    pub to: (usize, usize),
    pub size: (usize, usize),
}

/// The 2D-DMA copy engine. One copy at a time.
pub struct FbCopy {
    handle: Handle,
    /// Copies started, and finished (counted by the ISR; boxed, so its
    /// address stays fixed).
    submitted: u32,
    done: Box<AtomicU32>,
}

impl FbCopy {
    pub fn new() -> Result<Self, EspError> {
        let mut handle: Handle = core::ptr::null_mut();
        // The config struct is empty in C; the driver only checks for null.
        let config = 0u32;
        // SAFETY: both pointers are valid for the call; the driver keeps
        // neither.
        unsafe {
            esp!(esp_async_fbcpy_install(
                (&raw const config).cast(),
                &mut handle
            ))?
        };
        Ok(Self {
            handle,
            submitted: 0,
            done: Box::new(AtomicU32::new(0)),
        })
    }

    /// Starts a copy. Waits for the previous one first.
    pub fn start(&mut self, blit: Blit) {
        let Blit {
            src,
            from,
            dst,
            to,
            size,
        } = blit;
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
        // SAFETY: the descriptor is read during the call. Both pictures are
        // DMA-capable memory holding the rectangles (asserted above, by
        // their owners' contract). `done` is boxed and outlives the copy:
        // `drop` waits for it.
        unsafe {
            esp!(esp_async_fbcpy(
                self.handle,
                &mut d,
                Some(on_done),
                (&raw const *self.done).cast_mut().cast()
            ))
            .expect("2D-DMA copy");
        }
        self.submitted = self.submitted.wrapping_add(1);
    }

    /// True once the last copy has finished.
    pub fn is_done(&self) -> bool {
        (self
            .done
            .load(Ordering::Acquire)
            .wrapping_sub(self.submitted) as i32)
            >= 0
    }

    /// Waits until the last copy has finished.
    pub fn wait(&self) {
        while !self.is_done() {
            core::hint::spin_loop();
        }
    }
}

impl Drop for FbCopy {
    /// The ISR writes `done` until the last copy has finished.
    fn drop(&mut self) {
        self.wait();
    }
}
