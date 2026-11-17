//! Buffers in PSRAM for DMA, freed when dropped.

use esp_idf_svc::sys;

/// The L2 cache line (`CONFIG_CACHE_L2_CACHE_LINE_128B`). PSRAM DMA works
/// on whole cache lines: buffers start on one and are sized in whole ones.
pub const CACHE_LINE: usize = 128;

/// A cache-line aligned buffer in PSRAM (`heap_caps_aligned_alloc`), freed
/// on drop. Its memory is shared with DMA engines and other threads, so it
/// hands out a pointer, not a slice: the owner decides who touches what.
pub struct PsramBuffer {
    ptr: *mut u8,
}

// SAFETY: the buffer owns its allocation; nothing ties it to a thread.
unsafe impl Send for PsramBuffer {}

impl PsramBuffer {
    /// `len` bytes (rounded up to whole cache lines) with `caps` added to
    /// `MALLOC_CAP_SPIRAM`; `None` if there is no room.
    pub fn new(len: usize, caps: u32) -> Option<Self> {
        let len = len.next_multiple_of(CACHE_LINE);
        // SAFETY: a plain allocation; the result is checked below.
        let ptr =
            unsafe { sys::heap_caps_aligned_alloc(CACHE_LINE, len, sys::MALLOC_CAP_SPIRAM | caps) }
                .cast::<u8>();
        (!ptr.is_null()).then_some(Self { ptr })
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr
    }
}

impl Drop for PsramBuffer {
    fn drop(&mut self) {
        // SAFETY: allocated by `heap_caps_aligned_alloc`, freed once.
        unsafe { sys::heap_caps_free(self.ptr.cast()) };
    }
}
