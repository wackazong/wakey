//! CPU cache maintenance for memory that DMA engines (panel, PPA, 2D-DMA)
//! read or write. They access PSRAM directly, not through the cache.

use core::ffi::c_void;
use esp_idf_svc::sys;

/// L2 cache line size (`CONFIG_CACHE_L2_CACHE_LINE_128B`).
const LINE: usize = 128;

// `esp_cache.h` is not in esp-idf-sys's default bindings.
const ESP_CACHE_MSYNC_FLAG_UNALIGNED: i32 = 1 << 1;
const ESP_CACHE_MSYNC_FLAG_DIR_C2M: i32 = 1 << 2;
const ESP_CACHE_MSYNC_FLAG_DIR_M2C: i32 = 1 << 3;
unsafe extern "C" {
    fn esp_cache_msync(addr: *mut c_void, size: usize, flags: i32) -> sys::esp_err_t;
}

/// Writes CPU-written pixels back to PSRAM, so DMA engines see them.
pub fn writeback(buf: *mut u16, pixels: usize) {
    unsafe {
        esp_cache_msync(
            buf.cast(),
            pixels * 2,
            ESP_CACHE_MSYNC_FLAG_DIR_C2M | ESP_CACHE_MSYNC_FLAG_UNALIGNED,
        );
    }
}

/// Drops cached copies of `pixels` pixels at `buf` (widened to whole cache
/// lines), so CPU reads see what a DMA engine wrote. Cached CPU writes in
/// those lines are lost.
pub fn invalidate(buf: *mut u16, pixels: usize) {
    let first = buf as usize & !(LINE - 1);
    let last = buf as usize + pixels * 2;
    unsafe {
        esp_cache_msync(
            first as *mut c_void,
            (last - first).next_multiple_of(LINE),
            ESP_CACHE_MSYNC_FLAG_DIR_M2C,
        );
    }
}
