//! Reading a track while it downloads (the sound page's preview).

use super::store::{part_path, track_path};
use super::{Tidal, TrackStatus};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A track being downloaded, read as it arrives: `tidal/<id>.flac.part`
/// while it grows, the finished `tidal/<id>.flac` once renamed. At the end
/// of what has arrived it opens the file again (FatFs fixes the size at
/// opening; the card writer syncs every 256 KiB) and waits while the
/// download goes on. Ends (no more data) when the download fails, and with
/// an error once `cancel` is set (the preview stopped: the player's decoder
/// must not wait for data).
pub struct Growing {
    id: u64,
    tidal: Tidal,
    cancel: Arc<AtomicBool>,
    file: Option<std::fs::File>,
    /// `file` is the finished track: its end is the end.
    done: bool,
    pos: u64,
}

impl Growing {
    pub(super) fn new(id: u64, tidal: Tidal, cancel: Arc<AtomicBool>) -> Self {
        Self {
            id,
            tidal,
            cancel,
            file: None,
            done: false,
            pos: 0,
        }
    }

    /// Opens the finished file, else the part if it has more than `pos`.
    fn reopen(&mut self) -> std::io::Result<bool> {
        if let Ok(mut f) = std::fs::File::open(track_path(self.id)) {
            f.seek(SeekFrom::Start(self.pos))?;
            self.file = Some(f);
            self.done = true;
            return Ok(true);
        }
        if let Ok(mut f) = std::fs::File::open(part_path(self.id))
            && f.metadata()?.len() > self.pos
        {
            f.seek(SeekFrom::Start(self.pos))?;
            self.file = Some(f);
            return Ok(true);
        }
        Ok(false)
    }
}

impl Read for Growing {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Err(std::io::Error::other("preview stopped"));
            }
            if let Some(f) = &mut self.file {
                let n = f.read(buf)?;
                if n > 0 {
                    self.pos += n as u64;
                    return Ok(n);
                }
                if self.done {
                    return Ok(0);
                }
                self.file = None;
            }
            if self.reopen()? {
                continue;
            }
            match self.tidal.track_status(self.id) {
                TrackStatus::Downloading | TrackStatus::Missing => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                // Renamed between the two looks: the finished file is there.
                TrackStatus::Ready if track_path(self.id).exists() => {}
                _ => return Ok(0),
            }
        }
    }
}

impl Seek for Growing {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => match (&self.file, self.done) {
                (Some(f), true) => f.metadata()?.len().checked_add_signed(d),
                _ => return Err(std::io::Error::other("end of a file still downloading")),
            },
        }
        .ok_or_else(|| std::io::Error::other("seek out of range"))?;
        self.pos = pos;
        if let Some(f) = &mut self.file {
            f.seek(SeekFrom::Start(pos))?;
        }
        Ok(pos)
    }
}
