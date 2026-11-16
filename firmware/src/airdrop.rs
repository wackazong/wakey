//! AirDrop: the C6 receives sound files from iPhones while the sound page's
//! Files tab is shown ([`set_wanted`]).
//!
//! The C6 runs either esp_hosted (Wi-Fi) or the AirDrop receiver: two
//! images in its two app slots ([`crate::c6`]). On the `wifi` thread,
//! [`run`] has the C6 boot the receiver (installing what is missing
//! first), then reads what it receives. The receiver makes esp_hosted the
//! next boot again, so powering the C6 off ends AirDrop.
//!
//! Received files come as frames on the C6's IO2 → GPIO14 (UART, one way;
//! `wakey_core::airdrop`) and go to `sounds/` as part files, renamed once
//! the transfer is known to be whole.

use crate::board::{C6Link, sd};
use crate::c6::C6;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::uart::{UartRxDriver, config::Config as UartConfig};
use esp_idf_svc::hal::units::Hertz;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use wakey_core::airdrop::{Event, Frame, FrameReader, Progress, Receiver, Store};

/// The link's speed (`c6/airdrop/main/link.h`).
const BAUD: u32 = 3_000_000;
/// The UART driver's receive buffer: what arrives while a card write
/// holds this thread (at most ~300 KB/s).
const RX_BUFFER: usize = 64 * 1024;
/// A receiver that sends nothing (not even its status, once a second) for
/// this long has stopped: the C6 is powered off and on, and started again.
const SILENCE: Duration = Duration::from_secs(5);

/// What AirDrop is doing, for the sound page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Off,
    /// The C6 is being switched (or the receiver installed).
    Starting,
    /// "Wakey" is in the iPhones' AirDrop list.
    Ready,
    Receiving(Progress),
    /// It did not come up (details in the log); not again until wanted
    /// anew.
    Failed,
}

static WANTED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State::Off);
/// Bumped when a file was kept (the list reads the card again).
static FILES: AtomicU32 = AtomicU32::new(0);

/// AirDrop on (the Files tab shows) or off. The `wifi` thread follows: a
/// Wi-Fi session under way ends first (TIDAL downloads stop and resume
/// later).
pub fn set_wanted(on: bool) {
    if WANTED.swap(on, Ordering::Relaxed) != on {
        log::info!(
            "airdrop: {}",
            if on { "wanted" } else { "no longer wanted" }
        );
    }
}

pub fn wanted() -> bool {
    WANTED.load(Ordering::Relaxed)
}

pub fn state() -> State {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Changes when received files were added to `sounds/`.
pub fn files_version() -> u32 {
    FILES.load(Ordering::Relaxed)
}

fn set_state(s: State) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = s;
}

/// On the `wifi` thread, while [`wanted`]: the C6 as receiver, files to
/// the card. Returns with the C6 off.
pub(crate) fn run(link: &mut C6Link, c6: &C6) {
    set_state(State::Starting);
    let mut failures = 0;
    while wanted() {
        let result = c6.boot_receiver(&wanted).and_then(|()| receive(link));
        match result {
            Ok(()) => failures = 0,
            Err(e) => {
                log::warn!("airdrop: {e}");
                failures += 1;
                if failures >= 3 {
                    set_state(State::Failed);
                    // Not again until wanted anew.
                    while wanted() {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        }
        c6.off();
        std::thread::sleep(Duration::from_millis(300));
    }
    set_state(State::Off);
}

/// Reads the receiver's frames until AirDrop is no longer wanted. An error
/// if the receiver goes silent.
fn receive(link: &mut C6Link) -> anyhow::Result<()> {
    let config = UartConfig::new()
        .baudrate(Hertz(BAUD))
        .rx_fifo_size(RX_BUFFER);
    // SAFETY: the reborrowed UART and pin live only as long as this driver,
    // dropped before the next call reborrows them.
    let uart = UartRxDriver::new(
        unsafe { link.uart.reborrow() },
        unsafe { link.rx.reborrow() },
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &config,
    )?;
    let mut frames = FrameReader::new();
    let mut receiver = Receiver::new(SdStore::new());
    let mut buf = vec![0u8; 4096];
    let mut last_frame = Instant::now();
    let mut hello = false;
    let result = loop {
        if !wanted() {
            break Ok(());
        }
        let n = uart.read(&mut buf, 10).unwrap_or(0);
        for frame in frames.push(&buf[..n]) {
            last_frame = Instant::now();
            match &frame {
                Frame::Hello(v) => {
                    hello = true;
                    log::info!("airdrop: receiver {v} started");
                }
                Frame::Log(line) => log::info!("c6: {line}"),
                Frame::Done { ok, why } => log::info!(
                    "airdrop: transfer {} ({why})",
                    if *ok { "ok" } else { "failed" }
                ),
                _ => {}
            }
            match receiver.frame(frame) {
                Event::None => {}
                Event::Status { ready } => {
                    if receiver.progress().is_none() {
                        set_state(if ready { State::Ready } else { State::Starting });
                    }
                }
                Event::Progress(p) => set_state(State::Receiving(p)),
                Event::Finished { kept, error } => {
                    if let Some(e) = &error {
                        log::warn!("airdrop: nothing kept: {e}");
                    }
                    if !kept.is_empty() {
                        log::info!("airdrop: kept {kept:?}");
                        FILES.fetch_add(1, Ordering::Relaxed);
                    }
                    set_state(State::Ready);
                }
            }
        }
        let silence = if hello { SILENCE } else { SILENCE * 2 };
        if last_frame.elapsed() > silence {
            break Err(anyhow::anyhow!("the receiver is silent"));
        }
    };
    receiver.abort();
    if frames.damaged() > 0 {
        log::warn!("airdrop: {} damaged frames", frames.damaged());
    }
    result
}

/// Received files in `sounds/` on the card: `<name>.part` while they come.
struct SdStore {
    dir: PathBuf,
}

impl SdStore {
    fn new() -> Self {
        let dir = sd::path(crate::sounds::DIR);
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn part(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.part"))
    }
}

impl Store for SdStore {
    type File = std::io::BufWriter<std::fs::File>;

    fn exists(&self, name: &str) -> bool {
        self.dir.join(name).exists() || self.part(name).exists()
    }

    fn create(&mut self, name: &str, _size: u32) -> std::io::Result<Self::File> {
        Ok(std::io::BufWriter::with_capacity(
            32 * 1024,
            std::fs::File::create(self.part(name))?,
        ))
    }

    fn write(&mut self, file: &mut Self::File, bytes: &[u8]) -> std::io::Result<()> {
        file.write_all(bytes)
    }

    fn close(&mut self, mut file: Self::File) -> std::io::Result<()> {
        file.flush()?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all()
    }

    fn commit(&mut self, name: &str) -> std::io::Result<()> {
        std::fs::rename(self.part(name), self.dir.join(name))
    }

    fn discard(&mut self, name: &str) {
        let _ = std::fs::remove_file(self.part(name));
    }
}
