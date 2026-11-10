//! Wi-Fi station through the ESP32-C6, and NTP.
//!
//! The P4 has no radio. The C6 runs Espressif's esp_hosted slave firmware
//! (flashed by M5Stack) on SDIO; the esp_hosted and esp_wifi_remote
//! components make it look like the normal `esp_wifi` driver, so `EspWifi`
//! works unchanged.
//!
//! [`start`] runs a `wifi` thread: power the C6, bring up the driver,
//! connect (retrying with back-off), and once connected sync the clock by
//! SNTP and report it through `on_time_synced`. Without credentials it
//! scans once, so the UI can show that networks are in range.
//! [`Wifi::status`] is the latest state, for display.

use crate::board::ioexp::Switch;
use crate::config::WifiConfig;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::sys::{ESP_ERR_TIMEOUT, EspError};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Retry delays after a failed connection attempt: doubling up to the cap.
const BACKOFF_START: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Status refresh while connected (signal strength).
const POLL: Duration = Duration::from_secs(2);

/// What the station is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Powering the C6 and starting the driver.
    Starting,
    /// No credentials. `networks`: networks in range from one scan, if it
    /// worked.
    NotConfigured {
        networks: Option<usize>,
    },
    Connecting {
        ssid: String,
    },
    Connected {
        ssid: String,
        ip: String,
        /// Signal strength, dBm.
        rssi: Option<i32>,
        /// The clock was synced by NTP on this connection.
        time_synced: bool,
    },
    /// The last attempt failed; retrying later (or never, for `Driver`).
    Failed {
        ssid: String,
        error: Failure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The driver or the C6 did not come up. No retry.
    Driver,
    /// No connection within the driver's timeout (wrong password, out of
    /// range).
    Timeout,
    /// Any other ESP-IDF error code.
    Other(i32),
}

impl From<&EspError> for Failure {
    fn from(e: &EspError) -> Self {
        match e.code() {
            ESP_ERR_TIMEOUT => Failure::Timeout,
            c => Failure::Other(c),
        }
    }
}

/// Handle on the `wifi` thread. Cheap to clone.
#[derive(Clone)]
pub struct Wifi {
    status: Arc<Mutex<Status>>,
}

impl Wifi {
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Starts the `wifi` thread. `power` switches the C6's 3.3 V (expander pin
/// WLAN_PWR_EN); the driver keeps its calibration in `nvs`;
/// `on_time_synced` runs (on the `wifi` thread) after each successful NTP
/// sync.
pub fn start(
    modem: Modem<'static>,
    power: Switch,
    nvs: EspDefaultNvsPartition,
    cfg: Option<WifiConfig>,
    on_time_synced: impl Fn() + Send + 'static,
) -> Wifi {
    let status = Arc::new(Mutex::new(Status::Starting));
    let shared = status.clone();
    std::thread::Builder::new()
        .name("wifi".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let set = |s: Status| *shared.lock().unwrap_or_else(|e| e.into_inner()) = s;
            let ssid = cfg.as_ref().map_or("WLAN".into(), |c| c.ssid.clone());
            let result = bring_up(modem, &power, nvs).and_then(|wifi| match &cfg {
                Some(cfg) => stay_connected(wifi, cfg, &set, &on_time_synced),
                None => scan_once(wifi, &set),
            });
            if let Err(e) = result {
                log::error!("wifi: driver failed: {e:?}");
                set(Status::Failed {
                    ssid,
                    error: Failure::Driver,
                });
            }
        })
        .expect("wifi thread");
    Wifi { status }
}

/// Powers the C6 and creates the driver (~2.8 s; `esp_wifi_init` resets
/// the C6 through GPIO15 and waits for the SDIO link).
fn bring_up(
    modem: Modem<'static>,
    power: &Switch,
    nvs: EspDefaultNvsPartition,
) -> anyhow::Result<BlockingWifi<EspWifi<'static>>> {
    power(true)?;
    std::thread::sleep(Duration::from_millis(500));
    let sysloop = EspSystemEventLoop::take()?;
    let t = Instant::now();
    let wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), Some(nvs))?, sysloop)?;
    log::info!("wifi: driver up after {} ms", t.elapsed().as_millis());
    Ok(wifi)
}

/// No credentials: one scan for the status line, then the radio stays off
/// (the C6 stays powered; re-initialising it is slow).
fn scan_once(mut wifi: BlockingWifi<EspWifi<'static>>, set: &dyn Fn(Status)) -> anyhow::Result<()> {
    log::info!(
        "wifi: no credentials ({}), scanning once",
        crate::config::FILE
    );
    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
    wifi.start()?;
    let networks = match wifi.scan() {
        Ok(aps) => {
            for ap in &aps {
                log::info!(
                    "wifi: {:>4} dBm  ch {:>2}  {}",
                    ap.signal_strength,
                    ap.channel,
                    ap.ssid
                );
            }
            Some(aps.len())
        }
        Err(e) => {
            log::warn!("wifi: scan failed: {e}");
            None
        }
    };
    set(Status::NotConfigured { networks });
    wifi.stop()?;
    loop {
        std::thread::park();
    }
}

/// Connects and reconnects forever; SNTP while connected.
fn stay_connected(
    mut wifi: BlockingWifi<EspWifi<'static>>,
    cfg: &WifiConfig,
    set: &dyn Fn(Status),
    on_time_synced: &dyn Fn(),
) -> anyhow::Result<()> {
    let too_long = |what| anyhow::anyhow!("Wi-Fi {what} too long");
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: cfg.ssid.as_str().try_into().map_err(|_| too_long("SSID"))?,
        password: cfg
            .password
            .as_str()
            .try_into()
            .map_err(|_| too_long("password"))?,
        auth_method: if cfg.password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    }))?;
    wifi.start()?;

    let ssid = cfg.ssid.clone();
    let mut backoff = BACKOFF_START;
    loop {
        set(Status::Connecting { ssid: ssid.clone() });
        log::info!("wifi: connecting to \"{ssid}\"");
        if let Err(e) = wifi.connect().and_then(|()| wifi.wait_netif_up()) {
            log::warn!("wifi: connecting failed: {e}");
            set(Status::Failed {
                ssid: ssid.clone(),
                error: Failure::from(&e),
            });
            let _ = wifi.disconnect();
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        }
        backoff = BACKOFF_START;
        let ip = wifi
            .wifi()
            .sta_netif()
            .get_ip_info()
            .map(|i| i.ip.to_string())
            .unwrap_or_default();
        log::info!("wifi: connected, IP {ip}");

        let sntp = EspSntp::new_default()?;
        let mut time_synced = false;
        while wifi.is_connected().unwrap_or(false) {
            if !time_synced && sntp.get_sync_status() == SyncStatus::Completed {
                time_synced = true;
                log::info!("wifi: NTP sync done");
                on_time_synced();
            }
            set(Status::Connected {
                ssid: ssid.clone(),
                ip: ip.clone(),
                rssi: wifi.wifi().driver().get_rssi().ok(),
                time_synced,
            });
            std::thread::sleep(POLL);
        }
        drop(sntp);
        log::warn!("wifi: connection lost");
        let _ = wifi.disconnect();
    }
}
