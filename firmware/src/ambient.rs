//! Screen brightness from the room's light, measured with the camera
//! ([`crate::board::camera`]): very low in the dark, full in daylight.
//!
//! The Tab5 has no light sensor. Every [`PERIOD`] a thread wakes the
//! sensor, takes a few frames through the MIPI-CSI receiver and the ISP
//! (RAW8 → RGB565 into one buffer in PSRAM) and averages a sparse grid of
//! pixels; exposure and gain are stepped until that average is neither
//! black nor white. The light is the average divided by exposure × gain.
//! Then the sensor sleeps again. No frame is kept or sent anywhere: the
//! buffer is overwritten by the next measurement.
//!
//! The light maps to a backlight level ([`level_for`]; the panel's
//! backlight barely dims, so the night halves the pixels instead), and the
//! thread fades the backlight there.

use crate::board::Backlight;
use crate::board::camera;
use crate::board::i2c::SharedI2c;
use esp_idf_svc::sys::{self, esp};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Between measurements.
const PERIOD: Duration = Duration::from_secs(10);
/// The panel's backlight barely dims with the PWM duty: 1 % is its
/// dimmest visible level, 2–40 % all look the same (a plateau), only above
/// 40 % does it get visibly brighter (measured by eye, 200 Hz–44.1 kHz).
/// So there are three ranges of room light (average pixel value at the
/// longest exposure and 1× gain):
/// - night, below [`LIGHT_NIGHT`]: [`LEVEL_NIGHT`] and every pixel at
///   [`NIGHT_PIXELS`] ([`crate::ui::dim`]); left only above
///   [`LIGHT_NIGHT_END`];
/// - a lit room: [`LEVEL_ROOM`];
/// - from [`LIGHT_ROOM_END`] to [`LIGHT_BRIGHT`]: [`LEVEL_ROOM_TOP`] to
///   [`LEVEL_MAX`] on a log scale.
const LEVEL_NIGHT: f32 = 0.01;
/// Pixel brightness at night, in 1/256 (35 %).
const NIGHT_PIXELS: u16 = 90;
/// A change of brightness takes this long.
const FADE: Duration = Duration::from_secs(3);
const LEVEL_ROOM: f32 = 0.13;
const LEVEL_ROOM_TOP: f32 = 0.4;
const LEVEL_MAX: f32 = 1.0;
const LIGHT_NIGHT: f32 = 2.0;
const LIGHT_NIGHT_END: f32 = 4.0;
const LIGHT_ROOM_END: f32 = 30.0;
const LIGHT_BRIGHT: f32 = 300.0;
/// Exposure settings from least to most sensitive: (lines, gain as a power
/// of two).
const SETTINGS: [(u32, usize); 9] = [
    (camera::MIN_EXPOSURE, 0),
    (32, 0),
    (128, 0),
    (512, 0),
    (camera::MAX_EXPOSURE, 0),
    (camera::MAX_EXPOSURE, 1),
    (camera::MAX_EXPOSURE, 2),
    (camera::MAX_EXPOSURE, 3),
    (camera::MAX_EXPOSURE, 4),
];
/// A frame average in this range is a usable reading.
const MEAN_LOW: f32 = 25.0;
const MEAN_HIGH: f32 = 200.0;
/// A frame finished into our buffer (set by the driver's ISR callback).
static FRAME_DONE: AtomicBool = AtomicBool::new(false);

/// Starts the light meter; it owns the backlight from now on. Without a
/// working camera the backlight stays at full brightness (logged).
pub fn start(bus: SharedI2c, mut backlight: Backlight) {
    let run = move || {
        let mut meter = match Meter::new(&bus) {
            Ok(m) => m,
            Err(e) => {
                log::error!("ambient: no light meter, full brightness: {e}");
                let _ = backlight.set_level(LEVEL_MAX);
                return;
            }
        };
        let mut level = LEVEL_MAX;
        let mut night = false;
        let _ = backlight.set_level(level);
        loop {
            match meter.measure(&bus) {
                Ok(light) => {
                    night = if night {
                        light < LIGHT_NIGHT_END
                    } else {
                        light < LIGHT_NIGHT
                    };
                    let target = level_for(light, night);
                    let pixels = if night {
                        NIGHT_PIXELS
                    } else {
                        crate::ui::dim::FULL
                    };
                    log::info!(
                        "ambient: backlight {:.1} %, pixels {} %",
                        target * 100.0,
                        u32::from(pixels) * 100 / 256
                    );
                    fade(&mut backlight, &mut level, target, pixels);
                }
                Err(e) => log::warn!("ambient: {e}"),
            }
            std::thread::sleep(PERIOD);
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("ambient".into())
        .stack_size(6 * 1024)
        .spawn(run)
    {
        log::error!("ambient: thread: {e}");
    }
}

/// The backlight level for `light` (see [`LEVEL_ROOM`]).
fn level_for(light: f32, night: bool) -> f32 {
    if night {
        return LEVEL_NIGHT;
    }
    if light <= LIGHT_ROOM_END {
        return LEVEL_ROOM;
    }
    let t = (light.ln() - LIGHT_ROOM_END.ln()) / (LIGHT_BRIGHT.ln() - LIGHT_ROOM_END.ln());
    // Even steps to the eye: geometric in between.
    LEVEL_ROOM_TOP * (LEVEL_MAX / LEVEL_ROOM_TOP).powf(t.clamp(0.0, 1.0))
}

/// Moves the backlight to `target` and the pixel brightness to `pixels`
/// (1/256) over [`FADE`]. The backlight steps every 50 ms; the pixels
/// change in 10 steps (each repaints the whole window, ~0.2 s).
fn fade(backlight: &mut Backlight, level: &mut f32, target: f32, pixels: u16) {
    const STEP: Duration = Duration::from_millis(50);
    let steps = (FADE.as_millis() / STEP.as_millis()) as u32;
    let (from, pixels_from) = (*level, crate::ui::dim::factor());
    if from == target && pixels_from == pixels {
        return;
    }
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        // Geometric steps: even to the eye.
        let _ = backlight.set_level(from * (target / from).powf(t));
        // The pixels in tenths of the way: 10 repaints, not 60.
        let tenth = (t * 10.0).floor() / 10.0;
        let p = f32::from(pixels_from) * (f32::from(pixels) / f32::from(pixels_from)).powf(tenth);
        crate::ui::dim::set(p.round() as u16);
        std::thread::sleep(STEP);
    }
    crate::ui::dim::set(pixels);
    *level = target;
}

/// The camera pipeline: CSI receiver → ISP → `buf`.
struct Meter {
    ctlr: sys::esp_cam_ctlr_handle_t,
    buf: *mut u16,
    /// Index into [`SETTINGS`] of the last good reading.
    setting: usize,
}

// The handle and buffer are used by the light meter thread only.
unsafe impl Send for Meter {}

unsafe extern "C" fn frame_done(
    _ctlr: sys::esp_cam_ctlr_handle_t,
    _trans: *mut sys::esp_cam_ctlr_trans_t,
    _user: *mut core::ffi::c_void,
) -> bool {
    FRAME_DONE.store(true, Ordering::Release);
    false
}

const PIXELS: usize = (camera::WIDTH * camera::HEIGHT) as usize;

impl Meter {
    fn new(bus: &SharedI2c) -> anyhow::Result<Self> {
        camera::init(bus)?;
        unsafe {
            let buf = sys::heap_caps_aligned_alloc(
                128,
                (PIXELS * 2).next_multiple_of(128),
                sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_DMA,
            )
            .cast::<u16>();
            anyhow::ensure!(!buf.is_null(), "no PSRAM for the frame");

            let mut csi: sys::esp_cam_ctlr_csi_config_t = core::mem::zeroed();
            csi.ctlr_id = 0;
            csi.clk_src = sys::soc_periph_mipi_csi_phy_clk_src_t_MIPI_CSI_PHY_CLK_SRC_DEFAULT;
            csi.h_res = camera::WIDTH;
            csi.v_res = camera::HEIGHT;
            csi.data_lane_num = 1;
            csi.lane_bit_rate_mbps = camera::LANE_MBPS;
            csi.input_data_color_type = sys::cam_ctlr_color_t_CAM_CTLR_COLOR_RAW8;
            csi.output_data_color_type = sys::cam_ctlr_color_t_CAM_CTLR_COLOR_RGB565;
            csi.queue_items = 1;
            let mut ctlr = core::ptr::null_mut();
            esp!(sys::esp_cam_new_csi_ctlr(&csi, &mut ctlr))?;
            let cbs = sys::esp_cam_ctlr_evt_cbs_t {
                on_get_new_trans: None,
                on_trans_finished: Some(frame_done),
            };
            esp!(sys::esp_cam_ctlr_register_event_callbacks(
                ctlr,
                &cbs,
                core::ptr::null_mut()
            ))?;
            esp!(sys::esp_cam_ctlr_enable(ctlr))?;

            let mut isp: sys::esp_isp_processor_cfg_t = core::mem::zeroed();
            isp.clk_src = sys::soc_periph_isp_clk_src_t_ISP_CLK_SRC_DEFAULT;
            isp.clk_hz = 80_000_000;
            isp.input_data_source = sys::isp_input_data_source_t_ISP_INPUT_DATA_SOURCE_CSI;
            isp.input_data_color_type = sys::isp_color_t_ISP_COLOR_RAW8;
            isp.output_data_color_type = sys::isp_color_t_ISP_COLOR_RGB565;
            isp.h_res = camera::WIDTH;
            isp.v_res = camera::HEIGHT;
            isp.bayer_order = sys::color_raw_element_order_t_COLOR_RAW_ELEMENT_ORDER_BGGR;
            let mut proc_ = core::ptr::null_mut();
            esp!(sys::esp_isp_new_processor(&isp, &mut proc_))?;
            esp!(sys::esp_isp_enable(proc_))?;
            log::info!("ambient: camera light meter ready");
            Ok(Self {
                ctlr,
                buf,
                setting: SETTINGS.len() - 1,
            })
        }
    }

    /// Wakes the sensor, finds an exposure with a usable average, puts it
    /// to sleep again. Returns the light: the average as it would be at the
    /// longest exposure and 1× gain.
    fn measure(&mut self, bus: &SharedI2c) -> anyhow::Result<f32> {
        let started = Instant::now();
        let (lines, gain) = SETTINGS[self.setting];
        camera::set_exposure(bus, lines, gain)?;
        camera::stream(bus, true)?;
        unsafe { esp!(sys::esp_cam_ctlr_start(self.ctlr))? };
        let result = self.settle(bus);
        unsafe { esp!(sys::esp_cam_ctlr_stop(self.ctlr))? };
        camera::stream(bus, false)?;
        let (mean, tries) = result?;
        let (lines, gain) = SETTINGS[self.setting];
        let light = mean * camera::MAX_EXPOSURE as f32 / (lines as f32 * (1 << gain) as f32);
        log::info!(
            "ambient: mean {mean:.0} at {lines} lines {}x ({tries} frames, {} ms) → light {light:.2}",
            1 << gain,
            started.elapsed().as_millis()
        );
        Ok(light)
    }

    /// Steps through [`SETTINGS`] until a frame's average is in range (or
    /// at an end). Returns the average and the frames taken.
    fn settle(&mut self, bus: &SharedI2c) -> anyhow::Result<(f32, u32)> {
        // The first frames after waking are not settled yet (darker), and
        // the one after a change may still have the old exposure.
        self.frame()?;
        self.frame()?;
        for tries in 1..=12 {
            let mean = self.frame()?;
            let next = if mean > MEAN_HIGH && self.setting > 0 {
                self.setting - 1
            } else if mean < MEAN_LOW && self.setting < SETTINGS.len() - 1 {
                self.setting + 1
            } else {
                return Ok((mean, tries));
            };
            self.setting = next;
            let (lines, gain) = SETTINGS[next];
            camera::set_exposure(bus, lines, gain)?;
            self.frame()?;
        }
        anyhow::bail!("exposure did not settle")
    }

    /// Takes one frame into `buf` and returns its average brightness
    /// (0–255, every 8th pixel of every 8th line).
    fn frame(&mut self) -> anyhow::Result<f32> {
        FRAME_DONE.store(false, Ordering::Release);
        let mut trans = sys::esp_cam_ctlr_trans_t {
            buffer: self.buf.cast(),
            buflen: PIXELS * 2,
            received_size: 0,
        };
        unsafe { esp!(sys::esp_cam_ctlr_receive(self.ctlr, &mut trans, 100))? };
        let deadline = Instant::now() + Duration::from_millis(300);
        while !FRAME_DONE.load(Ordering::Acquire) {
            anyhow::ensure!(Instant::now() < deadline, "no frame from the camera");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The driver invalidated the cache for the buffer before the
        // callback: these reads see the frame.
        let frame = unsafe { core::slice::from_raw_parts(self.buf, PIXELS) };
        let (mut sum, mut n) = (0u32, 0u32);
        for y in (0..camera::HEIGHT as usize).step_by(8) {
            let row = &frame[y * camera::WIDTH as usize..][..camera::WIDTH as usize];
            for &p in row.iter().step_by(8) {
                let (r, g, b) = ((p >> 11) & 0x1f, (p >> 5) & 0x3f, p & 0x1f);
                // Luma, 0–255: R and B 5 bits, G 6 bits.
                sum += (u32::from(r) * 8 * 2 + u32::from(g) * 4 * 5 + u32::from(b) * 8) / 8;
                n += 1;
            }
        }
        Ok(sum as f32 / n as f32)
    }
}
