//! PPA probe for the Tab5 (P4 rev v1.3), with the display streaming as in
//! the real app.
//!
//! 1. Throughput: rotate a 640×720 RGB565 window (PSRAM → frame buffer), the
//!    job of the scrolling-list compositor, and time it.
//! 2. Stalls: rotate strips of every height 1..=16 and report the first that
//!    never completes (strips under 8 lines stall on this chip).
//!
//! `cargo run --release --bin ppa_probe`

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sys::{self as sys, esp};

static DONE: AtomicU32 = AtomicU32::new(0);
const R0: sys::ppa_srm_rotation_angle_t = sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_0;
const R90: sys::ppa_srm_rotation_angle_t = sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_90;

unsafe extern "C" fn done(
    _c: sys::ppa_client_handle_t,
    _e: *mut sys::ppa_event_data_t,
    _u: *mut c_void,
) -> bool {
    DONE.fetch_add(1, Ordering::Release);
    false
}

// ESP-IDF's private 2D-DMA frame buffer copy (esp_lcd/priv_include/esp_async_fbcpy.h).
#[repr(C)]
struct FbcpyDesc {
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
type FbcpyHandle = *mut c_void;
unsafe extern "C" {
    fn esp_async_fbcpy_install(config: *const c_void, mcp: *mut FbcpyHandle) -> sys::esp_err_t;
    fn esp_async_fbcpy(
        mcp: FbcpyHandle,
        transaction: *mut FbcpyDesc,
        done: Option<unsafe extern "C" fn(FbcpyHandle, *mut c_void, *mut c_void) -> bool>,
        cb_args: *mut c_void,
    ) -> sys::esp_err_t;
}
static FB_DONE: AtomicU32 = AtomicU32::new(0);
unsafe extern "C" fn fb_done(_m: FbcpyHandle, _e: *mut c_void, _a: *mut c_void) -> bool {
    FB_DONE.fetch_add(1, Ordering::Release);
    false
}

fn main() -> ! {
    wakey::runtime::start(|spawner| spawner.spawn(app().unwrap()))
}

/// One rotate-90 (counter-clockwise) transaction; waits up to `timeout_ms`.
/// Returns the duration in µs, or None if it never completed.
#[allow(clippy::too_many_arguments)]
unsafe fn rotate(
    client: sys::ppa_client_handle_t,
    angle: sys::ppa_srm_rotation_angle_t,
    inb: *const c_void,
    pic_w: u32,
    pic_h: u32,
    block: (u32, u32, u32, u32),
    out: *mut c_void,
    out_xy: (u32, u32),
    timeout_ms: u32,
) -> Option<u32> {
    let (bx, by, bw, bh) = block;
    unsafe {
        let mut c: sys::ppa_srm_oper_config_t = core::mem::zeroed();
        c.in_.buffer = inb;
        c.in_.pic_w = pic_w;
        c.in_.pic_h = pic_h;
        c.in_.block_offset_x = bx;
        c.in_.block_offset_y = by;
        c.in_.block_w = bw;
        c.in_.block_h = bh;
        c.in_.__bindgen_anon_1.srm_cm = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565;
        c.out.buffer = out;
        c.out.buffer_size = 720 * 1280 * 2;
        c.out.pic_w = 720;
        c.out.pic_h = 1280;
        c.out.block_offset_x = out_xy.0;
        c.out.block_offset_y = out_xy.1;
        c.out.__bindgen_anon_1.srm_cm = sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565;
        c.rotation_angle = angle;
        c.scale_x = 1.0;
        c.scale_y = 1.0;
        c.mode = sys::ppa_trans_mode_t_PPA_TRANS_MODE_NON_BLOCKING;
        let before = DONE.load(Ordering::Acquire);
        let t0 = sys::esp_timer_get_time();
        esp!(sys::ppa_do_scale_rotate_mirror(client, &c)).unwrap();
        let deadline = t0 + i64::from(timeout_ms) * 1000;
        while DONE.load(Ordering::Acquire) == before {
            if sys::esp_timer_get_time() > deadline {
                return None;
            }
        }
        Some((sys::esp_timer_get_time() - t0) as u32)
    }
}

#[embassy_executor::task]
async fn app() {
    let wakey::board::Board { mut display, .. } = wakey::board::Board::init().expect("board");
    let fb = display.back_buffer().await.as_mut_ptr() as *mut c_void;
    unsafe {
        let mut client: sys::ppa_client_handle_t = core::ptr::null_mut();
        let mut cfg: sys::ppa_client_config_t = core::mem::zeroed();
        cfg.oper_type = sys::ppa_operation_t_PPA_OPERATION_SRM;
        cfg.max_pending_trans_num = 4;
        esp!(sys::ppa_register_client(&cfg, &mut client)).unwrap();
        let cbs = sys::ppa_event_callbacks_t {
            on_trans_done: Some(done),
        };
        esp!(sys::ppa_client_register_event_callbacks(client, &cbs)).unwrap();

        // 1. Throughput: a 640×960 layer in PSRAM, rotate a 640×720 window.
        let layer = sys::heap_caps_aligned_alloc(
            128,
            640 * 960 * 2,
            sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_DMA,
        );
        assert!(!layer.is_null());
        for (h, label) in [
            (720u32, "640x720 window"),
            (360, "640x360 half"),
            (96, "640x96 row"),
        ] {
            let mut total = 0;
            let n = 10;
            for i in 0..n {
                let by = (i * 23) % (960 - h);
                total += rotate(
                    client,
                    R90,
                    layer,
                    640,
                    960,
                    (0, by, 640, h),
                    fb,
                    (0, 0),
                    500,
                )
                .expect("throughput case stalled");
            }
            let us = total / n;
            log::info!(
                "ppa_probe: {label}: {:.2} ms, {:.0} Mpx/s",
                us as f32 / 1000.0,
                (640 * h) as f32 / us as f32
            );
        }

        // 1b. Unrotated copy from a portrait layer (960 wide × 640 tall):
        //     the compositor's per-frame job if the layer is stored rotated.
        let player = sys::heap_caps_aligned_alloc(
            128,
            960 * 640 * 2,
            sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_DMA,
        );
        assert!(!player.is_null());
        for (w, label) in [
            (720u32, "copy 720x640 window"),
            (360, "copy 360x640 half"),
            (96, "copy 96x640 slot"),
        ] {
            let mut total = 0;
            let n = 10;
            for i in 0..n {
                let bx = (i * 23) % (960 - w);
                total += rotate(
                    client,
                    R0,
                    player,
                    960,
                    640,
                    (bx, 0, w, 640),
                    fb,
                    (0, 0),
                    500,
                )
                .expect("copy case stalled");
            }
            let us = total / n;
            log::info!(
                "ppa_probe: {label}: {:.2} ms, {:.0} Mpx/s",
                us as f32 / 1000.0,
                (640 * w) as f32 / us as f32
            );
        }
        // 1c. The same window with the 2D DMA (no PPA).
        let mut fbcpy: FbcpyHandle = core::ptr::null_mut();
        let dummy = 0u32;
        esp!(esp_async_fbcpy_install(
            (&raw const dummy).cast(),
            &mut fbcpy
        ))
        .unwrap();
        for (w, label) in [
            (720usize, "dma2d copy 720x640"),
            (96, "dma2d copy 96x640"),
            (3, "dma2d copy 3x640"),
        ] {
            let mut total = 0;
            let n = 10;
            for i in 0..n {
                let mut d = FbcpyDesc {
                    src_buffer: player,
                    dst_buffer: fb,
                    src_buffer_size_x: 960,
                    src_buffer_size_y: 640,
                    dst_buffer_size_x: 720,
                    dst_buffer_size_y: 1280,
                    src_offset_x: (i * 23) % (960 - w),
                    src_offset_y: 0,
                    dst_offset_x: 0,
                    dst_offset_y: 0,
                    copy_size_x: w,
                    copy_size_y: 640,
                    pixel_format_unique_id: sys::lcd_color_format_t_LCD_COLOR_FMT_RGB565,
                };
                let before = FB_DONE.load(Ordering::Acquire);
                let t0 = sys::esp_timer_get_time();
                esp!(esp_async_fbcpy(
                    fbcpy,
                    &mut d,
                    Some(fb_done),
                    core::ptr::null_mut()
                ))
                .unwrap();
                while FB_DONE.load(Ordering::Acquire) == before {
                    assert!(
                        sys::esp_timer_get_time() - t0 < 500_000,
                        "dma2d copy stalled"
                    );
                }
                total += (sys::esp_timer_get_time() - t0) as u32;
            }
            let us = total / n as u32;
            log::info!(
                "ppa_probe: {label}: {:.2} ms, {:.0} Mpx/s",
                us as f32 / 1000.0,
                (640 * w) as f32 / us as f32
            );
        }

        // Narrow unrotated copies (ring-buffer wrap point).
        let mut ok = 0;
        'narrow: for _round in 0..20 {
            for w in (1..=16u32).rev() {
                for ox in [0u32, 1, 713] {
                    if ox + w > 720 {
                        continue;
                    }
                    if rotate(
                        client,
                        R0,
                        player,
                        960,
                        640,
                        (17, 0, w, 640),
                        fb,
                        (ox, 0),
                        200,
                    )
                    .is_none()
                    {
                        log::error!("ppa_probe: unrotated copy {w} px wide at x {ox} STALLED");
                        break 'narrow;
                    }
                    ok += 1;
                }
            }
        }
        log::info!("ppa_probe: {ok} narrow unrotated copies completed");

        // 2. Stall sweep over strip heights (input in internal RAM).
        let inb = sys::heap_caps_aligned_alloc(
            128,
            1280 * 16 * 2,
            sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_DMA,
        );
        assert!(!inb.is_null());
        let mut ok = 0;
        'sweep: for _round in 0..20 {
            for h in (1..=16u32).rev() {
                for ox in [396u32, 397] {
                    for (ix, oy) in [(0u32, 640u32), (640, 0)] {
                        if rotate(
                            client,
                            R90,
                            inb,
                            1280,
                            h,
                            (ix, 0, 640, h),
                            fb,
                            (ox, oy),
                            200,
                        )
                        .is_none()
                        {
                            log::error!("ppa_probe: strip of {h} lines at ({ox},{oy}) STALLED");
                            break 'sweep;
                        }
                        ok += 1;
                    }
                }
            }
        }
        log::info!("ppa_probe: done, {ok} strips completed");
    }
    loop {
        FreeRtos::delay_ms(1000);
    }
}
