//! PCM audio files: WAV (RIFF/WAVE) and AIFF/AIFC, uncompressed.
//!
//! [`AudioFile::open`] reads the header; [`AudioFile::read`] returns
//! interleaved **stereo i16** frames at the file's own sample rate (mono is
//! duplicated, 24/32-bit is truncated to 16 bit, 8-bit is widened).

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Wav,
    Aiff,
}

#[derive(Debug, Clone)]
pub struct Format {
    pub container: Container,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// Frames in the file.
    pub frames: u64,
    big_endian: bool,
    /// 8-bit WAV is unsigned.
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

pub struct AudioFile {
    pub format: Format,
    reader: BufReader<File>,
    /// Frames not read yet.
    remaining: u64,
    raw: Vec<u8>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// IEEE 754 80-bit extended (AIFF sample rate) → u32.
fn extended_to_u32(b: &[u8; 10]) -> u32 {
    let exp = (i32::from(b[0] & 0x7F) << 8 | i32::from(b[1])) - 16383;
    let mant = u64::from_be_bytes(b[2..10].try_into().unwrap());
    if !(0..=63).contains(&exp) {
        return 0;
    }
    (mant >> (63 - exp)) as u32
}

impl AudioFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut reader = BufReader::with_capacity(32 * 1024, File::open(path)?);
        let mut head = [0u8; 12];
        reader.read_exact(&mut head)?;
        let format = match (&head[0..4], &head[8..12]) {
            (b"RIFF", b"WAVE") => Self::parse_wav(&mut reader)?,
            (b"FORM", b"AIFF") => Self::parse_aiff(&mut reader, false)?,
            (b"FORM", b"AIFC") => Self::parse_aiff(&mut reader, true)?,
            _ => return Err(bad("not a WAV or AIFF file")),
        };
        if format.channels == 0 || format.channels > 2 || !(8..=32).contains(&format.bits) {
            return Err(bad("unsupported channels or bit depth"));
        }
        let remaining = format.frames;
        Ok(Self {
            format,
            reader,
            remaining,
            raw: Vec::new(),
        })
    }

    /// Leaves the reader at the first sample.
    fn parse_wav(r: &mut BufReader<File>) -> io::Result<Format> {
        let mut fmt: Option<(u16, u16, u32, u16)> = None;
        loop {
            let mut h = [0u8; 8];
            r.read_exact(&mut h)?;
            let len = u32::from_le_bytes(h[4..8].try_into().unwrap()) as u64;
            match &h[0..4] {
                b"fmt " => {
                    let mut f = vec![0u8; len as usize];
                    r.read_exact(&mut f)?;
                    let tag = u16::from_le_bytes([f[0], f[1]]);
                    let channels = u16::from_le_bytes([f[2], f[3]]);
                    let rate = u32::from_le_bytes(f[4..8].try_into().unwrap());
                    let bits = u16::from_le_bytes([f[14], f[15]]);
                    // 1 = PCM, 0xFFFE = extensible (assume PCM sub-format).
                    if tag != 1 && tag != 0xFFFE {
                        return Err(bad("WAV is not PCM"));
                    }
                    fmt = Some((tag, channels, rate, bits));
                    if len % 2 == 1 {
                        r.seek_relative(1)?;
                    }
                }
                b"data" => {
                    let (_, channels, sample_rate, bits) =
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

    fn parse_aiff(r: &mut BufReader<File>, aifc: bool) -> io::Result<Format> {
        let mut comm: Option<(u16, u32, u16, u32, bool)> = None;
        loop {
            let mut h = [0u8; 8];
            r.read_exact(&mut h)?;
            let len = u32::from_be_bytes(h[4..8].try_into().unwrap()) as u64;
            match &h[0..4] {
                b"COMM" => {
                    let mut c = vec![0u8; len as usize];
                    r.read_exact(&mut c)?;
                    let channels = u16::from_be_bytes([c[0], c[1]]);
                    let frames = u32::from_be_bytes(c[2..6].try_into().unwrap());
                    let bits = u16::from_be_bytes([c[6], c[7]]);
                    let rate = extended_to_u32(c[8..18].try_into().unwrap());
                    // AIFC: 'NONE' = big-endian PCM, 'sowt' = little-endian PCM.
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
        let want = ((out.len() / 2) as u64).min(self.remaining) as usize;
        if want == 0 {
            return Ok(0);
        }
        let bps = f.bytes_per_sample();
        let bpf = f.bytes_per_frame();
        self.raw.resize(want * bpf, 0);
        self.reader.read_exact(&mut self.raw)?;
        let sample = |b: &[u8]| -> i16 {
            // Most significant 16 bits of each sample.
            let (hi, lo) = if f.big_endian {
                (b[0], b.get(1).copied().unwrap_or(0))
            } else {
                (b[bps - 1], if bps >= 2 { b[bps - 2] } else { 0 })
            };
            if bps == 1 {
                let v = if f.unsigned8 {
                    b[0] as i16 - 128
                } else {
                    b[0] as i8 as i16
                };
                v << 8
            } else {
                i16::from_be_bytes([hi, lo])
            }
        };
        for (i, frame) in self.raw.chunks_exact(bpf).enumerate() {
            let l = sample(&frame[..bps]);
            let r = if f.channels == 2 {
                sample(&frame[bps..2 * bps])
            } else {
                l
            };
            out[2 * i] = l;
            out[2 * i + 1] = r;
        }
        self.remaining -= want as u64;
        Ok(want)
    }

    /// Back to the first frame.
    pub fn rewind(&mut self) -> io::Result<()> {
        let skip = (self.format.frames - self.remaining) * self.format.bytes_per_frame() as u64;
        self.reader.seek(SeekFrom::Current(-(skip as i64)))?;
        self.remaining = self.format.frames;
        Ok(())
    }
}
