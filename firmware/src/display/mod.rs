//! The Tab5's 5" 720×1280 MIPI-DSI panel (2 lanes), driven by ESP-IDF's
//! `esp_lcd` DSI/DPI driver.
//!
//! # How frames reach the glass
//!
//! The DPI controller streams one of two RGB565 frame buffers in PSRAM to
//! the panel continuously (video mode); the CPU never copies pixels to the
//! panel. Rendering goes directly into the *back* buffer, then
//! [`Display::present`]:
//!
//! 1. writes back the CPU cache for the rows that changed (the DMA reads
//!    PSRAM, not the cache),
//! 2. selects the back buffer for the next frame (`esp_lcd_panel_draw_bitmap`
//!    with a pointer into the frame buffer: the driver only switches, it does
//!    not copy).
//!
//! The DMA picks up the selected buffer at the start of the next frame and
//! raises `on_frame_buf_complete` for the frame that just ended. Only after
//! that event is the old front buffer free to draw into, which
//! [`Display::back_buffer`] awaits. Rendering is therefore tear-free and
//! paced by the panel's refresh rate.
//!
//! The ISR and the render loop must run on the same core (the ISR is
//! installed on the core that creates the panel), see [`Display::present`].

pub mod cache;
pub mod dma2d;
mod init_tables;
pub mod ppa;

use core::ffi::c_void;
use core::ops::Range;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::interrupt::{self, asynch::HalIsrNotification};
use esp_idf_svc::hal::lcd::DSI;
use esp_idf_svc::sys::{self as sys, EspError, esp};
use init_tables::InitCmd;

/// Native panel width (portrait), pixels.
pub const WIDTH: usize = 720;
/// Native panel height (portrait), pixels.
pub const HEIGHT: usize = 1280;
/// Pixels per frame buffer.
pub const PIXELS: usize = WIDTH * HEIGHT;

/// The three LCD/touch combinations shipped in the Tab5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelVariant {
    /// First production run: ILI9881C panel, GT911 touch.
    Ili9881c,
    /// ST7123 TDDI (display + touch in one chip), touch firmware 3.
    St7123,
    /// ST7121 panel with ST712x touch, touch firmware 1.
    St7121,
}

struct PanelTiming {
    lane_bit_rate_mbps: f32,
    pixel_clock_mhz: f32,
    hsync_pulse_width: u32,
    hsync_back_porch: u32,
    hsync_front_porch: u32,
    vsync_pulse_width: u32,
    vsync_back_porch: u32,
    vsync_front_porch: u32,
}

impl PanelTiming {
    /// Panel refresh rate that results from the timing, Hz.
    fn refresh_hz(&self) -> f32 {
        let h =
            WIDTH as u32 + self.hsync_pulse_width + self.hsync_back_porch + self.hsync_front_porch;
        let v =
            HEIGHT as u32 + self.vsync_pulse_width + self.vsync_back_porch + self.vsync_front_porch;
        self.pixel_clock_mhz * 1e6 / (h * v) as f32
    }
}

impl PanelVariant {
    /// Timings from M5Stack's M5GFX (`esp32p4_board_data.hpp`, the values
    /// M5Stack's own firmware uses). Espressif's BSP runs the panels slower
    /// (60/70 MHz); at 58 Hz the ST7123 showed visible flicker in black.
    fn timing(self) -> PanelTiming {
        match self {
            // 940 × 1324 at 80 MHz ≈ 64 Hz
            PanelVariant::Ili9881c => PanelTiming {
                lane_bit_rate_mbps: 1040.0,
                pixel_clock_mhz: 80.0,
                hsync_pulse_width: 40,
                hsync_back_porch: 140,
                hsync_front_porch: 40,
                vsync_pulse_width: 4,
                vsync_back_porch: 20,
                vsync_front_porch: 20,
            },
            // 802 × 1510 at 80 MHz ≈ 66 Hz
            PanelVariant::St7123 => PanelTiming {
                lane_bit_rate_mbps: 1040.0,
                pixel_clock_mhz: 80.0,
                hsync_pulse_width: 2,
                hsync_back_porch: 40,
                hsync_front_porch: 40,
                vsync_pulse_width: 2,
                vsync_back_porch: 8,
                vsync_front_porch: 220,
            },
            // 802 × 1524 at 70 MHz ≈ 57 Hz
            PanelVariant::St7121 => PanelTiming {
                lane_bit_rate_mbps: 900.0,
                pixel_clock_mhz: 70.0,
                hsync_pulse_width: 2,
                hsync_back_porch: 40,
                hsync_front_porch: 40,
                vsync_pulse_width: 20,
                vsync_back_porch: 24,
                vsync_front_porch: 200,
            },
        }
    }
}

// ── Frame-complete ISR → async ──────────────────────────────────────────────

/// The DPI driver's allocation of a frame buffer (`esp_lcd_panel_dpi.c`).
const FB_CAPS: u32 = sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT | sys::MALLOC_CAP_DMA;

/// Frames fully scanned out since boot (wraps).
static FRAMES_DONE: AtomicU32 = AtomicU32::new(0);
/// Wakes the task waiting in [`Display::back_buffer`].
static FRAME_DONE: HalIsrNotification = HalIsrNotification::new();

/// `on_frame_buf_complete`: the DMA finished reading a frame and has already
/// (re)started the next one from the currently selected buffer.
unsafe extern "C" fn on_frame_buf_complete(
    _panel: sys::esp_lcd_panel_handle_t,
    _edata: *mut sys::esp_lcd_dpi_panel_event_data_t,
    _user_ctx: *mut c_void,
) -> bool {
    FRAMES_DONE.fetch_add(1, Ordering::Release);
    interrupt::with_isr_yield_signal(|| {
        FRAME_DONE.notify_lsb();
    })
}

// ── Display ─────────────────────────────────────────────────────────────────

pub struct Display {
    variant: PanelVariant,
    panel: sys::esp_lcd_panel_handle_t,
    io: sys::esp_lcd_panel_io_handle_t,
    bus: sys::esp_lcd_dsi_bus_handle_t,
    phy_ldo: sys::esp_ldo_channel_handle_t,
    fbs: [*mut u16; 2],
    /// Index of the buffer the DMA streams (or will stream from the next frame on).
    front: usize,
    /// `FRAMES_DONE` value when the last flip was requested; the old front
    /// buffer is free once the counter has moved past it.
    pending_flip: Option<u32>,
    /// Pixel clock override, else the panel's own.
    pixel_clock_mhz: Option<f32>,
    /// While the stream is suspended: the freed frame buffers' memory, held
    /// for the next stream (see [`Display::suspend_stream`]).
    held: [*mut c_void; 2],
}

impl Display {
    pub fn new(_dsi: DSI<'static>, variant: PanelVariant) -> Result<Self, EspError> {
        let timing = variant.timing();
        let mut this = Self {
            variant,
            panel: ptr::null_mut(),
            io: ptr::null_mut(),
            bus: ptr::null_mut(),
            phy_ldo: ptr::null_mut(),
            fbs: [ptr::null_mut(); 2],
            front: 0,
            pending_flip: None,
            pixel_clock_mhz: None,
            held: [ptr::null_mut(); 2],
        };

        unsafe {
            // VDD_MIPI_DPHY is LDO channel 3 at 2.5 V.
            let mut ldo_cfg: sys::esp_ldo_channel_config_t = core::mem::zeroed();
            ldo_cfg.chan_id = 3;
            ldo_cfg.voltage_mv = 2500;
            esp!(sys::esp_ldo_acquire_channel(&ldo_cfg, &mut this.phy_ldo))?;

        }

        this.open_link()?;
        this.init_panel()?;

        this.start_stream()?;

        log::info!(
            "display: {:?} {}x{} RGB565, {:.1} Hz, frame buffers {:?}",
            variant,
            WIDTH,
            HEIGHT,
            timing.refresh_hz(),
            this.fbs
        );
        Ok(this)
    }

    /// Creates the DSI bus (PHY) and the command interface (DBI).
    fn open_link(&mut self) -> Result<(), EspError> {
        let timing = self.variant.timing();
        unsafe {
            let mut bus_cfg: sys::esp_lcd_dsi_bus_config_t = core::mem::zeroed();
            bus_cfg.bus_id = 0;
            bus_cfg.num_data_lanes = 2;
            // 0 = let the driver choose: PLL_F20M on chip rev < 3 (the Tab5),
            // XTAL on rev ≥ 3. The `..._DEFAULT` constant is the rev ≥ 3
            // choice and aborts in the HAL on rev 1.x.
            bus_cfg.phy_clk_src = 0;
            bus_cfg.lane_bit_rate_mbps = timing.lane_bit_rate_mbps;
            esp!(sys::esp_lcd_new_dsi_bus(&bus_cfg, &mut self.bus))?;

            // Command mode (DBI) for the vendor init sequence.
            let dbi_cfg = sys::esp_lcd_dbi_io_config_t {
                virtual_channel: 0,
                lcd_cmd_bits: 8,
                lcd_param_bits: 8,
            };
            esp!(sys::esp_lcd_new_panel_io_dbi(
                self.bus,
                &dbi_cfg,
                &mut self.io
            ))?;
        }
        Ok(())
    }

    /// Creates the DPI panel and starts streaming fb0 (video mode).
    fn start_stream(&mut self) -> Result<(), EspError> {
        let timing = self.variant.timing();
        // The held memory goes to the new frame buffers.
        for h in &mut self.held {
            unsafe { sys::heap_caps_free(*h) };
            *h = ptr::null_mut();
        }
        unsafe {
            // Video mode (DPI): two frame buffers in PSRAM, allocated by the driver.
            let mut dpi_cfg: sys::esp_lcd_dpi_panel_config_t = core::mem::zeroed();
            dpi_cfg.virtual_channel = 0;
            dpi_cfg.dpi_clk_src =
                sys::soc_periph_mipi_dsi_dpi_clk_src_t_MIPI_DSI_DPI_CLK_SRC_DEFAULT;
            dpi_cfg.dpi_clock_freq_mhz = self.pixel_clock_mhz.unwrap_or(timing.pixel_clock_mhz);
            dpi_cfg.pixel_format = sys::lcd_color_rgb_pixel_format_t_LCD_COLOR_PIXEL_FORMAT_RGB565;
            dpi_cfg.in_color_format = sys::lcd_color_format_t_LCD_COLOR_FMT_RGB565;
            dpi_cfg.num_fbs = 2;
            dpi_cfg.video_timing = sys::esp_lcd_video_timing_t {
                h_size: WIDTH as u32,
                v_size: HEIGHT as u32,
                hsync_pulse_width: timing.hsync_pulse_width,
                hsync_back_porch: timing.hsync_back_porch,
                hsync_front_porch: timing.hsync_front_porch,
                vsync_pulse_width: timing.vsync_pulse_width,
                vsync_back_porch: timing.vsync_back_porch,
                vsync_front_porch: timing.vsync_front_porch,
            };
            // No DMA2D: we render straight into the frame buffers.
            esp!(sys::esp_lcd_new_panel_dpi(
                self.bus,
                &dpi_cfg,
                &mut self.panel
            ))?;

            let mut cbs: sys::esp_lcd_dpi_panel_event_callbacks_t = core::mem::zeroed();
            cbs.__bindgen_anon_1.on_frame_buf_complete = Some(on_frame_buf_complete);
            esp!(sys::esp_lcd_dpi_panel_register_event_callbacks(
                self.panel,
                &cbs,
                ptr::null_mut()
            ))?;

            // Starts the DMA: from here on fb0 is streamed continuously.
            esp!(sys::esp_lcd_panel_init(self.panel))?;

            let mut fb0: *mut c_void = ptr::null_mut();
            let mut fb1: *mut c_void = ptr::null_mut();
            esp!(sys::esp_lcd_dpi_panel_get_frame_buffer(
                self.panel,
                2,
                &mut fb0 as *mut *mut c_void,
                &mut fb1 as *mut *mut c_void,
            ))?;
            self.fbs = [fb0.cast(), fb1.cast()];
        }
        self.front = 0;
        self.pending_flip = None;
        Ok(())
    }

    fn tx(&self, cmd: u8, params: &[u8]) -> Result<(), EspError> {
        let p = if params.is_empty() {
            ptr::null()
        } else {
            params.as_ptr().cast()
        };
        unsafe {
            esp!(sys::esp_lcd_panel_io_tx_param(
                self.io,
                cmd as i32,
                p,
                params.len()
            ))
        }
    }

    fn rx(&self, cmd: u8, out: &mut [u8]) -> Result<(), EspError> {
        unsafe {
            esp!(sys::esp_lcd_panel_io_rx_param(
                self.io,
                cmd as i32,
                out.as_mut_ptr().cast(),
                out.len()
            ))
        }
    }

    fn send_all(&self, cmds: &[InitCmd], extra_delay_ms: u32) -> Result<(), EspError> {
        for c in cmds {
            self.tx(c.cmd, c.params)?;
            let delay = c.delay_ms + extra_delay_ms;
            if delay > 0 {
                FreeRtos::delay_ms(delay);
            }
        }
        Ok(())
    }

    /// Vendor init, ported from the esp-bsp panel drivers (`esp_lcd_ili9881c`,
    /// `esp_lcd_st7123`). The LCD reset line is on the IO expander and has
    /// already been pulsed by the board; this adds a software reset.
    fn init_panel(&self) -> Result<(), EspError> {
        const SWRESET: u8 = 0x01;
        const SLPOUT: u8 = 0x11;
        const MADCTL: u8 = 0x36;
        const COLMOD: u8 = 0x3A;
        match self.variant {
            PanelVariant::Ili9881c => {
                const PAGE_SELECT: u8 = 0xFF;
                self.tx(SWRESET, &[])?;
                FreeRtos::delay_ms(20);
                self.tx(PAGE_SELECT, &[0x98, 0x81, 0x01])?;
                let mut id = [0u8; 3];
                for (i, b) in id.iter_mut().enumerate() {
                    let mut v = [0u8];
                    self.rx(i as u8, &mut v)?;
                    *b = v[0];
                }
                log::info!("display: ILI9881C id {:02x?}", id);
                self.tx(0xB7, &[0x03])?; // pad control: 2 data lanes
                self.tx(PAGE_SELECT, &[0x98, 0x81, 0x00])?;
                self.tx(SLPOUT, &[])?;
                FreeRtos::delay_ms(120);
                self.tx(MADCTL, &[0x00])?;
                self.tx(COLMOD, &[0x55])?; // RGB565
                self.send_all(init_tables::ILI9881C_INIT, 0)
            }
            PanelVariant::St7123 => {
                self.tx(SWRESET, &[])?;
                FreeRtos::delay_ms(120);
                self.send_all(init_tables::ST7123_INIT, 5)
            }
            PanelVariant::St7121 => {
                self.tx(SWRESET, &[])?;
                FreeRtos::delay_ms(120);
                self.send_all(init_tables::ST7121_INIT, 5)
            }
        }
    }

    pub fn variant(&self) -> PanelVariant {
        self.variant
    }

    /// Spike support (`bin/chip_sleep`): streams the DSI host's own colour
    /// bars instead of the frame buffer (`true`; no PSRAM reads), or the
    /// frame buffer again (`false`).
    pub fn set_test_pattern(&mut self, on: bool) -> Result<(), EspError> {
        let pattern = if on {
            sys::mipi_dsi_pattern_type_t_MIPI_DSI_PATTERN_BAR_HORIZONTAL
        } else {
            sys::mipi_dsi_pattern_type_t_MIPI_DSI_PATTERN_NONE
        };
        esp!(unsafe { sys::esp_lcd_dpi_panel_set_pattern(self.panel, pattern) })
    }

    /// Spike support (`bin/chip_sleep`): panel display off and sleep in
    /// (`true`), or sleep out and display on (`false`). The video stream
    /// keeps running.
    pub fn set_panel_sleep(&self, sleep: bool) -> Result<(), EspError> {
        if sleep {
            self.tx(0x28, &[])?;
            FreeRtos::delay_ms(20);
            self.tx(0x10, &[])?;
        } else {
            self.tx(0x11, &[])?;
            FreeRtos::delay_ms(120);
            self.tx(0x29, &[])?;
        }
        FreeRtos::delay_ms(120);
        Ok(())
    }

    /// Spike support (`bin/chip_sleep`): the panel's software reset and
    /// vendor init again (ends with sleep out and display on), with the
    /// video stream running.
    pub fn reinit_panel(&self) -> Result<(), EspError> {
        self.init_panel()
    }

    /// Spike support (`bin/chip_sleep`): takes the whole display link down
    /// (stream, command interface, DSI bus and PHY; the PHY's LDO stays
    /// on). Light sleep stops the PHY's clock: after it, commands stay in
    /// the DSI command FIFO forever. [`link_up`] rebuilds it as at boot.
    ///
    /// [`link_up`]: Self::link_up
    pub fn link_down(&mut self) -> Result<(), EspError> {
        if !self.panel.is_null() {
            self.suspend_stream()?;
        }
        unsafe {
            esp!(sys::esp_lcd_panel_io_del(self.io))?;
            esp!(sys::esp_lcd_del_dsi_bus(self.bus))?;
        }
        self.io = ptr::null_mut();
        self.bus = ptr::null_mut();
        Ok(())
    }

    /// Spike support: DSI bus, panel init (software reset, vendor init,
    /// sleep out, display on) and a new stream, as at boot.
    pub fn link_up(&mut self) -> Result<(), EspError> {
        self.open_link()?;
        self.init_panel()?;
        self.start_stream()
    }

    /// Spike support (`bin/chip_sleep`): stops the video stream (deletes
    /// the DPI panel and its frame buffers); [`resume_stream`] starts a new
    /// one. The DSI link and the panel stay powered.
    ///
    /// [`resume_stream`]: Self::resume_stream
    pub fn suspend_stream(&mut self) -> Result<(), EspError> {
        self.stop_stream_dma();
        esp!(unsafe { sys::esp_lcd_panel_del(self.panel) })?;
        self.panel = ptr::null_mut();
        self.fbs = [ptr::null_mut(); 2];
        // PSRAM is tight in the app (~6.5 MB free, the largest block often
        // just short of one frame buffer): if anything took a piece of the
        // freed buffers before the next stream allocates its own, that
        // failed (ESP_ERR_NO_MEM, seen on the fourth sleep). Hold the
        // memory with the driver's own allocation until then.
        for h in &mut self.held {
            *h = unsafe { sys::heap_caps_calloc(1, PIXELS * 2, FB_CAPS) };
        }
        Ok(())
    }

    /// Stops the stream's DW-GDMA channel. `esp_lcd_panel_del` (ESP-IDF
    /// 5.5.5) frees the channel without disabling it: it keeps streaming
    /// from the freed frame buffers. Alone on the DW-GDMA that goes unseen
    /// (the controller is switched off and reset with its last channel);
    /// with the camera's CSI receiver on it too, the next stream got the
    /// still running channel, and writing its registers was a store access
    /// fault (`dpi_panel_init`, CH1_LLP0). The channel is found by its
    /// source address, which lies in one of our frame buffers.
    fn stop_stream_dma(&self) {
        const DMAC: usize = 0x5008_1000;
        const CHEN0: usize = DMAC + 0x18;
        const CHANNELS: usize = 4;
        let fb_bytes = PIXELS * 2;
        let in_fb = |a: usize| {
            self.fbs
                .iter()
                .any(|&fb| !fb.is_null() && (fb as usize..fb as usize + fb_bytes).contains(&a))
        };
        for ch in 0..CHANNELS {
            // SAFETY: DW-GDMA registers (bus clock on: our channel runs).
            unsafe {
                let enabled = ptr::read_volatile(CHEN0 as *const u32) & (1 << ch) != 0;
                let sar = ptr::read_volatile((DMAC + 0x100 * (ch + 1)) as *const u32) as usize;
                if !enabled || !in_fb(sar) {
                    continue;
                }
                // Enable bit off, with its write-enable bit.
                ptr::write_volatile(CHEN0 as *mut u32, 0x100 << ch);
                for _ in 0..100 {
                    if ptr::read_volatile(CHEN0 as *const u32) & (1 << ch) == 0 {
                        break;
                    }
                    FreeRtos::delay_ms(1);
                }
                log::debug!("display: stream DMA channel {ch} stopped");
            }
        }
    }

    /// Spike support: a new video stream after [`suspend_stream`] (new frame
    /// buffers, contents undefined).
    ///
    /// [`suspend_stream`]: Self::suspend_stream
    pub fn resume_stream(&mut self) -> Result<(), EspError> {
        self.start_stream()
    }

    /// Pixel clock `mhz` (80 MHz = 66 Hz refresh on the ST7123), or the
    /// panel's own with `None`; used while the display sleeps. The frame
    /// buffers are new afterwards (contents undefined).
    ///
    /// A new stream alone leaves the panel black: the whole link is rebuilt
    /// as at boot (panel init, then the stream).
    pub fn set_pixel_clock(&mut self, mhz: Option<f32>) -> Result<(), EspError> {
        self.link_down()?;
        self.pixel_clock_mhz = mhz;
        self.link_up()
    }

    /// Spike support (`bin/chip_sleep`): DCS "get power mode" (0x0A):
    /// bit 4 sleep out, bit 2 display on.
    pub fn power_mode(&self) -> Result<u8, EspError> {
        let mut v = [0u8];
        self.rx(0x0A, &mut v)?;
        Ok(v[0])
    }


    /// Frames the panel has scanned out since boot (wrapping counter).
    pub fn frames_scanned() -> u32 {
        FRAMES_DONE.load(Ordering::Acquire)
    }

    /// Waits until the buffer that is not on screen can be drawn into, and
    /// returns it (row-major, `WIDTH` pixels per row, native RGB565).
    ///
    /// It holds the frame before the one currently shown (double buffering),
    /// so partial renderers must repaint the union of the last two dirty
    /// regions (Slint: `RepaintBufferType::SwappedBuffers`).
    pub async fn back_buffer(&mut self) -> &mut [u16] {
        if let Some(requested_at) = self.pending_flip {
            // The flip is done once one full frame ended after the request.
            while Self::frames_scanned().wrapping_sub(requested_at) as i32 <= 0 {
                FRAME_DONE.wait().await;
            }
            self.pending_flip = None;
        }
        let back = 1 - self.front;
        // SAFETY: the driver allocated PIXELS u16 per buffer; the DMA only
        // reads the front buffer, and `&mut self` prevents aliasing.
        unsafe { core::slice::from_raw_parts_mut(self.fbs[back], PIXELS) }
    }

    /// The back buffer if the last flip is done, without waiting (see
    /// [`back_buffer`](Self::back_buffer)).
    pub fn try_back_buffer(&mut self) -> Option<&mut [u16]> {
        if let Some(requested_at) = self.pending_flip {
            if Self::frames_scanned().wrapping_sub(requested_at) as i32 <= 0 {
                return None;
            }
            self.pending_flip = None;
        }
        let back = 1 - self.front;
        // SAFETY: as in `back_buffer`.
        Some(unsafe { core::slice::from_raw_parts_mut(self.fbs[back], PIXELS) })
    }

    /// The buffer on screen (or about to be, after a flip), for DMA reads
    /// only: the CPU must not write it.
    pub fn front_buffer(&self) -> *mut u16 {
        self.fbs[self.front]
    }

    /// Shows the back buffer from the next frame on. `rows` are the panel
    /// rows (native orientation) that changed since this buffer was last
    /// shown; only their cache lines are written back to PSRAM.
    ///
    /// Must be called from the core that created the display (the core the
    /// DMA ISR runs on), so the ISR cannot interleave with the index switch
    /// and the counter read below.
    pub fn present(&mut self, rows: Range<usize>) -> Result<(), EspError> {
        let back = 1 - self.front;
        // The driver rejects empty areas; a one-row write-back costs nothing.
        let start = rows.start.min(HEIGHT - 1);
        let end = rows.end.clamp(start + 1, HEIGHT);
        unsafe {
            // A pointer inside a frame buffer makes the driver write back
            // the cache for rows start..end and switch buffers, no copy.
            esp!(sys::esp_lcd_panel_draw_bitmap(
                self.panel,
                0,
                start as i32,
                WIDTH as i32,
                end as i32,
                self.fbs[back] as *const c_void,
            ))?;
        }
        // Read after the switch: if the DMA restarted the old buffer just
        // before it, that event is already counted and we wait for the next.
        self.pending_flip = Some(Self::frames_scanned());
        self.front = back;
        Ok(())
    }
}
