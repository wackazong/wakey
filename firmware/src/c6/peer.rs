//! Wakey's addition to esp_hosted on the C6 (`firmware/c6/hosted/wakey_switch.c`):
//! messages over esp_hosted's peer-data channel that ask what the other
//! app slot holds and boot the AirDrop receiver from it.

use super::{HOSTED_MIN, Transport};
use esp_idf_svc::sys;
use std::ffi::c_void;
use std::ops::Range;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Message ids (`WK_*` in `wakey_switch.c`).
const WK_QUERY: u32 = 0x574B_0001;
const WK_BOOT_AIRDROP: u32 = 0x574B_0002;
const WK_INFO: u32 = 0x574B_8001;
const WK_RESULT: u32 = 0x574B_8002;

/// Fields of the `WK_INFO` reply (`struct wk_info` in `wakey_switch.c`,
/// after its protocol byte): NUL-padded strings.
const OTHER_LABEL: Range<usize> = 1..18;
const OTHER_PROJECT: Range<usize> = 18..50;
const OTHER_VERSION: Range<usize> = 50..82;
/// Sent from protocol 2 on; empty from the first build.
const OWN_VERSION: Range<usize> = 82..114;

/// How long our esp_hosted gets to answer.
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Our esp_hosted's answer: its own version and what the other slot holds.
pub(super) struct SlotInfo {
    pub own_version: String,
    pub other_label: String,
    pub other_project: String,
    pub other_version: String,
}

/// Asks our esp_hosted what the other slot holds; `None` if the C6 runs
/// an esp_hosted without Wakey's addition.
pub(super) fn query(transport: &Transport) -> anyhow::Result<Option<SlotInfo>> {
    // Older than our esp_hosted (the factory 1.4.1): it cannot have the
    // addition, and its custom-data RPC fails (-1) instead of answering
    // `ESP_ERR_NOT_FOUND`.
    if transport.firmware_version().is_some_and(|v| v < HOSTED_MIN) {
        return Ok(None);
    }
    let Some(reply) = request(transport, WK_QUERY, WK_INFO)? else {
        return Ok(None);
    };
    let field = |r: Range<usize>| text(&reply[r.start.min(reply.len())..r.end.min(reply.len())]);
    Ok(Some(SlotInfo {
        own_version: field(OWN_VERSION),
        other_label: field(OTHER_LABEL),
        other_project: field(OTHER_PROJECT),
        other_version: field(OTHER_VERSION),
    }))
}

/// Tells our esp_hosted to boot the receiver: it restarts the C6 into it
/// right after answering.
pub(super) fn boot_receiver(transport: &Transport) -> anyhow::Result<()> {
    let result = request(transport, WK_BOOT_AIRDROP, WK_RESULT)?
        .ok_or_else(|| anyhow::anyhow!("no answer to booting the receiver"))?;
    let code = result
        .first_chunk::<4>()
        .map_or(-1, |b| i32::from_le_bytes(*b));
    anyhow::ensure!(code == 0, "the C6 refused to boot the receiver: {code}");
    Ok(())
}

/// A NUL-padded string.
pub(super) fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// The last reply from our esp_hosted: (message id, payload).
static REPLY: Mutex<Option<(u32, Vec<u8>)>> = Mutex::new(None);
static REPLIED: Condvar = Condvar::new();

unsafe extern "C" fn on_reply(id: u32, data: *const u8, len: usize, _ctx: *mut c_void) {
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        // SAFETY: esp_hosted passes `len` valid bytes for the call.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    *REPLY.lock().unwrap_or_else(|e| e.into_inner()) = Some((id, bytes));
    REPLIED.notify_all();
}

/// Sends `msg` to our esp_hosted and waits for `reply`. `None`: esp_hosted
/// has no handler for `msg` (it answers the RPC with `ESP_ERR_NOT_FOUND`:
/// an esp_hosted without Wakey's addition) or sent no reply.
fn request(_transport: &Transport, msg: u32, reply: u32) -> anyhow::Result<Option<Vec<u8>>> {
    // SAFETY: registers a plain function, which esp_hosted keeps.
    let err = unsafe {
        sys::esp_hosted_register_custom_callback(reply, Some(on_reply), core::ptr::null_mut())
    };
    anyhow::ensure!(err == 0, "register_custom_callback: {err}");
    let mut slot = REPLY.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
    // SAFETY: no payload; the transport is up (`_transport`).
    let err = unsafe { sys::esp_hosted_send_custom_data(msg, core::ptr::null(), 0) };
    if err == sys::ESP_ERR_NOT_FOUND {
        return Ok(None);
    }
    anyhow::ensure!(err == 0, "send_custom_data: {err}");
    let deadline = Instant::now() + REPLY_TIMEOUT;
    loop {
        if let Some((id, data)) = slot.take()
            && id == reply
        {
            return Ok(Some(data));
        }
        let rest = deadline.saturating_duration_since(Instant::now());
        if rest.is_zero() {
            return Ok(None);
        }
        slot = REPLIED
            .wait_timeout(slot, rest)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}
