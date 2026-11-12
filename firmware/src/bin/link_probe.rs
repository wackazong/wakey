//! Spike: does rebuilding the display link (`Display::set_pixel_clock`)
//! crash while the camera light meter runs? In the app it did, at the
//! first display sleep: store access fault in `dpi_panel_init`
//! (`dw_gdma_ll_channel_set_link_list_master_port`, MTVAL 0x50081128, a
//! DW-GDMA channel register), as if the DMA controller's clock were off.
//! The camera's CSI receiver uses the same DW-GDMA. Without the camera
//! (`power_probe`, `chip_sleep`) the rebuild worked.
//!
//! Rebuilds the link 10 times, alternating 33 and 66 Hz, with the light
//! meter running, and logs each; no touching needed.
//!
//! `cargo run --release --bin link_probe`

use esp_idf_svc::sys;
use std::time::Duration;
use wakey::board::Board;

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let Board {
        backlight,
        mut display,
        i2c,
        ..
    } = Board::init().expect("board");
    wakey::ambient::start(i2c.clone(), backlight, 35);
    std::thread::sleep(Duration::from_secs(5));
    for i in 0..30 {
        let mhz = if i % 2 == 0 { Some(40.0) } else { None };
        log::info!("probe: rebuild {i} at {mhz:?} MHz");
        let r = display.set_pixel_clock(mhz);
        let (free, largest, internal) = unsafe {
            (
                sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM),
                sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_SPIRAM),
                sys::heap_caps_get_free_size(sys::MALLOC_CAP_INTERNAL),
            )
        };
        log::info!(
            "probe: rebuild {i}: {r:?}; PSRAM free {free}, largest {largest}; internal free {internal}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    log::info!("probe: done");
    loop {
        std::thread::sleep(Duration::from_secs(10));
    }
}
