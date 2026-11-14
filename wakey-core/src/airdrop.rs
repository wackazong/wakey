//! Files received by AirDrop: the C6's frames and what the P4 keeps.
//!
//! The C6 runs the AirDrop receiver (`firmware/c6/airdrop`) and sends each
//! file's bytes over a one-way UART as frames (`main/link.h` there):
//!
//! ```text
//! 0xA5 0x5A  type  length (u16 LE)  payload  CRC-32 (LE, over type, length, payload)
//! ```
//!
//! [`FrameReader`] finds them in the byte stream; [`Receiver`] turns them
//! into files through a [`Store`]: each file goes into a part file and is
//! renamed only when the whole transfer is known to be intact (the C6's
//! verdict comes after the last file). Files the clock cannot play, and the
//! AppleDouble companions (`._name`) iOS sends along, are skipped.

use std::fmt;

const MAGIC: [u8; 2] = [0xA5, 0x5A];
/// The largest payload the C6 sends (a data frame: offset + 1024 bytes),
/// with room to spare. A length above it is a false start.
const MAX_PAYLOAD: usize = 2048;

/// One frame from the C6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// The receiver started; its version.
    Hello(String),
    /// Once a second: `ready` once the AirDrop listener runs; AWDL peers.
    Status { ready: bool, peers: u8 },
    /// A file of `size` bytes starts.
    Begin { size: u32, name: String },
    /// Bytes of the current file from `offset`.
    Data { offset: u32, bytes: Vec<u8> },
    /// The current file ends; `complete` if all its bytes came.
    End { complete: bool },
    /// The transfer ends: `ok` if whole and intact.
    Done { ok: bool, why: String },
    /// A log line of the C6.
    Log(String),
}

/// Finds frames in the bytes from the UART. Bytes between frames (noise
/// at start, a damaged frame) are skipped; [`FrameReader::damaged`] counts
/// frames dropped for a bad checksum or length.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
    damaged: u32,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Frames dropped so far.
    pub fn damaged(&self) -> u32 {
        self.damaged
    }

    /// Adds bytes; returns the frames complete in them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut at = 0;
        loop {
            let Some(start) = find_magic(&self.buf[at..]).map(|i| at + i) else {
                // Keep a trailing 0xA5: it may start the next magic.
                at = if self.buf.last() == Some(&MAGIC[0]) { self.buf.len() - 1 } else { self.buf.len() };
                break;
            };
            let head = &self.buf[start..];
            if head.len() < 5 {
                at = start;
                break;
            }
            let len = u16::from_le_bytes([head[3], head[4]]) as usize;
            if len > MAX_PAYLOAD {
                self.damaged += 1;
                at = start + 1;
                continue;
            }
            if head.len() < 5 + len + 4 {
                at = start;
                break;
            }
            let body = &head[2..5 + len];
            let crc = u32::from_le_bytes(head[5 + len..9 + len].try_into().unwrap());
            match (crc32(body) == crc).then(|| parse(head[2], &head[5..5 + len])).flatten() {
                Some(frame) => {
                    frames.push(frame);
                    at = start + 9 + len;
                }
                None => {
                    self.damaged += 1;
                    at = start + 1;
                }
            }
        }
        self.buf.drain(..at);
        frames
    }
}

fn find_magic(b: &[u8]) -> Option<usize> {
    b.windows(2).position(|w| w == MAGIC)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn parse(kind: u8, p: &[u8]) -> Option<Frame> {
    let u32_at = |i: usize| p.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    Some(match kind {
        1 => Frame::Hello(text(p)),
        2 => Frame::Status { ready: *p.first()? != 0, peers: *p.get(1)? },
        3 => Frame::Begin { size: u32_at(0)?, name: text(&p[4..]) },
        4 => Frame::Data { offset: u32_at(0)?, bytes: p[4..].to_vec() },
        5 => Frame::End { complete: *p.first()? != 0 },
        6 => Frame::Done { ok: *p.first()? != 0, why: text(&p[1..]) },
        7 => Frame::Log(text(p)),
        _ => return None,
    })
}

/// CRC-32 (IEEE 802.3, as zlib's and the ESP ROM's `esp_rom_crc32_le`
/// with 0 as start).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Encodes a frame as the C6 does (for tests and tools).
pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = MAGIC.to_vec();
    f.push(kind);
    f.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    f.extend_from_slice(payload);
    let crc = crc32(&f[2..]);
    f.extend_from_slice(&crc.to_le_bytes());
    f
}

/// File types kept: what the clock plays (`wakey_core::audio_file`, FLAC,
/// and AAC/ALAC in .m4a, MP3).
pub const SOUND_EXTENSIONS: [&str; 8] = ["wav", "aif", "aiff", "aifc", "flac", "m4a", "mp3", "aac"];

/// The name to store a received file under, or `None` to skip it:
/// AppleDouble companions (`._x`), hidden files and types the clock cannot
/// play. Keeps the last path component in composed form (NFC: iOS sends
/// "ö" as "o" and a combining diaeresis, which the clock's fonts do not
/// draw as one letter), replaces characters FAT does not allow and control
/// characters, and shortens the stem to 100 bytes.
pub fn sound_name(received: &str) -> Option<String> {
    use unicode_normalization::UnicodeNormalization;
    let received: String = received.nfc().collect();
    let base = received.rsplit(['/', '\\']).next().unwrap_or(&received).trim();
    if base.starts_with('.') {
        return None;
    }
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let (stem, ext) = cleaned.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    if !SOUND_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let stem = stem.trim();
    let stem = if stem.is_empty() { "AirDrop" } else { stem };
    let mut cut = stem.len().min(100);
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(format!("{}.{ext}", stem[..cut].trim_end()))
}

/// `text` in composed form (NFC), for names shown.
pub fn nfc(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    text.nfc().collect()
}

/// `name`, or with " 2", " 3", … before the extension if `taken` says the
/// name is in use.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_owned();
    }
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    (2..)
        .map(|n| if ext.is_empty() { format!("{stem} {n}") } else { format!("{stem} {n}.{ext}") })
        .find(|candidate| !taken(candidate))
        .expect("some number is free")
}

/// Where received files go. Names are final names (as [`sound_name`]); the
/// store keeps a file as a part until [`Store::commit`].
pub trait Store {
    type File;
    /// True if a sound file of this name exists (or is being received).
    fn exists(&self, name: &str) -> bool;
    /// Starts the part file for `name`.
    fn create(&mut self, name: &str, size: u32) -> std::io::Result<Self::File>;
    fn write(&mut self, file: &mut Self::File, bytes: &[u8]) -> std::io::Result<()>;
    /// Closes the part file.
    fn close(&mut self, file: Self::File) -> std::io::Result<()>;
    /// Makes the closed part file `name` the sound file.
    fn commit(&mut self, name: &str) -> std::io::Result<()>;
    /// Deletes the part file of `name`.
    fn discard(&mut self, name: &str);
}

/// The file being received, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub name: String,
    pub got: u32,
    pub size: u32,
}

/// What a frame changed, for the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Nothing the UI shows.
    None,
    /// The receiver runs (`ready`) or not yet.
    Status { ready: bool },
    /// A file is being received (also on each data frame).
    Progress(Progress),
    /// A transfer ended; the files kept (their names), and why not all.
    Finished { kept: Vec<String>, error: Option<String> },
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} of {} bytes)", self.name, self.got, self.size)
    }
}

struct Current<F> {
    /// `None`: skipped (not a sound, or the part could not be created).
    file: Option<(String, F)>,
    progress: Progress,
}

/// Turns frames into files. A transfer is the files between its first
/// `Begin` and `Done`; its files are kept only if the C6 reports it whole
/// and intact and every byte arrived here in order.
pub struct Receiver<S: Store> {
    store: S,
    current: Option<Current<S::File>>,
    /// Closed part files of this transfer.
    done: Vec<String>,
    /// Set when something went wrong here (a gap, a write error).
    error: Option<String>,
}

impl<S: Store> Receiver<S> {
    pub fn new(store: S) -> Self {
        Self { store, current: None, done: Vec::new(), error: None }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// The file being received, if any.
    pub fn progress(&self) -> Option<&Progress> {
        self.current.as_ref().map(|c| &c.progress)
    }

    fn fail(&mut self, why: String) {
        if self.error.is_none() {
            self.error = Some(why);
        }
    }

    fn close_current(&mut self) {
        if let Some(cur) = self.current.take()
            && let Some((name, file)) = cur.file
        {
            match self.store.close(file) {
                Ok(()) => self.done.push(name),
                Err(e) => {
                    self.store.discard(&name);
                    self.fail(format!("{name}: {e}"));
                }
            }
        }
    }

    /// Drops what is unfinished (the C6 restarted or the link ended).
    pub fn abort(&mut self) {
        self.close_current();
        for name in self.done.drain(..) {
            self.store.discard(&name);
        }
        self.error = None;
    }

    pub fn frame(&mut self, frame: Frame) -> Event {
        match frame {
            Frame::Hello(_) => {
                self.abort();
                Event::Status { ready: false }
            }
            Frame::Status { ready, .. } => Event::Status { ready },
            Frame::Log(_) => Event::None,
            Frame::Begin { size, name } => {
                self.close_current();
                let file = match sound_name(&name) {
                    Some(n) => {
                        let n = unique_name(&n, |c| self.store.exists(c) || self.done.iter().any(|d| d == c));
                        match self.store.create(&n, size) {
                            Ok(f) => Some((n, f)),
                            Err(e) => {
                                self.fail(format!("{n}: {e}"));
                                None
                            }
                        }
                    }
                    None => None,
                };
                let shown = file.as_ref().map_or(name, |(n, _)| n.clone());
                let progress = Progress { name: shown, got: 0, size };
                let event = Event::Progress(progress.clone());
                self.current = Some(Current { file, progress });
                // Skipped files (companions, other types) are not shown.
                if self.current.as_ref().is_some_and(|c| c.file.is_some()) { event } else { Event::None }
            }
            Frame::Data { offset, bytes } => {
                let Some(cur) = self.current.as_mut() else {
                    self.fail("data outside a file".into());
                    return Event::None;
                };
                if offset != cur.progress.got {
                    let why = format!("{}: gap at {} (expected {})", cur.progress.name, offset, cur.progress.got);
                    self.fail(why);
                    return Event::None;
                }
                cur.progress.got += bytes.len() as u32;
                let Some((_, file)) = cur.file.as_mut() else { return Event::None };
                if let Err(e) = self.store.write(file, &bytes) {
                    let why = format!("{}: {e}", cur.progress.name);
                    let (name, file) = cur.file.take().unwrap();
                    let _ = self.store.close(file);
                    self.store.discard(&name);
                    self.fail(why);
                    return Event::None;
                }
                Event::Progress(cur.progress.clone())
            }
            Frame::End { complete } => {
                if let Some(cur) = &self.current
                    && cur.file.is_some()
                    && (!complete || cur.progress.got != cur.progress.size)
                {
                    let why = format!("{}: incomplete", cur.progress.name);
                    self.fail(why);
                }
                self.close_current();
                Event::None
            }
            Frame::Done { ok, why } => {
                self.close_current();
                let error = if ok { self.error.take() } else { Some(self.error.take().unwrap_or(why)) };
                let mut kept = Vec::new();
                for name in self.done.drain(..) {
                    if error.is_none() {
                        match self.store.commit(&name) {
                            Ok(()) => kept.push(name),
                            Err(_) => self.store.discard(&name),
                        }
                    } else {
                        self.store.discard(&name);
                    }
                }
                Event::Finished { kept, error }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn crc_matches_zlib() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn frames_survive_splits_noise_and_damage() {
        let mut stream = vec![0x00, 0xA5, 0x13]; // noise, a false start
        stream.extend(encode(1, b"1.0.0"));
        let mut bad = encode(7, b"damaged");
        bad[6] ^= 0x40;
        stream.extend(bad);
        stream.extend(encode(2, &[1, 3]));
        stream.extend(encode(4, &[0, 0, 0, 0, 9, 8, 7]));
        let mut r = FrameReader::new();
        let mut frames = Vec::new();
        for piece in stream.chunks(3) {
            frames.extend(r.push(piece));
        }
        assert_eq!(
            frames,
            vec![
                Frame::Hello("1.0.0".into()),
                Frame::Status { ready: true, peers: 3 },
                Frame::Data { offset: 0, bytes: vec![9, 8, 7] },
            ]
        );
        assert_eq!(r.damaged(), 1);
    }

    #[test]
    fn names() {
        assert_eq!(sound_name("Tintenfischsong.m4a").as_deref(), Some("Tintenfischsong.m4a"));
        assert_eq!(sound_name("._Tintenfischsong.m4a"), None);
        assert_eq!(sound_name("Foto.HEIC"), None);
        assert_eq!(sound_name("dir/Weck: mich?.MP3").as_deref(), Some("Weck_ mich_.mp3"));
        assert_eq!(sound_name("noext"), None);
        // NFD from iOS → NFC.
        assert_eq!(sound_name("Schneeglo\u{308}ckchen.m4a").as_deref(), Some("Schneegl\u{f6}ckchen.m4a"));
        let long = format!("{}.wav", "ä".repeat(80));
        let n = sound_name(&long).unwrap();
        assert!(n.len() <= 104 && n.ends_with(".wav"), "{n}");
        assert_eq!(unique_name("a.m4a", |n| n == "a.m4a" || n == "a 2.m4a"), "a 3.m4a");
    }

    /// Part files and sound files in memory.
    #[derive(Default)]
    struct Mem {
        parts: BTreeMap<String, Vec<u8>>,
        files: BTreeMap<String, Vec<u8>>,
    }

    impl Store for Mem {
        type File = String;
        fn exists(&self, name: &str) -> bool {
            self.files.contains_key(name) || self.parts.contains_key(name)
        }
        fn create(&mut self, name: &str, _size: u32) -> std::io::Result<String> {
            self.parts.insert(name.into(), Vec::new());
            Ok(name.into())
        }
        fn write(&mut self, file: &mut String, bytes: &[u8]) -> std::io::Result<()> {
            self.parts.get_mut(file).unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn close(&mut self, _file: String) -> std::io::Result<()> {
            Ok(())
        }
        fn commit(&mut self, name: &str) -> std::io::Result<()> {
            let data = self.parts.remove(name).unwrap();
            self.files.insert(name.into(), data);
            Ok(())
        }
        fn discard(&mut self, name: &str) {
            self.parts.remove(name);
        }
    }

    fn data(offset: u32, bytes: &[u8]) -> Frame {
        Frame::Data { offset, bytes: bytes.to_vec() }
    }

    fn transfer(rx: &mut Receiver<Mem>, ok: bool) -> Event {
        rx.frame(Frame::Begin { size: 4, name: "a.m4a".into() });
        rx.frame(data(0, b"ab"));
        rx.frame(data(2, b"cd"));
        rx.frame(Frame::End { complete: true });
        rx.frame(Frame::Begin { size: 2, name: "._a.m4a".into() });
        rx.frame(data(0, b"xx"));
        rx.frame(Frame::End { complete: true });
        rx.frame(Frame::Done { ok, why: if ok { "ok".into() } else { "bad record".into() } })
    }

    #[test]
    fn a_whole_transfer_is_kept_and_companions_skipped() {
        let mut rx = Receiver::new(Mem::default());
        let ev = transfer(&mut rx, true);
        assert_eq!(ev, Event::Finished { kept: vec!["a.m4a".into()], error: None });
        assert_eq!(rx.store().files.get("a.m4a").unwrap(), b"abcd");
        assert!(rx.store().parts.is_empty());
        // The same name again: kept beside the first.
        let ev = transfer(&mut rx, true);
        assert_eq!(ev, Event::Finished { kept: vec!["a 2.m4a".into()], error: None });
    }

    #[test]
    fn a_failed_or_gappy_transfer_keeps_nothing() {
        let mut rx = Receiver::new(Mem::default());
        let ev = transfer(&mut rx, false);
        assert_eq!(ev, Event::Finished { kept: vec![], error: Some("bad record".into()) });
        assert!(rx.store().files.is_empty() && rx.store().parts.is_empty());

        rx.frame(Frame::Begin { size: 4, name: "b.wav".into() });
        rx.frame(data(0, b"ab"));
        rx.frame(data(3, b"d")); // a frame was lost
        rx.frame(Frame::End { complete: true });
        let Event::Finished { kept, error } = rx.frame(Frame::Done { ok: true, why: "ok".into() }) else {
            panic!()
        };
        assert!(kept.is_empty() && error.unwrap().contains("gap"));
        assert!(rx.store().parts.is_empty());
    }

    #[test]
    fn a_restart_of_the_c6_drops_the_unfinished() {
        let mut rx = Receiver::new(Mem::default());
        rx.frame(Frame::Begin { size: 4, name: "c.flac".into() });
        rx.frame(data(0, b"ab"));
        assert_eq!(rx.progress().map(|p| p.got), Some(2));
        rx.frame(Frame::Hello("1.0.0".into()));
        assert!(rx.progress().is_none() && rx.store().parts.is_empty());
    }
}
