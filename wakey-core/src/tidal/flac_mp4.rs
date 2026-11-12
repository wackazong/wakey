//! FLAC in fragmented MP4 (TIDAL's DASH lossless) → a plain `.flac`, while
//! it downloads.
//!
//! In MP4 every sample of a FLAC track is one complete FLAC frame, and the
//! `dfLa` box in the sample entry holds the stream's metadata blocks
//! (STREAMINFO first) exactly as in a `.flac` file. So the file is
//! `fLaC` + those blocks + the payload of every `mdat` in order; the `moof`
//! boxes (sample tables) are not needed. The segments are fed in as they
//! arrive ([`Unpacker::push`]), in pieces of any size.

use super::{Result, ensure, err};

/// Largest box kept in memory (`moov`; TIDAL's are a few KiB).
const MAX_BUFFERED: u64 = 1 << 20;

enum State {
    /// Collecting a box header (8 or 16 bytes) in `pending`.
    Header,
    /// Keeping a whole box (`moov`) to read it.
    Keep { remaining: u64, buf: Vec<u8> },
    /// Skipping a box's payload.
    Skip { remaining: u64 },
    /// Copying an `mdat` payload out (`None`: up to the end).
    Media { remaining: Option<u64> },
}

/// Unpacks a fragmented MP4 with one FLAC track into FLAC bytes.
pub struct Unpacker {
    state: State,
    pending: Vec<u8>,
    header_written: bool,
    media_bytes: u64,
}

impl Default for Unpacker {
    fn default() -> Self {
        Self::new()
    }
}

impl Unpacker {
    pub fn new() -> Self {
        Self {
            state: State::Header,
            pending: Vec::new(),
            header_written: false,
            media_bytes: 0,
        }
    }

    /// Feeds the next bytes of the MP4; appends the FLAC bytes they yield
    /// to `out`.
    pub fn push(&mut self, mut data: &[u8], out: &mut Vec<u8>) -> Result<()> {
        while !data.is_empty() {
            match &mut self.state {
                State::Header => {
                    let need = if self.pending.len() >= 8 && box_size(&self.pending) == 1 {
                        16
                    } else {
                        8
                    };
                    let take = (need - self.pending.len()).min(data.len());
                    self.pending.extend_from_slice(&data[..take]);
                    data = &data[take..];
                    if self.pending.len() < need {
                        continue;
                    }
                    if need == 8 && box_size(&self.pending) == 1 {
                        continue; // 64-bit size follows
                    }
                    self.start_box(out)?;
                }
                State::Keep { remaining, buf } => {
                    let take = (*remaining).min(data.len() as u64) as usize;
                    buf.extend_from_slice(&data[..take]);
                    *remaining -= take as u64;
                    data = &data[take..];
                    if *remaining == 0 {
                        let moov = core::mem::take(buf);
                        self.state = State::Header;
                        self.write_header(&moov, out)?;
                    }
                }
                State::Skip { remaining } => {
                    let take = (*remaining).min(data.len() as u64);
                    *remaining -= take;
                    data = &data[take as usize..];
                    if *remaining == 0 {
                        self.state = State::Header;
                    }
                }
                State::Media { remaining } => {
                    let take = match remaining {
                        Some(r) => (*r).min(data.len() as u64) as usize,
                        None => data.len(),
                    };
                    out.extend_from_slice(&data[..take]);
                    self.media_bytes += take as u64;
                    data = &data[take..];
                    if let Some(r) = remaining {
                        *r -= take as u64;
                        if *r == 0 {
                            self.state = State::Header;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Checks that the MP4 ended where a box ended and held FLAC frames.
    pub fn finish(&self) -> Result<()> {
        let at_boundary = match &self.state {
            State::Header => self.pending.is_empty(),
            State::Media { remaining: None } => true,
            _ => false,
        };
        ensure!(at_boundary, "MP4: cut off");
        ensure!(self.header_written, "MP4: no FLAC track");
        ensure!(self.media_bytes > 0, "MP4: no audio");
        Ok(())
    }

    fn start_box(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let h = core::mem::take(&mut self.pending);
        let kind: [u8; 4] = h[4..8].try_into().unwrap();
        let (header_len, size) = match box_size(&h) {
            0 => (8, None),
            1 => (16, Some(u64::from_be_bytes(h[8..16].try_into().unwrap()))),
            n => (8, Some(u64::from(n))),
        };
        let payload = match size {
            Some(s) => {
                ensure!(s >= header_len, "MP4: invalid box size");
                Some(s - header_len)
            }
            None => None,
        };
        self.state = match (&kind, payload) {
            (b"mdat", remaining) => {
                ensure!(self.header_written, "MP4: audio before the FLAC header");
                match remaining {
                    Some(0) => State::Header,
                    remaining => State::Media { remaining },
                }
            }
            (_, None) => return Err(err("MP4: box without size")),
            (b"moov", Some(n)) => {
                ensure!(n <= MAX_BUFFERED, "MP4: header too large");
                if n == 0 {
                    self.write_header(&[], out)?;
                    State::Header
                } else {
                    State::Keep {
                        remaining: n,
                        buf: Vec::with_capacity(n as usize),
                    }
                }
            }
            (_, Some(0)) => State::Header,
            (_, Some(n)) => State::Skip { remaining: n },
        };
        Ok(())
    }

    /// `fLaC` and the metadata blocks from the first FLAC sample entry.
    fn write_header(&mut self, moov: &[u8], out: &mut Vec<u8>) -> Result<()> {
        ensure!(!self.header_written, "MP4: two headers");
        let dfla = find_dfla(moov).ok_or_else(|| err("MP4: no FLAC track"))?;
        // dfLa: version and flags, then the metadata blocks.
        ensure!(dfla.len() > 4, "MP4: empty FLAC header");
        let blocks = &dfla[4..];
        let mut at = 0;
        let mut header = b"fLaC".to_vec();
        let mut first = true;
        while at < blocks.len() {
            ensure!(at + 4 <= blocks.len(), "MP4: broken FLAC header");
            let kind = blocks[at] & 0x7F;
            let len =
                u32::from_be_bytes([0, blocks[at + 1], blocks[at + 2], blocks[at + 3]]) as usize;
            ensure!(at + 4 + len <= blocks.len(), "MP4: broken FLAC header");
            ensure!(!first || (kind == 0 && len == 34), "MP4: no STREAMINFO");
            let last = at + 4 + len == blocks.len();
            header.push(kind | if last { 0x80 } else { 0 });
            header.extend_from_slice(&blocks[at + 1..at + 4 + len]);
            at += 4 + len;
            first = false;
        }
        ensure!(!first, "MP4: empty FLAC header");
        out.extend_from_slice(&header);
        self.header_written = true;
        Ok(())
    }
}

fn box_size(h: &[u8]) -> u32 {
    u32::from_be_bytes(h[0..4].try_into().unwrap())
}

/// The boxes in `data`: (type, payload).
fn boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    core::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let size = box_size(data) as usize;
        let kind: [u8; 4] = data[4..8].try_into().unwrap();
        let (start, end) = match size {
            0 => (8, data.len()),
            1 => {
                let big = u64::from_be_bytes(data.get(8..16)?.try_into().ok()?);
                (16, usize::try_from(big).ok()?)
            }
            n => (8, n),
        };
        if end < start || end > data.len() {
            return None;
        }
        let payload = &data[start..end];
        data = &data[end..];
        Some((kind, payload))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> impl Iterator<Item = &'a [u8]> {
    boxes(data).filter(move |(k, _)| k == kind).map(|(_, p)| p)
}

/// The `dfLa` payload of the first FLAC sample entry:
/// moov/trak/mdia/minf/stbl/stsd/fLaC/dfLa.
fn find_dfla(moov: &[u8]) -> Option<&[u8]> {
    for trak in child(moov, b"trak") {
        for mdia in child(trak, b"mdia") {
            for minf in child(mdia, b"minf") {
                for stbl in child(minf, b"stbl") {
                    for stsd in child(stbl, b"stsd") {
                        // Version, flags, entry count.
                        let entries = stsd.get(8..)?;
                        for flac in child(entries, b"fLaC") {
                            // Audio sample entry fields, then child boxes.
                            if let Some(d) = child(flac.get(28..)?, b"dfLa").next() {
                                return Some(d);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_file::AudioFile;
    use std::io::Cursor;

    fn decode(flac: Vec<u8>) -> Vec<i16> {
        let mut f = AudioFile::from_reader(Cursor::new(flac)).unwrap();
        let mut all = Vec::new();
        let mut buf = [0i16; 512];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                return all;
            }
            all.extend_from_slice(&buf[..2 * n]);
        }
    }

    #[test]
    fn unpacks_ffmpeg_fragmented_mp4_in_any_pieces() {
        // ffmpeg -i stereo16.flac -c:a copy -f mp4 -movflags
        // frag_keyframe+empty_moov+default_base_moof -frag_duration 50000
        let mp4 = include_bytes!("../../testdata/stereo16_frag.mp4");
        let wav = include_bytes!("../../testdata/stereo16.wav");
        let want = decode_wav(wav);
        for piece in [1, 7, 100, 4096, mp4.len()] {
            let mut u = Unpacker::new();
            let mut flac = Vec::new();
            for chunk in mp4.chunks(piece) {
                u.push(chunk, &mut flac).unwrap();
            }
            u.finish().unwrap();
            assert_eq!(&flac[..4], b"fLaC");
            assert_eq!(decode(flac), want, "pieces of {piece}");
        }
    }

    fn decode_wav(bytes: &[u8]) -> Vec<i16> {
        let mut f = AudioFile::from_reader(Cursor::new(bytes.to_vec())).unwrap();
        let mut all = Vec::new();
        let mut buf = [0i16; 512];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                return all;
            }
            all.extend_from_slice(&buf[..2 * n]);
        }
    }

    #[test]
    fn rejects_cut_off_and_headerless() {
        let mp4 = include_bytes!("../../testdata/stereo16_frag.mp4");
        let mut u = Unpacker::new();
        let mut out = Vec::new();
        u.push(&mp4[..mp4.len() - 300], &mut out).unwrap();
        assert!(u.finish().is_err());
        // An mdat first.
        let mut u = Unpacker::new();
        let bad = [0, 0, 0, 9, b'm', b'd', b'a', b't', 0];
        assert!(u.push(&bad, &mut out).is_err());
    }
}
