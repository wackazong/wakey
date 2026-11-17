//! Downloading one track: the stream manifest, then the file (one file
//! from mirrors, or DASH segments unpacked to FLAC) through the card
//! writer.

use super::Tidal;
use super::card_writer::{BLOCK, CardWriter};
use super::session::{Error, Result};
use super::store;
use crate::board::sd;
use crate::http;
use std::cell::Cell;
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};
use wakey_core::audio_file::{AudioFile, Container};
use wakey_core::tidal::api::{self, Track};
use wakey_core::tidal::flac_mp4::Unpacker;
use wakey_core::tidal::manifest::MediaPlan;
use wakey_core::tidal::sync;

impl Tidal {
    /// Downloads one track to `tidal/<id>.flac` (through `<id>.flac.part`,
    /// renamed once complete and readable as FLAC).
    pub(super) fn download(&self, track: &Track) -> Result<()> {
        let s = self.session()?;
        let (status, body) = self.call(&s, &api::playback_info(&s, track.id), "stream")?;
        let plan = api::parse_playback_info(status, &body, track.id, f64::from(track.duration_s))?;
        std::fs::create_dir_all(sd::path(sync::DIR))?;
        let part = store::part_path(track.id);
        let t = Instant::now();
        self.set_progress(track.id, 0.0);
        let result = match &plan {
            MediaPlan::Mirrors(urls) => self.fetch_file(track.id, urls, &part),
            MediaPlan::Segments(urls) => self.fetch_segments(track.id, urls, &part),
        }
        .map_err(|e| {
            if self.deferred(track.id) {
                Error::Deferred
            } else {
                e
            }
        });
        self.lock().downloading = None;
        let bytes = match result {
            Ok(b) => b,
            // Stopped for another track: one file goes on later where it
            // stopped (DASH segments start over).
            Err(Error::Deferred) if matches!(plan, MediaPlan::Mirrors(_)) => {
                self.lock().resumable.insert(track.id);
                self.bump();
                return Err(Error::Deferred);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                self.bump();
                return Err(e);
            }
        };
        // Readable as FLAC, or it does not count.
        let check = match AudioFile::open(&part) {
            Ok(f) if f.format.container == Container::Flac && f.format.sample_rate > 0 => Ok(()),
            Ok(f) => Err(format!("{:?}, not FLAC", f.format.container)),
            Err(e) => Err(e.to_string()),
        };
        if let Err(why) = check {
            let _ = std::fs::remove_file(&part);
            self.bump();
            return Err(Error::Unusable(why));
        }
        let path = store::track_path(track.id);
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&part, &path)?;
        log::info!(
            "tidal: downloaded {} ({} KiB in {} s)",
            track.id,
            bytes / 1024,
            t.elapsed().as_secs()
        );
        let mut inner = self.lock();
        inner.have.insert(track.id);
        inner.bump();
        Ok(())
    }

    /// One file from the first mirror that delivers it, into `part`; the
    /// bytes in the file. A download stopped for another track before goes
    /// on where it stopped (whole blocks: aligned writes), from the start
    /// if the mirror refuses the range.
    fn fetch_file(&self, id: u64, urls: &[String], part: &Path) -> Result<u64> {
        log::info!("tidal: download {id}: one file, {} mirrors", urls.len());
        let mut offset = if self.lock().resumable.remove(&id) {
            let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
            have / BLOCK as u64 * BLOCK as u64
        } else {
            0
        };
        if offset > 0 {
            log::info!("tidal: download {id}: resuming at {} KiB", offset / 1024);
        }
        let mut last = Error::Unusable("no mirror".into());
        let mut tries: VecDeque<(&String, bool)> = urls.iter().map(|u| (u, false)).collect();
        while let Some((url, retried)) = tries.pop_front() {
            if self.deferred(id) {
                return Err(Error::Deferred);
            }
            let mut file = CardWriter::open(part, offset)?;
            let mut written = 0u64;
            // Time spent writing to the card (the rest: network, TLS,
            // waiting).
            let writing = Cell::new(Duration::ZERO);
            let mut sink = |b: &[u8]| -> anyhow::Result<()> {
                anyhow::ensure!(!self.deferred(id), "deferred");
                let w = Instant::now();
                file.write(b)?;
                writing.set(writing.get() + w.elapsed());
                written += b.len() as u64;
                Ok(())
            };
            let t = Instant::now();
            let at = offset;
            let mut logged = 0u64;
            // Counted over the whole file when resumed.
            let mut progress = |done: u64, total: Option<u64>| {
                let (done, total) = (done + at, total.map(|t| t + at));
                if let Some(total) = total.filter(|t| *t > 0) {
                    self.set_progress(id, done as f32 / total as f32);
                }
                if done >= logged + (2 << 20) {
                    logged = done;
                    log::info!(
                        "tidal: download {id}: {} KiB of {:?} KiB, {} KiB/s, {} of {} ms writing to the card",
                        done / 1024,
                        total.map(|t| t / 1024),
                        (done - at) / 1024 / t.elapsed().as_secs().max(1),
                        writing.get().as_millis(),
                        t.elapsed().as_millis()
                    );
                }
            };
            let result = if at > 0 {
                http::download_range(url, Some(&format!("{at}-")), &mut sink, &mut progress)
            } else {
                http::download(url, &mut sink, &mut progress)
            };
            match result {
                Ok(_) => {
                    file.finish()?;
                    return Ok(at + written);
                }
                // The rest refused (no range, another file): this mirror
                // once more from the start.
                Err(e) if at > 0 && !retried && !self.deferred(id) => {
                    log::warn!("tidal: download {id}: resuming failed ({e}), from the start");
                    offset = 0;
                    tries.push_front((url, true));
                }
                Err(e) => last = Error::Http(e),
            }
        }
        Err(last)
    }

    /// DASH segments of FLAC in fragmented MP4, unpacked into a plain FLAC
    /// file in `part` while they arrive; the bytes in the file.
    fn fetch_segments(&self, id: u64, urls: &[String], part: &Path) -> Result<u64> {
        log::info!("tidal: download {id}: {} DASH segments", urls.len());
        let t = Instant::now();
        let mut file = CardWriter::open(part, 0)?;
        let mut unpacker = Unpacker::new();
        let mut out = Vec::new();
        let written = Cell::new(0u64);
        // Bytes fed to the unpacker (it may take some without giving any
        // out).
        let fed = Cell::new(0u64);
        for (i, url) in urls.iter().enumerate() {
            self.set_progress(id, i as f32 / urls.len() as f32);
            if i % 10 == 0 {
                log::info!(
                    "tidal: download {id}: segment {i} of {}, {} KiB, {} s",
                    urls.len(),
                    written.get() / 1024,
                    t.elapsed().as_secs()
                );
            }
            let mut sink = |b: &[u8]| -> anyhow::Result<()> {
                anyhow::ensure!(!self.deferred(id), "deferred");
                fed.set(fed.get() + b.len() as u64);
                out.clear();
                unpacker.push(b, &mut out)?;
                file.write(&out)?;
                written.set(written.get() + out.len() as u64);
                Ok(())
            };
            // A segment that fails is tried once more only if it gave
            // nothing yet (the unpacker cannot go back).
            let before = fed.get();
            let mut quiet = |_: u64, _: Option<u64>| {};
            if let Err(e) = http::download(url, &mut sink, &mut quiet) {
                log::warn!("tidal: download {id}: segment {i}: {e}");
                if fed.get() != before {
                    return Err(e.into());
                }
                http::download(url, &mut sink, &mut quiet)?;
            }
        }
        unpacker.finish()?;
        file.finish()?;
        Ok(written.get())
    }
}
