//! Audio files: WAV (RIFF/WAVE) and AIFF/AIFC, uncompressed, and FLAC
//! (decoded with `claxon`; TIDAL's lossless format).
//!
//! [`AudioFile::open`] reads the header; [`AudioFile::read`] returns
//! interleaved **stereo i16** frames at the file's own sample rate: mono is
//! duplicated, 24/32-bit samples are cut to their top 16 bits, 8-bit
//! samples are widened.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Wav,
    Aiff,
    Flac,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Format {
    pub container: Container,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// Frames in the file.
    pub frames: u64,
    big_endian: bool,
    /// 8-bit WAV is unsigned, 8-bit AIFF signed.
    unsigned8: bool,
}

impl Format {
    fn bytes_per_sample(&self) -> usize {
        usize::from(self.bits).div_ceil(8)
    }

    fn bytes_per_frame(&self) -> usize {
        self.bytes_per_sample() * usize::from(self.channels)
    }

    pub fn duration_s(&self) -> f32 {
        self.frames as f32 / self.sample_rate as f32
    }
}

/// An open audio file, positioned at the next frame to read.
pub struct AudioFile<R: Read = BufReader<File>> {
    pub format: Format,
    body: Body<R>,
}

enum Body<R: Read> {
    Pcm {
        reader: R,
        /// Frames not read yet.
        remaining: u64,
        raw: Vec<u8>,
    },
    Flac {
        /// `None` only while rewinding.
        reader: Option<claxon::FlacReader<R>>,
        /// The last decoded block, channel after channel (`len` samples
        /// each), and the next frame in it.
        block: Vec<i32>,
        len: usize,
        pos: usize,
    },
}

fn flac_error(e: claxon::Error) -> io::Error {
    match e {
        claxon::Error::IoError(e) => e,
        e => io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// IEEE 754 80-bit extended (the AIFF sample rate field) → integer.
fn extended_to_u32(b: &[u8; 10]) -> u32 {
    let exp = (i32::from(b[0] & 0x7F) << 8 | i32::from(b[1])) - 16383;
    let mant = u64::from_be_bytes(b[2..10].try_into().unwrap());
    if !(0..=63).contains(&exp) {
        return 0;
    }
    (mant >> (63 - exp)) as u32
}

fn read_chunk_header<R: Read>(r: &mut R, big_endian: bool) -> io::Result<([u8; 4], u64)> {
    let mut h = [0u8; 8];
    r.read_exact(&mut h)?;
    let len = h[4..8].try_into().unwrap();
    let len = if big_endian {
        u32::from_be_bytes(len)
    } else {
        u32::from_le_bytes(len)
    };
    Ok((h[0..4].try_into().unwrap(), u64::from(len)))
}

impl AudioFile {
    /// Opens a file from the file system (32 KiB read buffer).
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_reader(BufReader::with_capacity(32 * 1024, File::open(path)?))
    }
}

impl<R: Read + Seek> AudioFile<R> {
    pub fn from_reader(mut reader: R) -> io::Result<Self> {
        let mut head = [0u8; 12];
        reader.read_exact(&mut head)?;
        if &head[0..4] == b"fLaC" {
            reader.seek(SeekFrom::Start(0))?;
            return Self::open_flac(reader);
        }
        let format = match (&head[0..4], &head[8..12]) {
            (b"RIFF", b"WAVE") => Self::parse_wav(&mut reader)?,
            (b"FORM", b"AIFF") => Self::parse_aiff(&mut reader, false)?,
            (b"FORM", b"AIFC") => Self::parse_aiff(&mut reader, true)?,
            _ => return Err(bad("not a WAV, AIFF or FLAC file")),
        };
        if format.channels == 0 || format.channels > 2 || !(8..=32).contains(&format.bits) {
            return Err(bad("unsupported channels or bit depth"));
        }
        let remaining = format.frames;
        Ok(Self {
            format,
            body: Body::Pcm {
                reader,
                remaining,
                raw: Vec::new(),
            },
        })
    }

    fn open_flac(reader: R) -> io::Result<Self> {
        let flac = claxon::FlacReader::new(reader).map_err(flac_error)?;
        let info = flac.streaminfo();
        let format = Format {
            container: Container::Flac,
            sample_rate: info.sample_rate,
            channels: info.channels as u16,
            bits: info.bits_per_sample as u16,
            frames: info.samples.unwrap_or(0),
            big_endian: false,
            unsigned8: false,
        };
        if format.channels == 0 || format.channels > 2 || !(8..=32).contains(&format.bits) {
            return Err(bad("unsupported channels or bit depth"));
        }
        Ok(Self {
            format,
            body: Body::Flac {
                reader: Some(flac),
                block: Vec::new(),
                len: 0,
                pos: 0,
            },
        })
    }

    /// Leaves the reader at the first sample.
    fn parse_wav(r: &mut R) -> io::Result<Format> {
        let mut fmt: Option<(u16, u32, u16)> = None;
        loop {
            let (id, len) = read_chunk_header(r, false)?;
            match &id {
                b"fmt " => {
                    if len < 16 {
                        return Err(bad("WAV fmt chunk too short"));
                    }
                    let mut f = vec![0u8; len as usize];
                    r.read_exact(&mut f)?;
                    let tag = u16::from_le_bytes([f[0], f[1]]);
                    // 1 = PCM, 0xFFFE = extensible (assumed PCM sub-format).
                    if tag != 1 && tag != 0xFFFE {
                        return Err(bad("WAV is not PCM"));
                    }
                    let channels = u16::from_le_bytes([f[2], f[3]]);
                    let rate = u32::from_le_bytes(f[4..8].try_into().unwrap());
                    let bits = u16::from_le_bytes([f[14], f[15]]);
                    fmt = Some((channels, rate, bits));
                    if len % 2 == 1 {
                        r.seek_relative(1)?;
                    }
                }
                b"data" => {
                    let (channels, sample_rate, bits) =
                        fmt.ok_or_else(|| bad("WAV data before fmt"))?;
                    let bpf = u64::from(bits).div_ceil(8) * u64::from(channels);
                    return Ok(Format {
                        container: Container::Wav,
                        sample_rate,
                        channels,
                        bits,
                        frames: len / bpf.max(1),
                        big_endian: false,
                        unsigned8: true,
                    });
                }
                _ => r.seek_relative((len + len % 2) as i64)?,
            }
        }
    }

    fn parse_aiff(r: &mut R, aifc: bool) -> io::Result<Format> {
        let mut comm: Option<(u16, u32, u16, u32, bool)> = None;
        loop {
            let (id, len) = read_chunk_header(r, true)?;
            match &id {
                b"COMM" => {
                    if len < 18 {
                        return Err(bad("AIFF COMM chunk too short"));
                    }
                    let mut c = vec![0u8; len as usize];
                    r.read_exact(&mut c)?;
                    let channels = u16::from_be_bytes([c[0], c[1]]);
                    let frames = u32::from_be_bytes(c[2..6].try_into().unwrap());
                    let bits = u16::from_be_bytes([c[6], c[7]]);
                    let rate = extended_to_u32(c[8..18].try_into().unwrap());
                    // AIFC: 'NONE'/'twos' = big-endian PCM, 'sowt' = little-endian.
                    let big_endian = if aifc && c.len() >= 22 {
                        match &c[18..22] {
                            b"NONE" | b"twos" => true,
                            b"sowt" => false,
                            _ => return Err(bad("compressed AIFC is not supported")),
                        }
                    } else {
                        true
                    };
                    comm = Some((channels, frames, bits, rate, big_endian));
                    if len % 2 == 1 {
                        r.seek_relative(1)?;
                    }
                }
                b"SSND" => {
                    let (channels, frames, bits, sample_rate, big_endian) =
                        comm.ok_or_else(|| bad("AIFF SSND before COMM"))?;
                    let mut ob = [0u8; 8];
                    r.read_exact(&mut ob)?;
                    let offset = u32::from_be_bytes(ob[0..4].try_into().unwrap());
                    r.seek_relative(i64::from(offset))?;
                    return Ok(Format {
                        container: Container::Aiff,
                        sample_rate,
                        channels,
                        bits,
                        frames: u64::from(frames),
                        big_endian,
                        unsigned8: false,
                    });
                }
                _ => r.seek_relative((len + len % 2) as i64)?,
            }
        }
    }

    /// Reads up to `out.len() / 2` stereo frames into `out`; returns the
    /// number of frames read (0 at the end).
    pub fn read(&mut self, out: &mut [i16]) -> io::Result<usize> {
        let f = &self.format;
        match &mut self.body {
            Body::Pcm {
                reader,
                remaining,
                raw,
            } => {
                let want = ((out.len() / 2) as u64).min(*remaining) as usize;
                if want == 0 {
                    return Ok(0);
                }
                let bps = f.bytes_per_sample();
                let bpf = f.bytes_per_frame();
                raw.resize(want * bpf, 0);
                reader.read_exact(raw)?;
                // The most significant 16 bits of one sample.
                let sample = |b: &[u8]| -> i16 {
                    if bps == 1 {
                        let v = if f.unsigned8 {
                            i16::from(b[0]) - 128
                        } else {
                            i16::from(b[0] as i8)
                        };
                        return v << 8;
                    }
                    if f.big_endian {
                        i16::from_be_bytes([b[0], b[1]])
                    } else {
                        i16::from_le_bytes([b[bps - 2], b[bps - 1]])
                    }
                };
                for (frame, dst) in raw.chunks_exact(bpf).zip(out.as_chunks_mut::<2>().0) {
                    let l = sample(&frame[..bps]);
                    let r = if f.channels == 2 {
                        sample(&frame[bps..2 * bps])
                    } else {
                        l
                    };
                    *dst = [l, r];
                }
                *remaining -= want as u64;
                Ok(want)
            }
            Body::Flac {
                reader,
                block,
                len,
                pos,
            } => {
                let bits = u32::from(f.bits);
                let to16 = |v: i32| -> i16 {
                    if bits >= 16 {
                        (v >> (bits - 16)) as i16
                    } else {
                        (v << (16 - bits)) as i16
                    }
                };
                let stereo = f.channels == 2;
                let mut done = 0;
                for dst in out.as_chunks_mut::<2>().0 {
                    if *pos == *len {
                        let Some(r) = reader.as_mut() else { break };
                        match r.blocks().read_next_or_eof(core::mem::take(block)) {
                            Ok(Some(b)) => {
                                *len = b.duration() as usize;
                                *pos = 0;
                                *block = b.into_buffer();
                                if *len == 0 {
                                    continue;
                                }
                            }
                            Ok(None) => break,
                            Err(e) => return Err(flac_error(e)),
                        }
                    }
                    let l = to16(block[*pos]);
                    let r = if stereo { to16(block[*len + *pos]) } else { l };
                    *dst = [l, r];
                    *pos += 1;
                    done += 1;
                }
                Ok(done)
            }
        }
    }

    /// Back to the first frame.
    pub fn rewind(&mut self) -> io::Result<()> {
        match &mut self.body {
            Body::Pcm {
                reader, remaining, ..
            } => {
                let read = (self.format.frames - *remaining) * self.format.bytes_per_frame() as u64;
                reader.seek(SeekFrom::Current(-(read as i64)))?;
                *remaining = self.format.frames;
            }
            Body::Flac {
                reader, len, pos, ..
            } => {
                let mut inner = reader
                    .take()
                    .ok_or_else(|| bad("FLAC reader lost"))?
                    .into_inner();
                inner.seek(SeekFrom::Start(0))?;
                *reader = Some(claxon::FlacReader::new(inner).map_err(flac_error)?);
                (*len, *pos) = (0, 0);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn wav(channels: u16, bits: u16, rate: u32, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data.len() as u32 + 12).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        // An unknown chunk first, odd length (padded), to be skipped.
        v.extend_from_slice(b"LIST");
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(&[1, 2, 3, 0]);
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        let block = channels * bits.div_ceil(8);
        v.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        v.extend_from_slice(&block.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    /// 44100 Hz as an 80-bit extended float.
    const RATE_44100: [u8; 10] = [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0];

    fn aiff(channels: u16, bits: u16, frames: u32, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"FORM");
        v.extend_from_slice(&(4 + 26 + 16 + data.len() as u32).to_be_bytes());
        v.extend_from_slice(b"AIFF");
        v.extend_from_slice(b"COMM");
        v.extend_from_slice(&18u32.to_be_bytes());
        v.extend_from_slice(&channels.to_be_bytes());
        v.extend_from_slice(&frames.to_be_bytes());
        v.extend_from_slice(&bits.to_be_bytes());
        v.extend_from_slice(&RATE_44100);
        v.extend_from_slice(b"SSND");
        v.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
        v.extend_from_slice(&[0; 8]); // offset, block size
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn extended_rates() {
        assert_eq!(extended_to_u32(&RATE_44100), 44_100);
        let r48 = [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0];
        assert_eq!(extended_to_u32(&r48), 48_000);
    }

    #[test]
    fn wav_16bit_stereo() {
        let data: Vec<u8> = [1i16, -2, 300, -400]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut f = AudioFile::from_reader(Cursor::new(wav(2, 16, 48_000, &data))).unwrap();
        assert_eq!(f.format.frames, 2);
        assert_eq!(f.format.sample_rate, 48_000);
        let mut out = [0i16; 8];
        assert_eq!(f.read(&mut out).unwrap(), 2);
        assert_eq!(&out[..4], &[1, -2, 300, -400]);
        assert_eq!(f.read(&mut out).unwrap(), 0);
        f.rewind().unwrap();
        assert_eq!(f.read(&mut out).unwrap(), 2);
        assert_eq!(&out[..4], &[1, -2, 300, -400]);
    }

    #[test]
    fn wav_8bit_mono_is_widened_and_duplicated() {
        let mut f = AudioFile::from_reader(Cursor::new(wav(1, 8, 8_000, &[128, 255, 0]))).unwrap();
        let mut out = [0i16; 6];
        assert_eq!(f.read(&mut out).unwrap(), 3);
        assert_eq!(out, [0, 0, 127 << 8, 127 << 8, -128 << 8, -128 << 8]);
    }

    #[test]
    fn wav_24bit_keeps_top_16_bits() {
        // 0x123456 and -1 (0xFFFFFF), little-endian.
        let mut f = AudioFile::from_reader(Cursor::new(wav(
            1,
            24,
            48_000,
            &[0x56, 0x34, 0x12, 0xFF, 0xFF, 0xFF],
        )))
        .unwrap();
        let mut out = [0i16; 4];
        f.read(&mut out).unwrap();
        assert_eq!(out, [0x1234, 0x1234, -1, -1]);
    }

    #[test]
    fn aiff_16bit_big_endian() {
        let data: Vec<u8> = [1000i16, -1000]
            .iter()
            .flat_map(|s| s.to_be_bytes())
            .collect();
        let mut f = AudioFile::from_reader(Cursor::new(aiff(2, 16, 1, &data))).unwrap();
        assert_eq!(f.format.container, Container::Aiff);
        assert_eq!(f.format.sample_rate, 44_100);
        let mut out = [0i16; 2];
        assert_eq!(f.read(&mut out).unwrap(), 1);
        assert_eq!(out, [1000, -1000]);
    }

    /// Decodes all of a file (read in odd-sized pieces), and once more
    /// after a rewind.
    fn decode_all(bytes: &'static [u8]) -> (Format, Vec<i16>) {
        let mut f = AudioFile::from_reader(Cursor::new(bytes)).unwrap();
        let mut all = Vec::new();
        let mut buf = [0i16; 2 * 333];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..2 * n]);
        }
        f.rewind().unwrap();
        let n = f.read(&mut buf).unwrap();
        assert_eq!(&buf[..2 * n], &all[..2 * n], "same after rewind");
        (f.format.clone(), all)
    }

    #[test]
    fn flac_decodes_like_its_wav() {
        // Pairs made with ffmpeg (sine) and the reference `flac` encoder.
        for (flac, wav) in [
            (
                include_bytes!("../testdata/stereo16.flac").as_slice(),
                include_bytes!("../testdata/stereo16.wav").as_slice(),
            ),
            (
                include_bytes!("../testdata/mono24.flac").as_slice(),
                include_bytes!("../testdata/mono24.wav").as_slice(),
            ),
        ] {
            let (ff, fs) = decode_all(flac);
            let (wf, ws) = decode_all(wav);
            assert_eq!(ff.container, Container::Flac);
            assert_eq!(
                (ff.sample_rate, ff.channels, ff.bits, ff.frames),
                (wf.sample_rate, wf.channels, wf.bits, wf.frames)
            );
            assert_eq!(fs.len() as u64, 2 * ff.frames);
            assert_eq!(fs, ws);
        }
    }

    #[test]
    fn rejects_other_files() {
        assert!(AudioFile::from_reader(Cursor::new(b"ID3\x04 something else".to_vec())).is_err());
    }
}
