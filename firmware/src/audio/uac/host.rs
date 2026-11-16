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
    // SAFETY: HCFG of the HS controller, whose registers exist and are
    // clocked once `usb_host_install` returned; nothing else writes it
    // before the port is powered.
    unsafe {
        let hcfg = (&raw mut USB_DWC_HS).cast::<u8>().add(0x400).cast::<u32>();
        let v = hcfg.read_volatile();
        hcfg.write_volatile(v | 1 << 2);
    }
}

/// Installs the host library on the high-speed port with the root port
/// unpowered and starts its daemon thread.
pub(super) fn install() -> Result<(), EspError> {
    // SAFETY: plain call with a configuration that lives for it.
    unsafe {
        let mut cfg: sys::usb_host_config_t = core::mem::zeroed();
        cfg.intr_flags = sys::ESP_INTR_FLAG_LOWMED as i32;
        // peripheral_map 0: the default, the high-speed port (USB-A).
        cfg.root_port_unpowered = true;
        esp!(sys::usb_host_install(&cfg))?;
    }
    set_full_speed_only();
    crate::runtime::Thread::new(c"usb-lib", 4096)
        .spawn(|| {
            loop {
                let mut flags = 0u32;
                // SAFETY: the library's daemon loop, as ESP-IDF prescribes.
                unsafe { sys::usb_host_lib_handle_events(u32::MAX, &mut flags) };
                if flags & sys::USB_HOST_LIB_EVENT_FLAGS_NO_CLIENTS != 0 {
                    // SAFETY: as above.
                    unsafe { sys::usb_host_device_free_all() };
                }
            }
        })
        .expect("usb-lib thread");
    Ok(())
}

/// Powers the root port (connection detection, reset, enumeration). Never
/// switched off again: powered down while a device was still enumerating,
/// ESP-IDF's hub driver aborted at the next disconnection (`device.rs`).
pub(super) fn power_root_port() -> Result<(), EspError> {
    // SAFETY: plain call; the host library is installed.
    unsafe { esp!(sys::usb_host_lib_set_root_port_power(true)) }
}
