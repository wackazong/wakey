//! Spike: AirDrop reception on the C6 (`spikes/airdrop-c6`).
//!
//! Installs the spike's C6 image through esp_hosted's slave OTA (as
//! `c6_update`), lets the C6 restart into it, and prints the C6's log, which
//! leaves the C6 on its IO2 (UART0 TX, 921600 baud) into the P4's GPIO14.
//! The spike points the C6's boot slot back at esp_hosted as soon as it runs,
//! so the next power cycle of the C6 (the next boot of the P4: the expanders
//! switch it off) brings the normal firmware back.
//!
//! Build the C6 image first (`spikes/airdrop-c6/README.md`), then
//! `cargo run --release --bin airdrop_probe --features airdrop-probe`.

use anyhow::Context;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::{UartRxDriver, config::Config as UartConfig};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use esp_idf_svc::wifi::EspWifi;
use std::time::Duration;
use wakey::board::i2c::SysI2c;
use wakey::board::ioexp::{IoExpanders, Pin};

static IMAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../spikes/airdrop-c6/build/wakey_airdrop_c6.bin"
));
/// The spike's project name in its app description.
const PROJECT: &str = "wakey_airdrop_c6";
/// Bytes per OTA write RPC (as `c6_update`).
const CHUNK: usize = 1400;
/// The C6's console (`spikes/airdrop-c6/sdkconfig.defaults`).
const C6_BAUD: u32 = 921_600;

fn main() -> anyhow::Result<()> {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let p = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    anyhow::ensure!(
        project_name(IMAGE) == Some(PROJECT),
        "the image is not the spike's ({:?})",
        project_name(IMAGE)
    );

    let i2c = std::sync::Arc::new(std::sync::Mutex::new(SysI2c::new(
        p.i2c0,
        p.pins.gpio31.degrade_input_output(),
        p.pins.gpio32.degrade_input_output(),
    )?));
    let ioexp = IoExpanders::new(i2c)?;
    let power = ioexp.switch(Pin::WlanPwrEn);

    // The log reader first: the C6 starts talking within ~2 s of `activate`.
    let uart = UartRxDriver::new(
        p.uart1,
        p.pins.gpio14,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &UartConfig::new().baudrate(Hertz(C6_BAUD)).rx_fifo_size(4096),
    )?;

    // esp_hosted up, as in a Wi-Fi session (`net::session`), and the image
    // installed. If the C6 does not answer as esp_hosted, it may still be
    // booting the spike: then only listen.
    power(true)?;
    std::thread::sleep(Duration::from_millis(500));
    let installed = (move || -> anyhow::Result<()> {
        // SAFETY: plain FFI calls; the C6 is powered.
        let err = unsafe { sys::esp_hosted_init() };
        anyhow::ensure!(err == 0, "esp_hosted_init: {err}");
        let wifi = EspWifi::new(p.modem, sysloop, Some(nvs))?;
        log_c6_version();
        install(IMAGE).context("installing the spike")?;
        drop(wifi);
        // SAFETY: as above; the slave restarts on its own after `activate`.
        let err = unsafe { sys::esp_hosted_deinit() };
        if err != 0 {
            log::warn!("probe: esp_hosted_deinit: {err}");
        }
        Ok(())
    })();
    match installed {
        Ok(()) => log::info!("probe: the C6 restarts into the spike; its log follows (c6 | …)"),
        Err(e) => log::warn!("probe: not installed ({e:#}); listening to the C6 as it is"),
    }
    let mut log = C6Log::default();
    if log.follow(&uart, Duration::from_secs(12)) {
        log.follow_forever(&uart);
    }

    // Silence: boot the C6 afresh from its boot record (the spike, unless it
    // already ran and pointed it back at esp_hosted).
    log::warn!("probe: nothing from the C6; power-cycling it");
    power(false)?;
    std::thread::sleep(Duration::from_secs(1));
    power(true)?;
    if log.follow(&uart, Duration::from_secs(12)) {
        log.follow_forever(&uart);
    }

    // Still silent: is esp_hosted running?
    log::warn!("probe: still nothing; asking for esp_hosted");
    // SAFETY: as above.
    let err = unsafe { sys::esp_hosted_init() };
    log::info!("probe: esp_hosted_init: {err}");
    log_c6_version();
    log::warn!(
        "probe: an answer means the C6 boots esp_hosted (the spike was rejected, or ran \
         without output); none means it is stuck in the spike"
    );
    log.follow_forever(&uart)
}

/// The project name in an app image's description.
fn project_name(image: &[u8]) -> Option<&str> {
    // esp_app_desc_t after the 24-byte image header and the 8-byte segment
    // header: magic, secure_version, reserv1[2], version[32], project_name[32].
    if image.first() != Some(&0xE9) || image.get(32..36)? != 0xABCD_5432u32.to_le_bytes() {
        return None;
    }
    let field = image.get(80..112)?;
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).ok()
}

fn log_c6_version() {
    let mut v = sys::esp_hosted_coprocessor_fwver_t::default();
    // SAFETY: `v` is a plain out-parameter, valid for the call.
    let err = unsafe { sys::esp_hosted_get_coprocessor_fwversion(&mut v) };
    if err == 0 {
        log::info!("probe: the C6 runs esp_hosted {}.{}.{}", v.major1, v.minor1, v.patch1);
    } else {
        log::warn!("probe: C6 version unknown ({err})");
    }
}

/// esp_hosted's slave OTA: begin, write, end (the C6 checks the image),
/// activate (the C6 boots it next, restarting after 2 s).
fn install(image: &[u8]) -> anyhow::Result<()> {
    log::info!("probe: sending the spike ({} bytes)", image.len());
    // SAFETY: plain RPCs over the transport, which is up.
    let err = unsafe { sys::esp_hosted_slave_ota_begin() };
    anyhow::ensure!(err == 0, "ota_begin: {err}");
    let mut buf = vec![0u8; CHUNK];
    for (i, chunk) in image.chunks(CHUNK).enumerate() {
        buf[..chunk.len()].copy_from_slice(chunk);
        // SAFETY: `buf` holds `chunk.len()` valid bytes for the call.
        let err = unsafe { sys::esp_hosted_slave_ota_write(buf.as_mut_ptr(), chunk.len() as u32) };
        anyhow::ensure!(err == 0, "ota_write at {}: {err}", i * CHUNK);
    }
    // SAFETY: as above.
    let err = unsafe { sys::esp_hosted_slave_ota_end() };
    anyhow::ensure!(err == 0, "ota_end: {err}");
    // SAFETY: as above.
    let err = unsafe { sys::esp_hosted_slave_ota_activate() };
    anyhow::ensure!(err == 0, "ota_activate: {err}");
    log::info!("probe: installed and activated");
    Ok(())
}

/// The C6's log, read from the UART and printed line by line.
#[derive(Default)]
struct C6Log {
    line: Vec<u8>,
    /// Bytes received in all, also those of no complete line.
    bytes: usize,
}

impl C6Log {
    /// Prints what arrives for `quiet` after the last byte; true once
    /// anything arrived.
    fn follow(&mut self, uart: &UartRxDriver<'_>, quiet: Duration) -> bool {
        let start = self.bytes;
        let mut last = std::time::Instant::now();
        let mut report = std::time::Instant::now();
        while last.elapsed() < quiet {
            if self.read(uart) > 0 {
                last = std::time::Instant::now();
            }
            // Bytes without a newline (a wrong baud rate) show here.
            if report.elapsed() >= Duration::from_secs(3) {
                report = std::time::Instant::now();
                if !self.line.is_empty() {
                    log::info!("probe: {} B from the C6, unfinished line: {:02x?}", self.bytes,
                        &self.line[..self.line.len().min(32)]);
                }
            }
        }
        self.bytes > start
    }

    fn follow_forever(&mut self, uart: &UartRxDriver<'_>) -> ! {
        loop {
            self.follow(uart, Duration::from_secs(3600));
        }
    }

    /// One read with a short timeout (a blocking read would wait for the
    /// whole buffer); returns the bytes read.
    fn read(&mut self, uart: &UartRxDriver<'_>) -> usize {
        let timeout = TickType::from(Duration::from_millis(20)).ticks();
        let mut buf = [0u8; 512];
        let n = uart.read(&mut buf, timeout).unwrap_or(0);
        self.bytes += n;
        for &b in &buf[..n] {
            match b {
                b'\n' => {
                    let text = String::from_utf8_lossy(&self.line);
                    log::info!("c6 | {}", text.trim_end_matches('\r'));
                    self.line.clear();
                }
                _ if self.line.len() < 1024 => self.line.push(b),
                _ => {}
            }
        }
        n
    }
}
