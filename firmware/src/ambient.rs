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
//! backlight barely dims, so at night the pixels are scaled down as well),
//! and the thread fades both there.

use crate::board::Backlight;
use crate::board::camera;
use crate::board::i2c::SharedI2c;
use crate::psram::PsramBuffer;
use crate::runtime::Thread;
use esp_idf_svc::sys::{self, esp};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// ISP interrupt registers (raw, enable, clear) and the bits of FIFO
/// overflow and buffer full (`ISP_LL_EVENT_ASYNC_FIFO_OVF`, `_BUF_FULL`).
const ISP_INT_RAW: usize = 0x500A_1064;
const ISP_INT_ENA: usize = 0x500A_106C;
const ISP_INT_CLR: usize = 0x500A_1070;
const ISP_OVERFLOW: u32 = (1 << 1) | (1 << 2);

/// Between measurements.
const PERIOD: Duration = Duration::from_secs(10);
/// The panel's backlight barely dims with the PWM duty: 1 % is its
/// dimmest visible level, 2–40 % all look the same (a plateau), only above
/// 40 % does it get visibly brighter (measured by eye, 200 Hz–44.1 kHz).
/// So there are three ranges of room light (average pixel value at the
/// longest exposure and 1× gain):
/// - night, below [`LIGHT_NIGHT`]: [`LEVEL_NIGHT`] and every pixel at
///   `night_pixels` (config `display.night_pixels_percent`, the
///   [`Screen`]'s pixel scaling); left only above
///   [`LIGHT_NIGHT_END`];
/// - a lit room: [`LEVEL_ROOM`];
/// - from [`LIGHT_ROOM_END`] to [`LIGHT_BRIGHT`]: [`LEVEL_ROOM_TOP`] to
///   [`LEVEL_MAX`] on a log scale.
const LEVEL_NIGHT: f32 = 0.01;
const LEVEL_ROOM: f32 = 0.13;
const LEVEL_ROOM_TOP: f32 = 0.4;
const LEVEL_MAX: f32 = 1.0;
const LIGHT_NIGHT: f32 = 2.0;
const LIGHT_NIGHT_END: f32 = 4.0;
const LIGHT_ROOM_END: f32 = 30.0;
const LIGHT_BRIGHT: f32 = 300.0;
/// A change of brightness takes this long (3 s looks jumpy).
const FADE: Duration = Duration::from_secs(8);
/// Steps of the pixel brightness in a fade: each repaints the whole window
/// (~0.2 s), so about as many as fit into [`FADE`].
const PIXEL_STEPS: f32 = 40.0;
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
/// Pixels of one frame.
const PIXELS: usize = (camera::WIDTH * camera::HEIGHT) as usize;

/// The room is dark (night mode), from the last measurement.
static NIGHT: AtomicBool = AtomicBool::new(false);
/// The backlight level and the last light reading (f32 bits), for the
/// debug overlay.
static BACKLIGHT: AtomicU32 = AtomicU32::new(0);
static LIGHT: AtomicU32 = AtomicU32::new(0);
/// A frame finished into the meter's buffer (set by the driver's ISR
/// callback).
static FRAME_DONE: AtomicBool = AtomicBool::new(false);

/// True while the room is dark (night mode; `app::power` lets the display
/// sleep then).
pub fn night() -> bool {
    NIGHT.load(Ordering::Relaxed)
}

/// The backlight level last set (0.0–1.0).
pub fn backlight_level() -> f32 {
    f32::from_bits(BACKLIGHT.load(Ordering::Relaxed))
}

/// The last light reading (see [`LIGHT_NIGHT`]), 0 before the first.
pub fn last_light() -> f32 {
    f32::from_bits(LIGHT.load(Ordering::Relaxed))
}

fn set_backlight(backlight: &mut Backlight, level: f32) -> anyhow::Result<()> {
    BACKLIGHT.store(level.to_bits(), Ordering::Relaxed);
    backlight.set_level(level)
}

/// The display as the light meter sees it: whether it sleeps (backlight
/// off), and the brightness of its pixels (`full` = unchanged).
#[derive(Clone, Copy)]
pub struct Screen {
    pub asleep: fn() -> bool,
    pub pixels: fn() -> u16,
    pub set_pixels: fn(u16),
    pub full: u16,
}

/// Starts the light meter; it owns the backlight from now on and sets the
/// pixel brightness on `screen` (`night_pixels_percent` in the dark).
/// Without a working camera the backlight stays at full brightness
/// (logged).
pub fn start(bus: SharedI2c, mut backlight: Backlight, screen: Screen, night_pixels_percent: u8) {
    let night_pixels = (u16::from(night_pixels_percent) * screen.full / 100).max(1);
    let run = move || {
        let mut meter = match Meter::new(&bus) {
            Ok(m) => m,
            Err(e) => {
                log::error!("ambient: no light meter, full brightness: {e}");
                let _ = set_backlight(&mut backlight, LEVEL_MAX);
                return;
            }
        };
        let mut level = LEVEL_MAX;
        let mut night = false;
        let mut asleep = false;
        let _ = set_backlight(&mut backlight, level);
        let mut next = Instant::now();
        loop {
            // The display's sleep: backlight off at once, and back on once
            // it is awake again.
            if (screen.asleep)() != asleep {
                asleep = !asleep;
                let _ = set_backlight(&mut backlight, if asleep { 0.0 } else { level });
            }
            if Instant::now() >= next {
                next = Instant::now() + PERIOD;
                match meter.measure(&bus) {
                    Ok(light) => {
                        LIGHT.store(light.to_bits(), Ordering::Relaxed);
                        night = if night {
                            light < LIGHT_NIGHT_END
                        } else {
                            light < LIGHT_NIGHT
                        };
                        NIGHT.store(night, Ordering::Relaxed);
                        let target = level_for(light, night);
                        let pixels = if night { night_pixels } else { screen.full };
                        log::info!(
                            "ambient: backlight {:.1} %, pixels {} %",
                            target * 100.0,
                            u32::from(pixels) * 100 / u32::from(screen.full)
                        );
                        if asleep {
                            // Nothing to see: take the new values at once.
                            level = target;
                            (screen.set_pixels)(pixels);
                        } else {
                            fade(&screen, &mut backlight, &mut level, target, pixels);
                        }
                    }
                    Err(e) => log::warn!("ambient: {e}"),
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    if let Err(e) = Thread::new(c"ambient", 6 * 1024).spawn(run) {
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
/// over [`FADE`]. The backlight steps every 50 ms; the pixels change in
/// [`PIXEL_STEPS`] steps.
fn fade(screen: &Screen, backlight: &mut Backlight, level: &mut f32, target: f32, pixels: u16) {
    const STEP: Duration = Duration::from_millis(50);
    let steps = (FADE.as_millis() / STEP.as_millis()) as u32;
    let (from, pixels_from) = (*level, (screen.pixels)());
    if from == target && pixels_from == pixels {
        return;
    }
    for i in 1..=steps {
        if (screen.asleep)() {
            // The display went to sleep: the backlight stays off.
            break;
        }
        let t = i as f32 / steps as f32;
        // Geometric steps: even to the eye.
        let _ = set_backlight(backlight, from * (target / from).powf(t));
        let part = (t * PIXEL_STEPS).floor() / PIXEL_STEPS;
        let p = f32::from(pixels_from) * (f32::from(pixels) / f32::from(pixels_from)).powf(part);
        (screen.set_pixels)(p.round() as u16);
        std::thread::sleep(STEP);
    }
    (screen.set_pixels)(pixels);
    *level = target;
    if (screen.asleep)() {
        let _ = set_backlight(backlight, 0.0);
    }
}

/// The camera pipeline: CSI receiver → ISP → `buf`. Taken down when
/// dropped (also halfway set up).
struct Meter {
    ctlr: sys::esp_cam_ctlr_handle_t,
    isp: sys::isp_proc_handle_t,
    /// One RGB565 frame, written by the CSI receiver's DMA.
    buf: PsramBuffer,
    /// Index into [`SETTINGS`] of the last good reading.
    setting: usize,
}

unsafe extern "C" fn frame_done(
    _ctlr: sys::esp_cam_ctlr_handle_t,
    _trans: *mut sys::esp_cam_ctlr_trans_t,
    _user: *mut core::ffi::c_void,
) -> bool {
    FRAME_DONE.store(true, Ordering::Release);
    false
}

impl Meter {
    fn new(bus: &SharedI2c) -> anyhow::Result<Self> {
        camera::init(bus)?;
        let buf = PsramBuffer::new(PIXELS * 2, sys::MALLOC_CAP_DMA)
            .ok_or_else(|| anyhow::anyhow!("no PSRAM for the frame"))?;
        let mut meter = Self {
            ctlr: core::ptr::null_mut(),
            isp: core::ptr::null_mut(),
            buf,
            setting: SETTINGS.len() - 1,
        };
        // SAFETY: driver calls with configurations that live for each
        // call; the handles are kept in `meter`, whose drop deletes them;
        // the callback is a plain function. The ISP register is written
        // after `esp_isp_enable` (its clock is on).
        unsafe {
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
            esp!(sys::esp_cam_new_csi_ctlr(&csi, &mut meter.ctlr))?;
            let cbs = sys::esp_cam_ctlr_evt_cbs_t {
                on_get_new_trans: None,
                on_trans_finished: Some(frame_done),
            };
            esp!(sys::esp_cam_ctlr_register_event_callbacks(
                meter.ctlr,
                &cbs,
                core::ptr::null_mut()
            ))?;
            esp!(sys::esp_cam_ctlr_enable(meter.ctlr))?;

            let mut isp: sys::esp_isp_processor_cfg_t = core::mem::zeroed();
            isp.clk_src = sys::soc_periph_isp_clk_src_t_ISP_CLK_SRC_DEFAULT;
            isp.clk_hz = 80_000_000;
            isp.input_data_source = sys::isp_input_data_source_t_ISP_INPUT_DATA_SOURCE_CSI;
            isp.input_data_color_type = sys::isp_color_t_ISP_COLOR_RAW8;
            isp.output_data_color_type = sys::isp_color_t_ISP_COLOR_RGB565;
            isp.h_res = camera::WIDTH;
            isp.v_res = camera::HEIGHT;
            isp.bayer_order = sys::color_raw_element_order_t_COLOR_RAW_ELEMENT_ORDER_BGGR;
            esp!(sys::esp_isp_new_processor(&isp, &mut meter.isp))?;
            esp!(sys::esp_isp_enable(meter.isp))?;
            // FIFO overflow and buffer full do not interrupt: ESP-IDF's
            // handler logs each from the interrupt, and a burst of them
            // trips the interrupt watchdog. `frame` checks them instead.
            let ena = ISP_INT_ENA as *mut u32;
            ena.write_volatile(ena.read_volatile() & !ISP_OVERFLOW);
        }
        log::info!("ambient: camera light meter ready");
        Ok(meter)
    }

    /// Wakes the sensor, finds an exposure with a usable average, puts it
    /// to sleep again. Returns the light: the average as it would be at the
    /// longest exposure and 1× gain.
    fn measure(&mut self, bus: &SharedI2c) -> anyhow::Result<f32> {
        let started = Instant::now();
        let (lines, gain) = SETTINGS[self.setting];
        camera::set_exposure(bus, lines, gain)?;
        camera::stream(bus, true)?;
        // SAFETY: the controller is enabled (`new`).
        unsafe { esp!(sys::esp_cam_ctlr_start(self.ctlr))? };
        let result = self.settle(bus);
        // SAFETY: as above.
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
        // SAFETY: ISP registers (the ISP is enabled).
        unsafe { (ISP_INT_CLR as *mut u32).write_volatile(ISP_OVERFLOW) };
        let mut trans = sys::esp_cam_ctlr_trans_t {
            buffer: self.buf.as_mut_ptr().cast(),
            buflen: PIXELS * 2,
            received_size: 0,
        };
        // SAFETY: the buffer holds a whole frame and lives as long as the
        // controller; only this thread reads it, after `FRAME_DONE`.
        unsafe { esp!(sys::esp_cam_ctlr_receive(self.ctlr, &mut trans, 100))? };
        // A frame takes 200 ms at 5 fps.
        let deadline = Instant::now() + Duration::from_millis(1000);
        while !FRAME_DONE.load(Ordering::Acquire) {
            anyhow::ensure!(Instant::now() < deadline, "no frame from the camera");
            std::thread::sleep(Duration::from_millis(5));
        }
        // SAFETY: an ISP register (the ISP is enabled).
        let raw = unsafe { (ISP_INT_RAW as *const u32).read_volatile() };
        anyhow::ensure!(raw & ISP_OVERFLOW == 0, "ISP overflow, frame discarded");
        // SAFETY: the buffer holds `PIXELS` RGB565 pixels and is cache-line
        // aligned; the frame is complete and the driver invalidated the
        // cache for the buffer before the callback: these reads see it.
        // No DMA writes until the next `receive`.
        let frame =
            unsafe { core::slice::from_raw_parts(self.buf.as_mut_ptr().cast::<u16>(), PIXELS) };
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

impl Drop for Meter {
    fn drop(&mut self) {
        // SAFETY: handles from `new` (null if not created); errors of the
        // disable calls only mean it was not enabled.
        unsafe {
            if !self.isp.is_null() {
                sys::esp_isp_disable(self.isp);
                sys::esp_isp_del_processor(self.isp);
            }
            if !self.ctlr.is_null() {
                sys::esp_cam_ctlr_disable(self.ctlr);
                sys::esp_cam_ctlr_del(self.ctlr);
            }
        }
    }
}
