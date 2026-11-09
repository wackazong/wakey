//! ESP-IDF's USB host library on the Tab5's USB-A port, plus the one
//! register tweak the Pebble needs.

use esp_idf_svc::sys::{self as sys, EspError, esp};

unsafe extern "C" {
    /// Register block of the high-speed USB-DWC controller (linker symbol).
    static mut USB_DWC_HS: u32;
}

/// Makes the high-speed controller a full/low-speed-only host: sets
/// `HCFG.FSLSSupp` (offset 0x400, bit 2), so it does not answer a device's
/// high-speed chirp during the port reset. Full-speed devices (the Pebble)
/// failed the root-port reset on the Tab5's USB-A port without this. Newer
/// releases of Espressif's USB host stack expose this as
/// `usb_host_config_t::fsls_only`; ESP-IDF 5.5.5's HAL cannot. Must run
/// after `usb_host_install` (which resets the core) and before the root port
/// is powered.
fn set_full_speed_only() {
    unsafe {
        let hcfg = (&raw mut USB_DWC_HS).cast::<u8>().add(0x400).cast::<u32>();
        let v = hcfg.read_volatile();
        hcfg.write_volatile(v | 1 << 2);
    }
}

/// Installs the host library on the high-speed port with the root port
/// unpowered and starts its daemon thread.
pub(super) fn install() -> Result<(), EspError> {
    unsafe {
        let mut cfg: sys::usb_host_config_t = core::mem::zeroed();
        cfg.intr_flags = sys::ESP_INTR_FLAG_LOWMED as i32;
        // peripheral_map 0: the default, the high-speed port (USB-A).
        cfg.root_port_unpowered = true;
        esp!(sys::usb_host_install(&cfg))?;
    }
    set_full_speed_only();
    std::thread::Builder::new()
        .name("usb-lib".into())
        .stack_size(4096)
        .spawn(|| {
            loop {
                let mut flags = 0u32;
                unsafe { sys::usb_host_lib_handle_events(u32::MAX, &mut flags) };
                if flags & sys::USB_HOST_LIB_EVENT_FLAGS_NO_CLIENTS != 0 {
                    unsafe { sys::usb_host_device_free_all() };
                }
            }
        })
        .expect("usb-lib thread");
    Ok(())
}

/// Powers the root port (connection detection, reset, enumeration) or
/// powers it down. After a failed port reset ESP-IDF leaves the port
/// disabled; cycling the power retries.
pub(super) fn set_port_power(on: bool) -> Result<(), EspError> {
    unsafe { esp!(sys::usb_host_lib_set_root_port_power(on)) }
}
