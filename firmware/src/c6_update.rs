//! Updates the C6's esp_hosted firmware over SDIO (feature `c6-update`).
//!
//! The image (esp_hosted's slave project for the ESP32-C6, built from the
//! host component's sources: `c6/README.md`) is built into the firmware.
//! In the first Wi-Fi session, if the C6 runs another version than the
//! image says, it goes to
//! the C6 with esp_hosted's slave OTA RPCs: begin, write in chunks, end,
//! activate. The C6 writes it to its other OTA slot and switches to it;
//! slaves before 2.6 (the factory 1.4.1) switch at `end` and do not know
//! `activate`. Then the P4 restarts, which resets the C6 into the new
//! firmware. A failure before `end` leaves the C6 on its old firmware.

use std::time::Duration;

static IMAGE: &[u8] = include_bytes!("../c6/esp_hosted_slave-2.13.1-esp32c6.bin");

/// Compares the C6's `running` version with the image's and updates the
/// C6 if they differ; restarts the P4 after an update.
pub fn run(running: [u32; 3]) {
    let Some(image) = image_version() else {
        log::error!("c6-update: the image is not an ESP app image; nothing done");
        return;
    };
    if image != crate::net::C6_FIRMWARE {
        log::error!(
            "c6-update: the image is {image:?}, the firmware expects {:?} \
             (net::C6_FIRMWARE); nothing done",
            crate::net::C6_FIRMWARE
        );
        return;
    }
    if running == image {
        log::info!("c6-update: C6 runs {image:?}, as the image; nothing to do");
        return;
    }
    log::warn!(
        "c6-update: C6 runs {running:?}, image {image:?} ({} bytes): updating",
        IMAGE.len()
    );
    match update() {
        Ok(()) => {
            log::warn!("c6-update: done; restarting in 3 s");
            std::thread::sleep(Duration::from_secs(3));
            // SAFETY: restarts the chip; does not return.
            unsafe { esp_idf_svc::sys::esp_restart() };
        }
        Err(e) => log::error!("c6-update: {e}"),
    }
}

/// The version in the image's app description (`esp_app_desc_t` after the
/// 24-byte image header and the first 8-byte segment header).
fn image_version() -> Option<[u32; 3]> {
    if IMAGE.first() != Some(&0xE9) {
        return None;
    }
    let magic = u32::from_le_bytes(IMAGE.get(32..36)?.try_into().ok()?);
    if magic != 0xABCD_5432 {
        return None;
    }
    let field = IMAGE.get(48..80)?;
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let text = std::str::from_utf8(&field[..end]).ok()?;
    let mut parts = text.split('.').map(|p| p.parse::<u32>().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

fn update() -> anyhow::Result<()> {
    use esp_idf_svc::sys;
    crate::c6_ota::write(IMAGE, |percent| log::info!("c6-update: {percent} %"))?;
    log::info!("c6-update: end ok (image checked by the C6)");
    // SAFETY: plain RPC; the transport is up.
    let err = unsafe { sys::esp_hosted_slave_ota_activate() };
    if err == 0 {
        log::info!("c6-update: activate ok");
    } else {
        // Slaves before 2.6 have no activate RPC; they switched at `end`.
        log::warn!("c6-update: activate: {err} (expected before slave 2.6)");
    }
    Ok(())
}
