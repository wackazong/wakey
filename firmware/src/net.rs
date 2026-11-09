//! Wi-Fi station through the ESP32-C6 and time sync.
//!
//! The P4 has no radio. The C6 runs Espressif's esp_hosted slave firmware
//! (factory-flashed on the Tab5) and is connected over SDIO; the esp_hosted
//! and esp_wifi_remote components make it look like the normal `esp_wifi`
//! driver, so `EspWifi` works unchanged. Its 3.3 V comes from expander pin
//! WLAN_PWR_EN.
//!
//! [`start`] runs a `wifi` thread: power the C6, bring up the station,
//! connect (retrying with back-off), and once connected sync the clock with
//! SNTP and write the result to the RTC. [`Wifi::status`] is what the UI
//! shows.

use crate::board::ioexp::{IoExpanders, Pin};
use crate::board::rtc::Rx8130;
use crate::config::WifiConfig;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the station is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No credentials (no `config.json` with a `wifi` entry). The driver
    /// still scans once: networks in range, if the scan worked.
    NotConfigured {
        networks: Option<usize>,
    },
    /// Starting the C6 and the driver.
    Starting,
    Connecting {
        ssid: String,
    },
    Connected {
        ssid: String,
        ip: String,
        rssi: Option<i32>,
        /// The clock was synced with NTP on this connection.
        time_synced: bool,
    },
    /// Last attempt failed; retrying later.
    Failed {
        ssid: String,
        reason: String,
    },
}

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

/// Powers the C6 and starts the `wifi` thread. Without credentials it
/// scans once and reports the networks in range.
pub fn start(
    modem: Modem<'static>,
    ioexp: IoExpanders,
    rtc: Rx8130,
    cfg: Option<WifiConfig>,
) -> Wifi {
    let status = Arc::new(Mutex::new(Status::Starting));
    let shared = status.clone();
    std::thread::Builder::new()
        .name("wifi".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let set = |s: Status| *shared.lock().unwrap_or_else(|e| e.into_inner()) = s;
            if let Err(e) = run(modem, &ioexp, &rtc, cfg.as_ref(), &set) {
                log::error!("wifi: driver failed: {e:?}");
                set(Status::Failed {
                    ssid: cfg.map(|c| c.ssid).unwrap_or_else(|| "WLAN".into()),
                    reason: "Treiber".into(),
                });
            }
        })
        .expect("wifi thread");
    Wifi { status }
}

fn run(
    modem: Modem<'static>,
    ioexp: &IoExpanders,
    rtc: &Rx8130,
    cfg: Option<&WifiConfig>,
    set: &dyn Fn(Status),
) -> anyhow::Result<()> {
    // The C6 boots in ~0.5 s; esp_wifi_init resets it again (GPIO15) and
    // waits for the SDIO link.
    ioexp.set(Pin::WlanPwrEn, true)?;
    std::thread::sleep(Duration::from_millis(500));

    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let t = Instant::now();
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), Some(nvs))?, sysloop)?;
    log::info!("wifi: driver up after {} ms", t.elapsed().as_millis());

    let Some(cfg) = cfg else {
        log::info!(
            "wifi: not configured ({} has no \"wifi\" entry), scanning once",
            crate::config::PATH
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
        // Radio off; the C6 stays powered (re-initialising it is slow).
        wifi.stop()?;
        loop {
            std::thread::park();
        }
    };

    let auth = if cfg.password.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: cfg
            .ssid
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("SSID too long"))?,
        password: cfg
            .password
            .as_str()
            .try_into()
            .map_err(|_| anyhow::anyhow!("password too long"))?,
        auth_method: auth,
        ..Default::default()
    }))?;
    wifi.start()?;

    let mut backoff = Duration::from_secs(5);
    loop {
        set(Status::Connecting {
            ssid: cfg.ssid.clone(),
        });
        log::info!("wifi: connecting to \"{}\"", cfg.ssid);
        let attempt = wifi.connect().and_then(|()| wifi.wait_netif_up());
        if let Err(e) = attempt {
            log::warn!("wifi: connecting failed: {e}");
            set(Status::Failed {
                ssid: cfg.ssid.clone(),
                reason: reason(&e),
            });
            let _ = wifi.disconnect();
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(300));
            continue;
        }
        backoff = Duration::from_secs(5);
        let ip = wifi
            .wifi()
            .sta_netif()
            .get_ip_info()
            .map(|i| i.ip.to_string())
            .unwrap_or_default();
        log::info!("wifi: connected, IP {ip}");

        // NTP while connected; the RTC gets the result.
        let sntp = EspSntp::new_default()?;
        let mut time_synced = false;
        let mut sync_logged = false;
        while wifi.is_connected().unwrap_or(false) {
            if !time_synced && sntp.get_sync_status() == SyncStatus::Completed {
                time_synced = true;
                crate::clock::store_system_time(rtc);
            } else if !sync_logged && !time_synced {
                sync_logged = true;
                log::info!("wifi: waiting for NTP");
            }
            set(Status::Connected {
                ssid: cfg.ssid.clone(),
                ip: ip.clone(),
                rssi: wifi.wifi().driver().get_rssi().ok(),
                time_synced,
            });
            std::thread::sleep(Duration::from_secs(2));
        }
        drop(sntp);
        log::warn!("wifi: connection lost");
        let _ = wifi.disconnect();
    }
}

/// Short German reason for the status line.
fn reason(e: &esp_idf_svc::sys::EspError) -> String {
    match e.code() {
        esp_idf_svc::sys::ESP_ERR_TIMEOUT => "Zeitüberschreitung".into(),
        _ => format!("Fehler {}", e.code()),
    }
}
