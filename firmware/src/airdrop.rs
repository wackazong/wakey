//! AirDrop: the C6 receives sound files from iPhones while the sound page's
//! Files tab is shown ([`set_wanted`]).
//!
//! The C6 runs either esp_hosted (Wi-Fi) or the AirDrop receiver: two
//! images in its two app slots (`c6/README.md`). On the `wifi` thread,
//! [`run`] switches it: esp_hosted up, our addition to it asked over its
//! peer-data channel what the other slot holds (the receiver, and our
//! esp_hosted itself, are installed by OTA when missing or outdated), then
//! told to boot the receiver. The receiver makes esp_hosted the next boot
//! again, so powering the C6 off ends AirDrop.
//!
//! Received files come as frames on the C6's IO2 → GPIO14 (UART, one way;
//! `wakey_core::airdrop`) and go to `sounds/` as part files, renamed once
//! the transfer is known to be whole.

use crate::board::ioexp::Switch;
use crate::board::sd;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::uart::{UartRxDriver, config::Config as UartConfig};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys;
use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use wakey_core::airdrop::{Event, Frame, FrameReader, Progress, Receiver, Store};

/// The C6's images in the flash partition "c6" (`partitions.csv`, written
/// by `scripts/flash.sh` from `c6/pack.py`): esp_hosted with Wakey's
/// addition (`c6/hosted`) at 0, the receiver (`c6/airdrop`) at 2 MiB, each
/// after "WKC6" and its length. Not built into the app: the app's
/// constants are copied into PSRAM at boot, and 2.2 MB there left too
/// little for esp_hosted's buffers.
#[derive(Clone, Copy)]
enum C6Image {
    Hosted = 0,
    Receiver = 0x20_0000,
}
const RECEIVER_PROJECT: &str = "wakey_airdrop_c6";
/// The esp_hosted version our addition is built on (`net::C6_FIRMWARE`'s
/// major and minor): anything older is a stock esp_hosted.
const HOSTED_MIN: [u32; 3] = [2, 13, 0];
/// The first esp_hosted slave with the OTA activate RPC.
const ACTIVATE_MIN: [u32; 3] = [2, 6, 0];

/// The esp_hosted version the C6 reports (an RPC; the transport is up), or
/// `None` if it does not answer.
fn c6_version() -> Option<[u32; 3]> {
    let mut v = sys::esp_hosted_coprocessor_fwver_t::default();
    // SAFETY: `v` is a plain out-parameter, valid for the call.
    let err = unsafe { sys::esp_hosted_get_coprocessor_fwversion(&mut v) };
    (err == 0).then_some([v.major1, v.minor1, v.patch1])
}

/// The partition, and the image's offset and length in it.
fn image(which: C6Image) -> anyhow::Result<(*const sys::esp_partition_t, usize, usize)> {
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
    read_part(part, at, &mut head)?;
    anyhow::ensure!(
        &head[..4] == b"WKC6",
        "the C6 images are missing (scripts/flash.sh)"
    );
    let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
    Ok((part, at + 8, len))
}

fn read_part(part: *const sys::esp_partition_t, at: usize, buf: &mut [u8]) -> anyhow::Result<()> {
    // SAFETY: `buf` is valid for `buf.len()` bytes; the partition API
    // checks the range.
    let err = unsafe { sys::esp_partition_read(part, at, buf.as_mut_ptr().cast(), buf.len()) };
    anyhow::ensure!(err == 0, "reading the c6 partition at {at}: {err}");
    Ok(())
}

/// The link's speed (`c6/airdrop/main/link.h`).
const BAUD: u32 = 3_000_000;
/// The UART driver's receive buffer: what arrives while a card write
/// holds this thread (at most ~300 KB/s).
const RX_BUFFER: usize = 64 * 1024;
/// A receiver that sends nothing (not even its status, once a second) for
/// this long has stopped: the C6 is powered off and on, and started again.
const SILENCE: Duration = Duration::from_secs(5);

/// Peer-data messages to and from our esp_hosted (`c6/hosted/wakey_switch.c`).
const WK_QUERY: u32 = 0x574B_0001;
const WK_BOOT_AIRDROP: u32 = 0x574B_0002;
const WK_INFO: u32 = 0x574B_8001;
const WK_RESULT: u32 = 0x574B_8002;

/// What AirDrop is doing, for the sound page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Off,
    /// The C6 is being switched (or the receiver installed).
    Starting,
    /// "Wakey" is in the iPhones' AirDrop list.
    Ready,
    Receiving(Progress),
    Failed(String),
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

/// The UART and the pin the receiver's frames arrive on.
pub struct Link {
    pub uart: esp_idf_svc::hal::uart::UART1<'static>,
    pub rx: esp_idf_svc::hal::gpio::Gpio14<'static>,
}

/// On the `wifi` thread, while [`wanted`]: the C6 as receiver, files to
/// the card. Returns with the C6 off. `stop_transport` ends esp_hosted's
/// transport (as after a Wi-Fi session, `net::end_session` without the
/// power).
pub(crate) fn run(link: &mut Link, power: &Switch, stop_transport: &dyn Fn()) {
    set_state(State::Starting);
    let mut failures = 0;
    while wanted() {
        let result = prepare(power, stop_transport, true).and_then(|()| receive(link));
        match result {
            Ok(()) => failures = 0,
            Err(e) => {
                log::warn!("airdrop: {e}");
                failures += 1;
                if failures >= 3 {
                    set_state(State::Failed(e.to_string()));
                    // Not again until wanted anew.
                    while wanted() {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        }
        let _ = power(false);
        std::thread::sleep(Duration::from_millis(300));
    }
    set_state(State::Off);
}

/// Set by [`check`] when the C6 lacks our current esp_hosted or the
/// receiver.
static NEEDS_SETUP: AtomicBool = AtomicBool::new(false);

/// In a Wi-Fi session (the transport up), once per boot: whether the C6
/// runs our current esp_hosted and holds the current receiver. If not, [`setup`] installs
/// them after the session.
pub(crate) fn check() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let ready = match query() {
        Ok(Some(info)) => info.runs_current_hosted() && info.holds_receiver(),
        Ok(None) => false,
        Err(e) => {
            log::warn!("airdrop: the C6 not asked: {e}");
            return;
        }
    };
    if !ready {
        log::info!("airdrop: the C6 needs setting up for AirDrop (after this session)");
    }
    NEEDS_SETUP.store(!ready, Ordering::Relaxed);
}

/// On the `wifi` thread after a session, if [`check`] found something
/// missing: installs it (~35 s the first time, in the background). Returns
/// with the C6 off.
pub(crate) fn setup(power: &Switch, stop_transport: &dyn Fn()) {
    if !NEEDS_SETUP.swap(false, Ordering::Relaxed) {
        return;
    }
    match prepare(power, stop_transport, false) {
        Ok(()) => log::info!("airdrop: the C6 is set up for AirDrop"),
        Err(e) => log::warn!("airdrop: setting up the C6: {e}"),
    }
    let _ = power(false);
}

/// esp_hosted up, our esp_hosted and the receiver installed if needed, then
/// (`boot`) the receiver booted. The C6 is powered on here; the transport
/// is down again when this returns.
fn prepare(power: &Switch, stop_transport: &dyn Fn(), boot: bool) -> anyhow::Result<()> {
    let receiver_version = image_version(C6Image::Receiver);
    for _ in 0..3 {
        power(true)?;
        std::thread::sleep(Duration::from_millis(500));
        // SAFETY: plain FFI; the C6 is powered.
        let err = unsafe { sys::esp_hosted_init() };
        anyhow::ensure!(err == 0, "esp_hosted_init: {err}");
        // Until the C6 answers on SDIO (in a Wi-Fi session the driver's
        // start waits for it). Requests sent before go unanswered, and
        // tearing the transport down half set up crashed in
        // `hosted_sdio_card_deinit`.
        // SAFETY: plain FFI after `esp_hosted_init`.
        let err = unsafe { sys::esp_hosted_connect_to_slave() };
        anyhow::ensure!(err == 0, "esp_hosted_connect_to_slave: {err}");
        let step = (|| -> anyhow::Result<bool> {
            let Some(info) = query()? else {
                let version = c6_version();
                log::warn!(
                    "airdrop: the C6's esp_hosted {version:?} lacks Wakey's addition: installing it"
                );
                // Slaves before 2.6 have no activate RPC: they switch to
                // the new image at the end of the OTA, and the power cycle
                // below starts it.
                install(C6Image::Hosted, version.is_none_or(|v| v >= ACTIVATE_MIN))?;
                return Ok(false);
            };
            if !info.runs_current_hosted() {
                log::info!(
                    "airdrop: updating the C6's esp_hosted {} to {}",
                    info.own_version,
                    image_version(C6Image::Hosted)
                );
                install(C6Image::Hosted, true)?;
                return Ok(false);
            }
            if !info.holds_receiver() {
                log::info!(
                    "airdrop: installing the receiver {receiver_version} into {} (held '{}' {})",
                    info.other_label,
                    info.other_project,
                    info.other_version
                );
                install(C6Image::Receiver, false)?;
                return Ok(false);
            }
            if !boot {
                return Ok(true);
            }
            let result = request(WK_BOOT_AIRDROP, WK_RESULT)?
                .ok_or_else(|| anyhow::anyhow!("no answer to booting the receiver"))?;
            let code = result
                .get(..4)
                .map_or(-1, |b| i32::from_le_bytes(b.try_into().unwrap()));
            anyhow::ensure!(code == 0, "the C6 refused to boot the receiver: {code}");
            Ok(true)
        })();
        stop_transport();
        match step {
            Ok(true) => return Ok(()),
            // Installed: once more from a fresh start of the C6.
            Ok(false) => {
                let _ = power(false);
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => return Err(e),
        }
        if boot && !wanted() {
            anyhow::bail!("no longer wanted");
        }
    }
    anyhow::bail!("the C6 did not get ready for AirDrop")
}

/// Our esp_hosted's answer: its own version and what the other slot holds.
struct SlotInfo {
    /// Empty from the first version of Wakey's addition, which did not
    /// send it.
    own_version: String,
    other_label: String,
    other_project: String,
    other_version: String,
}

impl SlotInfo {
    /// The esp_hosted running is the one in the c6 partition.
    fn runs_current_hosted(&self) -> bool {
        let current = image_version(C6Image::Hosted);
        // An unreadable image is no reason to replace what runs.
        current.is_empty() || self.own_version == current
    }

    fn holds_receiver(&self) -> bool {
        self.other_project == RECEIVER_PROJECT
            && self.other_version == image_version(C6Image::Receiver)
    }
}

/// The version of an image in the c6 partition (its app description),
/// empty if it cannot be read.
fn image_version(which: C6Image) -> String {
    let read = || -> anyhow::Result<String> {
        let (part, at, _) = image(which)?;
        let mut head = [0u8; 112];
        read_part(part, at, &mut head)?;
        app_desc(&head)
            .map(|d| d.1)
            .ok_or_else(|| anyhow::anyhow!("not an app image"))
    };
    read().unwrap_or_else(|e| {
        log::warn!("airdrop: {e}");
        String::new()
    })
}

fn query() -> anyhow::Result<Option<SlotInfo>> {
    // Older than our esp_hosted (the factory 1.4.1): it cannot have the
    // addition, and its custom-data RPC fails (-1) instead of answering
    // `ESP_ERR_NOT_FOUND`.
    if c6_version().is_some_and(|v| v < HOSTED_MIN) {
        return Ok(None);
    }
    let Some(reply) = request(WK_QUERY, WK_INFO)? else {
        return Ok(None);
    };
    // A field the reply is too short for is empty.
    let field = |r: std::ops::Range<usize>| {
        let b = &reply[r.start.min(reply.len())..r.end.min(reply.len())];
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    };
    Ok(Some(SlotInfo {
        other_label: field(1..18),
        other_project: field(18..50),
        other_version: field(50..82),
        own_version: field(82..114),
    }))
}

/// The last reply from our esp_hosted: (message id, payload).
static REPLY: Mutex<Option<(u32, Vec<u8>)>> = Mutex::new(None);
static REPLIED: Condvar = Condvar::new();

unsafe extern "C" fn on_reply(id: u32, data: *const u8, len: usize, _ctx: *mut c_void) {
    // SAFETY: esp_hosted passes `len` valid bytes for the call.
    let bytes = if data.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    *REPLY.lock().unwrap_or_else(|e| e.into_inner()) = Some((id, bytes));
    REPLIED.notify_all();
}

/// Sends `msg` to our esp_hosted and waits for `reply`. `None`: esp_hosted
/// has no handler for `msg` (it answers the RPC with `ESP_ERR_NOT_FOUND`:
/// an esp_hosted without Wakey's addition) or sent no reply.
fn request(msg: u32, reply: u32) -> anyhow::Result<Option<Vec<u8>>> {
    // SAFETY: registers a plain function; esp_hosted keeps the pointer.
    let err = unsafe {
        sys::esp_hosted_register_custom_callback(reply, Some(on_reply), core::ptr::null_mut())
    };
    anyhow::ensure!(err == 0, "register_custom_callback: {err}");
    let mut slot = REPLY.lock().unwrap_or_else(|e| e.into_inner());
    *slot = None;
    // SAFETY: no payload.
    let err = unsafe { sys::esp_hosted_send_custom_data(msg, core::ptr::null(), 0) };
    if err == sys::ESP_ERR_NOT_FOUND {
        return Ok(None);
    }
    anyhow::ensure!(err == 0, "send_custom_data: {err}");
    let deadline = Instant::now() + Duration::from_millis(1500);
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

/// (project, version) of an app image.
fn app_desc(image: &[u8]) -> Option<(String, String)> {
    let text = |r: std::ops::Range<usize>| {
        let b = image.get(r)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Some(String::from_utf8_lossy(&b[..end]).into_owned())
    };
    (image.first() == Some(&0xE9)).then_some(())?;
    Some((text(80..112)?, text(48..80)?))
}

/// The image into the C6's other slot by esp_hosted's OTA; `activate`:
/// and boot it (esp_hosted restarts the C6).
fn install(which: C6Image, activate: bool) -> anyhow::Result<()> {
    let t = Instant::now();
    let (part, at, len) = image(which)?;
    crate::c6_ota::write_from(
        len,
        |offset, buf| read_part(part, at + offset, buf),
        |percent| log::info!("airdrop: install {percent} %"),
    )?;
    if activate {
        // SAFETY: plain RPC.
        let err = unsafe { sys::esp_hosted_slave_ota_activate() };
        anyhow::ensure!(err == 0, "ota_activate: {err}");
    }
    log::info!("airdrop: installed in {} s", t.elapsed().as_secs());
    Ok(())
}

/// Reads the receiver's frames until AirDrop is no longer wanted. An error
/// if the receiver goes silent.
fn receive(link: &mut Link) -> anyhow::Result<()> {
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
