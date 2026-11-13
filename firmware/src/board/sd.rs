//! microSD card: SD/MMC host slot 0 in 4-bit mode (P4 IO_MUX pins: CLK 43,
//! CMD 44, D0–D3 39–42), FAT mounted at [`MOUNT_POINT`].
//!
//! The P4's SD pins have their own I/O supply, fed by on-chip LDO channel 4
//! (the Tab5 BSP enables it through `sd_pwr_ctrl_by_on_chip_ldo`). Without
//! it the card never answers (`send_op_cond` times out). The card socket's
//! VDD itself is on SOC_3.3V.

use esp_idf_svc::fs::fatfs::Fatfs;
use esp_idf_svc::hal::gpio::{Gpio39, Gpio40, Gpio41, Gpio42, Gpio43, Gpio44};
use esp_idf_svc::hal::sd::mmc::{SDMMC0, SdMmcHostConfiguration, SdMmcHostDriver};
use esp_idf_svc::hal::sd::{SdCardConfiguration, SdCardDriver};
use esp_idf_svc::io::vfs::MountedFatfs;
use esp_idf_svc::sys::{self as sys, EspError, esp};

pub const MOUNT_POINT: &str = "/sdcard";

/// Verbose SD driver logs while bringing the card up.
const DEBUG_LOGS: bool = false;

/// The mounted card (and the pin supply); unmounted when dropped.
pub struct SdCard {
    _fs: MountedFatfs<Fatfs<SdCardDriver<SdMmcHostDriver<'static>>>>,
    _pin_supply: PinSupply,
}

/// LDO channel 4 at 3.3 V for the SD pins; released on drop.
struct PinSupply(sys::esp_ldo_channel_handle_t);

impl PinSupply {
    fn on() -> Result<Self, EspError> {
        let mut handle: sys::esp_ldo_channel_handle_t = core::ptr::null_mut();
        unsafe {
            let mut cfg: sys::esp_ldo_channel_config_t = core::mem::zeroed();
            cfg.chan_id = 4;
            cfg.voltage_mv = 3300;
            esp!(sys::esp_ldo_acquire_channel(&cfg, &mut handle))?;
        }
        Ok(Self(handle))
    }
}

impl Drop for PinSupply {
    fn drop(&mut self) {
        unsafe { sys::esp_ldo_release_channel(self.0) };
    }
}

pub struct SdPins {
    pub clk: Gpio43<'static>,
    pub cmd: Gpio44<'static>,
    pub d0: Gpio39<'static>,
    pub d1: Gpio40<'static>,
    pub d2: Gpio41<'static>,
    pub d3: Gpio42<'static>,
}

pub fn mount(slot: SDMMC0<'static>, p: SdPins) -> Result<SdCard, EspError> {
    if DEBUG_LOGS {
        for tag in [
            c"sdmmc_init",
            c"sdmmc_cmd",
            c"sdmmc_common",
            c"sdmmc_sd",
            c"sdmmc_req",
            c"sdmmc_periph",
        ] {
            unsafe { sys::esp_log_level_set(tag.as_ptr(), sys::esp_log_level_t_ESP_LOG_DEBUG) };
        }
    }
    let pin_supply = PinSupply::on()?;
    let host = SdMmcHostDriver::new_4bits(
        slot,
        p.cmd,
        p.clk,
        p.d0,
        p.d1,
        p.d2,
        p.d3,
        None::<esp_idf_svc::hal::gpio::AnyIOPin>,
        None::<esp_idf_svc::hal::gpio::AnyIOPin>,
        &SdMmcHostConfiguration::new(),
    )?;
    // 40 MHz (high speed) instead of the default 20: downloads write tens of
    // MB while the player reads; a card without it stays at its default.
    let mut cfg = SdCardConfiguration::new();
    cfg.speed_khz = 40_000;
    let mounted = SdCardDriver::new_mmc(host, &cfg)
        .and_then(|card| MountedFatfs::mount(Fatfs::new_sdcard(0, card)?, MOUNT_POINT, 4));
    match mounted {
        Ok(fs) => Ok(SdCard {
            _fs: fs,
            _pin_supply: pin_supply,
        }),
        Err(e) => {
            restore_sdio_host();
            Err(e)
        }
    }
}

/// Puts the SDMMC host back for the C6 after a failed mount. The card and
/// the C6 share the one SDMMC controller (slot 0 and slot 1); esp_hosted
/// initialises it in a C constructor, before `main`. When the card fails
/// (none inserted), esp-idf-hal drops the `SdMmcHostDriver`, and as the
/// only driver it knows it calls `sdmmc_host_deinit`, which tears the whole
/// controller down, slot 1 and the transaction mutex included. esp_hosted's
/// transport task then asserts in its first command to the C6
/// (`xQueueSemaphoreTake queue.c (( pxQueue ))` from `hosted_sdio_card_init`)
/// and the device reboots every few seconds. Re-initialising the host and
/// esp_hosted's slot, from the same sdkconfig values it uses, lets that
/// command succeed.
fn restore_sdio_host() {
    let nc = sys::gpio_num_t_GPIO_NUM_NC;
    let pin = |p: u32| p as sys::gpio_num_t;
    let slot_config = sys::sdmmc_slot_config_t {
        clk: pin(sys::CONFIG_ESP_HOSTED_SDIO_PIN_CLK),
        cmd: pin(sys::CONFIG_ESP_HOSTED_SDIO_PIN_CMD),
        d0: pin(sys::CONFIG_ESP_HOSTED_SDIO_PIN_D0),
        d1: pin(sys::CONFIG_ESP_HOSTED_SDIO_PIN_D1),
        d2: pin(sys::CONFIG_ESP_HOSTED_SDIO_PIN_D2),
        d3: pin(sys::CONFIG_ESP_HOSTED_SDIO_PIN_D3),
        d4: nc,
        d5: nc,
        d6: nc,
        d7: nc,
        __bindgen_anon_1: sys::sdmmc_slot_config_t__bindgen_ty_1 { cd: nc },
        __bindgen_anon_2: sys::sdmmc_slot_config_t__bindgen_ty_2 { wp: nc },
        width: 4,
        flags: 0,
    };
    // SAFETY: plain driver calls; the slot configuration is esp_hosted's own.
    let (host, slot) = unsafe {
        (
            sys::sdmmc_host_init(),
            sys::sdmmc_host_init_slot(sys::CONFIG_ESP_HOSTED_SDIO_SLOT as i32, &slot_config),
        )
    };
    if host != 0 || slot != 0 {
        log::error!("sd: SDMMC host for the C6 not restored (init {host}, slot {slot}): Wi-Fi will fail");
    } else {
        log::info!("sd: SDMMC host restored for the C6 (slot {})", sys::CONFIG_ESP_HOSTED_SDIO_SLOT);
    }
}
