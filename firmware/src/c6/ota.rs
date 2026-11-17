//! The C6's images and their installation: read from the flash partition
//! `c6` and written into the C6's other app slot over esp_hosted's slave
//! OTA RPCs (begin, write in chunks, end, optionally activate).

use super::Transport;
use super::peer::text;
use esp_idf_svc::sys;
use std::ops::Range;
use std::time::Instant;

/// The C6's images in the flash partition "c6" (`partitions.csv`, written
/// by `scripts/flash.sh` from `c6/pack.py`), at these offsets, each after
/// "WKC6" and its length. Not built into the app: the app's constants are
/// copied into PSRAM at boot, and 2.2 MB there leave too little for
/// esp_hosted's buffers.
#[derive(Clone, Copy, Debug)]
pub(super) enum Image {
    /// esp_hosted with Wakey's addition (`c6/hosted`).
    Hosted = 0,
    /// The AirDrop receiver (`c6/airdrop`).
    Receiver = 0x20_0000,
}

/// Project and version in an app image: `esp_app_desc_t` (`version` and
/// `project_name`) after the 24-byte image header and the first segment's
/// 8-byte header.
const APP_VERSION: Range<usize> = 48..80;
const APP_PROJECT: Range<usize> = 80..112;
/// An app image starts with this byte (`ESP_IMAGE_HEADER_MAGIC`).
const IMAGE_MAGIC: u8 = 0xE9;

/// Bytes per write RPC (esp_hosted's example uses 1400–1500).
const CHUNK: usize = 1400;

/// The partition, and the image's offset and length in it.
fn locate(which: Image) -> anyhow::Result<(*const sys::esp_partition_t, usize, usize)> {
    // SAFETY: a lookup in the partition table; the pointer stays valid.
    let part = unsafe {
        sys::esp_partition_find_first(
            sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
            sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_DATA_UNDEFINED,
            c"c6".as_ptr(),
        )
    };
    anyhow::ensure!(!part.is_null(), "no \"c6\" partition (partitions.csv)");
    let at = which as usize;
    let mut head = [0u8; 8];
    read(part, at, &mut head)?;
    anyhow::ensure!(
        &head[..4] == b"WKC6",
        "the C6 images are missing (scripts/flash.sh)"
    );
    let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
    Ok((part, at + 8, len))
}

fn read(part: *const sys::esp_partition_t, at: usize, buf: &mut [u8]) -> anyhow::Result<()> {
    // SAFETY: `part` comes from the partition table, `buf` is valid for
    // `buf.len()` bytes; the partition API checks the range.
    let err = unsafe { sys::esp_partition_read(part, at, buf.as_mut_ptr().cast(), buf.len()) };
    anyhow::ensure!(err == 0, "reading the c6 partition at {at}: {err}");
    Ok(())
}

/// The project name and version of an image in the partition.
pub(super) fn describe(which: Image) -> anyhow::Result<(String, String)> {
    let (part, at, _) = locate(which)?;
    let mut head = [0u8; APP_PROJECT.end];
    read(part, at, &mut head)?;
    anyhow::ensure!(head[0] == IMAGE_MAGIC, "{which:?}: not an app image");
    Ok((text(&head[APP_PROJECT]), text(&head[APP_VERSION])))
}

/// The version of an image in the partition, empty if it cannot be read.
pub(super) fn version(which: Image) -> String {
    describe(which).map_or_else(
        |e| {
            log::warn!("c6: {e}");
            String::new()
        },
        |(_, version)| version,
    )
}

/// The image into the C6's other slot; `activate`: and boot it (esp_hosted
/// restarts the C6). The C6 checks the image at the end; a failure before
/// leaves that slot unusable but the running firmware as it was.
pub(super) fn install(_transport: &Transport, which: Image, activate: bool) -> anyhow::Result<()> {
    let t = Instant::now();
    let (part, at, len) = locate(which)?;
    // SAFETY: plain calls (this and the RPCs below); the transport is up.
    let err = unsafe { sys::esp_hosted_slave_ota_begin() };
    anyhow::ensure!(err == 0, "ota_begin: {err}");
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0;
    let mut next_report = 0;
    while sent < len {
        let n = CHUNK.min(len - sent);
        read(part, at + sent, &mut buf[..n])?;
        // SAFETY: `buf` holds `n` valid bytes for the call.
        let err = unsafe { sys::esp_hosted_slave_ota_write(buf.as_mut_ptr(), n as u32) };
        anyhow::ensure!(err == 0, "ota_write at {sent}: {err}");
        sent += n;
        if sent * 10 / len >= next_report {
            log::info!("c6: installing {which:?}, {} %", sent * 100 / len);
            next_report = sent * 10 / len + 1;
        }
    }
    // SAFETY: as above.
    let err = unsafe { sys::esp_hosted_slave_ota_end() };
    anyhow::ensure!(err == 0, "ota_end: {err}");
    if activate {
        // SAFETY: as above.
        let err = unsafe { sys::esp_hosted_slave_ota_activate() };
        anyhow::ensure!(err == 0, "ota_activate: {err}");
    }
    log::info!("c6: {which:?} installed in {} s", t.elapsed().as_secs());
    Ok(())
}
