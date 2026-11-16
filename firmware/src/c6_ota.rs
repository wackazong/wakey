//! An app image into the C6's other slot over esp_hosted's slave OTA
//! (begin, write in chunks, end; activation is the caller's). Used by
//! AirDrop (`airdrop`) for Wakey's esp_hosted and the receiver, during a
//! session with esp_hosted's transport up.

use esp_idf_svc::sys;

/// Bytes per write RPC (esp_hosted's example uses 1400–1500).
const CHUNK: usize = 1400;

/// Writes an image of `len` bytes that `read(offset, buf)` fills piece by
/// piece (an image in a flash partition); `progress` gets the percentage
/// every 10 %. The C6 checks the image at `end`. A failure before leaves
/// the slot unusable but the running firmware as it was.
pub fn write_from(
    len: usize,
    mut read: impl FnMut(usize, &mut [u8]) -> anyhow::Result<()>,
    mut progress: impl FnMut(usize),
) -> anyhow::Result<()> {
    // SAFETY: plain RPCs; the transport is up.
    let err = unsafe { sys::esp_hosted_slave_ota_begin() };
    anyhow::ensure!(err == 0, "ota_begin: {err}");
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0usize;
    let mut next_report = 0usize;
    while sent < len {
        let n = CHUNK.min(len - sent);
        read(sent, &mut buf[..n])?;
        // SAFETY: `buf` holds `n` valid bytes for the call.
        let err = unsafe { sys::esp_hosted_slave_ota_write(buf.as_mut_ptr(), n as u32) };
        anyhow::ensure!(err == 0, "ota_write at {sent}: {err}");
        sent += n;
        if sent * 10 / len >= next_report {
            progress(sent * 100 / len);
            next_report = sent * 10 / len + 1;
        }
    }
    // SAFETY: as above.
    let err = unsafe { sys::esp_hosted_slave_ota_end() };
    anyhow::ensure!(err == 0, "ota_end: {err}");
    Ok(())
}
