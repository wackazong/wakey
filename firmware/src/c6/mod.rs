//! The ESP32-C6 radio co-processor: its power, esp_hosted's transport to
//! it, and its two app slots (esp_hosted with Wakey's addition for Wi-Fi,
//! the AirDrop receiver).
//!
//! The P4 has no radio. The C6 runs esp_hosted's slave on SDIO; the
//! esp_hosted and esp_wifi_remote components make it look like the normal
//! `esp_wifi` driver ([`crate::net`]). Wakey's addition answers peer-data
//! messages ([`peer`]) about the other slot, where the AirDrop receiver
//! lives ([`crate::airdrop`]); both images are installed from the flash
//! partition `c6` by esp_hosted's OTA ([`ota`]) when missing or outdated:
//! [`check`] in a Wi-Fi session finds out, [`C6::set_up`] installs after
//! it.

mod ota;
mod peer;

use crate::board::ioexp::Switch;
use esp_idf_svc::sys;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The esp_hosted this firmware is built for: the host component's version
/// (`Cargo.toml`), which Wakey's image in `firmware/c6/` is built on. The
/// factory firmware (M5Stack's 1.4.1) works, but with warnings at start
/// and a known RX stall; the setup replaces it after the first session.
pub const C6_FIRMWARE: [u32; 3] = [2, 13, 1];
/// Wakey's addition exists from this esp_hosted on: anything older is
/// stock.
const HOSTED_MIN: [u32; 3] = [C6_FIRMWARE[0], C6_FIRMWARE[1], 0];
/// The first esp_hosted slave with the OTA activate RPC; older ones switch
/// to the new image at the OTA's end.
const ACTIVATE_MIN: [u32; 3] = [2, 6, 0];

/// The C6, switched by its 3.3 V (expander pin WLAN_PWR_EN). Owned by the
/// `wifi` thread.
pub struct C6 {
    power: Switch,
}

impl C6 {
    pub fn new(power: Switch) -> Self {
        Self { power }
    }

    /// Powers the C6 and brings up esp_hosted's transport to it.
    pub fn connect(&self) -> anyhow::Result<Transport> {
        (self.power)(true)?;
        std::thread::sleep(Duration::from_millis(500));
        Transport::up()
    }

    /// Switches the C6 off. Its transport must be down by then
    /// ([`Transport`]).
    pub fn off(&self) {
        if let Err(e) = (self.power)(false) {
            log::warn!("c6: power off: {e}");
        }
    }

    /// After a Wi-Fi session, if [`check`] found the images missing or
    /// outdated: installs them (~35 s the first time). Returns with the C6
    /// off.
    pub fn set_up(&self) {
        if !NEEDS_SETUP.swap(false, Ordering::Relaxed) {
            return;
        }
        match self.prepare(false, &|| true) {
            Ok(()) => log::info!("c6: set up for AirDrop"),
            Err(e) => log::warn!("c6: setting up for AirDrop: {e}"),
        }
        self.off();
    }

    /// Installs what is missing, then boots the AirDrop receiver: the C6
    /// restarts into it and stays powered (the receiver makes esp_hosted
    /// the next boot again, so switching the C6 off ends it). Gives up
    /// between attempts once `wanted` says so.
    pub fn boot_receiver(&self, wanted: &dyn Fn() -> bool) -> anyhow::Result<()> {
        self.prepare(true, wanted)
    }

    /// Our esp_hosted and the receiver installed if needed (each install
    /// restarts the C6 for another round), then (`boot`) the receiver
    /// booted. The transport is down again when this returns.
    fn prepare(&self, boot: bool, wanted: &dyn Fn() -> bool) -> anyhow::Result<()> {
        // Three rounds: esp_hosted, the receiver (into the slot esp_hosted
        // left), then ready.
        for _ in 0..3 {
            let transport = self.connect()?;
            let ready = ensure_images(&transport)?;
            if ready && boot {
                peer::boot_receiver(&transport)?;
            }
            drop(transport);
            if ready {
                return Ok(());
            }
            self.off();
            std::thread::sleep(Duration::from_millis(300));
            if boot && !wanted() {
                anyhow::bail!("no longer wanted");
            }
        }
        anyhow::bail!("the C6 did not get ready for AirDrop")
    }
}

/// Installs the first image that is missing or outdated on the C6; true if
/// none is (the C6 is ready for AirDrop). After an install the C6 needs a
/// fresh start before the next look.
fn ensure_images(transport: &Transport) -> anyhow::Result<bool> {
    let Some(info) = peer::query(transport)? else {
        let version = transport.firmware_version();
        log::warn!("c6: esp_hosted {version:?} lacks Wakey's addition: installing it");
        // Without the activate RPC the switch happens at the OTA's end, and
        // the C6's restart starts the new image.
        ota::install(
            transport,
            ota::Image::Hosted,
            version.is_none_or(|v| v >= ACTIVATE_MIN),
        )?;
        return Ok(false);
    };
    let hosted = ota::version(ota::Image::Hosted);
    // An unreadable image is no reason to replace what runs.
    if !hosted.is_empty() && info.own_version != hosted {
        // Builds before the version field send none.
        let running = if info.own_version.is_empty() {
            "an older build"
        } else {
            &info.own_version
        };
        log::info!("c6: updating esp_hosted {running} to {hosted}");
        ota::install(transport, ota::Image::Hosted, true)?;
        return Ok(false);
    }
    let receiver = ota::describe(ota::Image::Receiver)?;
    if (info.other_project.as_str(), info.other_version.as_str())
        != (receiver.0.as_str(), receiver.1.as_str())
    {
        log::info!(
            "c6: installing the receiver {} into {} (held '{}' {})",
            receiver.1,
            info.other_label,
            info.other_project,
            info.other_version
        );
        ota::install(transport, ota::Image::Receiver, false)?;
        return Ok(false);
    }
    Ok(true)
}

/// Set by [`check`] when the C6 lacks our current esp_hosted or the
/// receiver.
static NEEDS_SETUP: AtomicBool = AtomicBool::new(false);

/// In a Wi-Fi session, once per boot: logs the C6's firmware and finds out
/// whether it runs our current esp_hosted and holds the current receiver;
/// if not, [`C6::set_up`] installs them after the session.
pub fn check(transport: &Transport) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    // The factory firmware's startup event carries no version (0.0.0);
    // the RPC needs slave ≥ 1.0.0.
    match transport.firmware_version() {
        Some(v) if v == C6_FIRMWARE => log::info!("c6: esp_hosted {v:?}"),
        Some(v) => log::warn!(
            "c6: esp_hosted {v:?}, expected {C6_FIRMWARE:?}: the setup installs it after this \
             session (firmware/c6/README.md)"
        ),
        None => log::warn!("c6: esp_hosted version not available"),
    }
    let ready = match peer::query(transport) {
        Ok(Some(info)) => {
            let hosted = ota::version(ota::Image::Hosted);
            let receiver = ota::describe(ota::Image::Receiver).ok();
            (hosted.is_empty() || info.own_version == hosted)
                && receiver.is_some_and(|(project, version)| {
                    info.other_project == project && info.other_version == version
                })
        }
        Ok(None) => false,
        Err(e) => {
            log::warn!("c6: not asked: {e}");
            return;
        }
    };
    if !ready {
        log::info!("c6: needs setting up for AirDrop (after this session)");
    }
    NEEDS_SETUP.store(!ready, Ordering::Relaxed);
}

/// esp_hosted's transport to the C6 (SDIO, RPC), up while this lives.
///
/// Up: `esp_hosted_init`, then `esp_hosted_connect_to_slave`, which waits
/// until the C6 answers on SDIO (requests sent before go unanswered). The
/// Wi-Fi driver's start does the same; doing it here first makes every
/// user alike.
///
/// Down (drop): `esp_hosted_deinit`, before the C6 is switched off: powered
/// off with the transport up, the next session finds the C6
/// "unresponsive" and aborts. A transport whose link never came up is not
/// torn down: `esp_hosted_deinit` with the SDIO card half set up asserts
/// (`docs/pitfalls.md`). It stays initialised, and the next [`C6::connect`]
/// tries the link again.
pub struct Transport(());

impl Transport {
    fn up() -> anyhow::Result<Self> {
        // SAFETY: plain calls; esp_hosted keeps its own state (a second
        // init while up is a no-op).
        let err = unsafe { sys::esp_hosted_init() };
        anyhow::ensure!(err == 0, "esp_hosted_init: {err}");
        let t = Instant::now();
        // SAFETY: as above.
        let err = unsafe { sys::esp_hosted_connect_to_slave() };
        anyhow::ensure!(err == 0, "esp_hosted_connect_to_slave: {err}");
        log::info!("c6: link up after {} ms", t.elapsed().as_millis());
        Ok(Self(()))
    }

    /// The esp_hosted version the C6 reports (an RPC), or `None` if it
    /// does not answer.
    pub fn firmware_version(&self) -> Option<[u32; 3]> {
        let mut v = sys::esp_hosted_coprocessor_fwver_t::default();
        // SAFETY: `v` is a plain out-parameter, valid for the call.
        let err = unsafe { sys::esp_hosted_get_coprocessor_fwversion(&mut v) };
        (err == 0).then_some([v.major1, v.minor1, v.patch1])
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        let leftovers = hosted_task_buffers();
        // SAFETY: the link is up (`up`), nothing uses the transport any
        // more (the driver is gone before its session's transport).
        let err = unsafe { sys::esp_hosted_deinit() };
        if err != 0 {
            log::warn!("c6: esp_hosted_deinit: {err}");
        }
        free_task_buffers(leftovers);
    }
}

// esp_hosted's tasks have their stacks in PSRAM
// (`CONFIG_ESP_HOSTED_DFLT_TASK_FROM_SPIRAM`): created static, stack and
// control block from the heap, and deleted by `esp_hosted_deinit` without
// freeing either (`hosted_thread_cancel`), ~31 KB per session. Their
// buffers are looked up before the deinit and freed once the tasks are
// gone. Fixed upstream after esp_hosted 2.13.1 (`docs/pitfalls.md`).

/// esp_hosted's tasks (SDIO transport and RPC; `hosted_thread_create`).
const HOSTED_TASKS: [&CStr; 7] = [
    c"sdio_rx_buf",
    c"sdio_read",
    c"sdio_process_rx",
    c"sdio_write",
    c"rpc_rx",
    c"rpc_tx",
    c"rpc_supp_cb",
];

/// A static task's name, stack and control block.
type TaskBuffers = (&'static CStr, *mut u8, *mut u8);

/// The buffers of each running esp_hosted task that was created static.
fn hosted_task_buffers() -> Vec<TaskBuffers> {
    HOSTED_TASKS
        .iter()
        .filter_map(|name| {
            // SAFETY: plain FreeRTOS queries; the handle is checked before
            // it is used.
            unsafe {
                let task = sys::xTaskGetHandle(name.as_ptr());
                if task.is_null() {
                    return None;
                }
                let mut stack: *mut sys::StackType_t = core::ptr::null_mut();
                let mut tcb: *mut sys::StaticTask_t = core::ptr::null_mut();
                (sys::xTaskGetStaticBuffers(task, &mut stack, &mut tcb) == 1)
                    .then(|| (*name, stack.cast(), tcb.cast()))
            }
        })
        .collect()
}

/// Frees the buffers of tasks that are gone (deleted by
/// `esp_hosted_deinit`; a task deleting itself is removed by the idle
/// task, hence the wait). One still there keeps them.
fn free_task_buffers(buffers: Vec<TaskBuffers>) {
    if buffers.is_empty() {
        return;
    }
    std::thread::sleep(Duration::from_millis(100));
    let mut freed = 0;
    for (name, stack, tcb) in buffers {
        // SAFETY: the task no longer exists (looked up by name, including
        // tasks waiting for deletion), so nothing uses its buffers; both
        // came from `heap_caps_*` (`hosted_thread_create`).
        unsafe {
            if !sys::xTaskGetHandle(name.as_ptr()).is_null() {
                log::warn!("c6: task {name:?} still there, its buffers kept");
                continue;
            }
            sys::heap_caps_free(stack.cast());
            sys::heap_caps_free(tcb.cast());
        }
        freed += 1;
    }
    log::debug!("c6: freed the buffers of {freed} esp_hosted tasks");
}
