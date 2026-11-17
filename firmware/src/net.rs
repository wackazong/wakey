//! Wi-Fi station through the ESP32-C6, and NTP, only when needed.
//!
//! The P4 has no radio. The C6 ([`crate::c6`]) runs esp_hosted 2.13.1
//! with Wakey's addition on SDIO (installed by the AirDrop setup if
//! missing); the esp_hosted and esp_wifi_remote components make it look
//! like the normal `esp_wifi` driver, so `EspWifi` works unchanged.
//!
//! [`start`] runs a `wifi` thread that works in sessions: power the C6,
//! bring up the driver, connect, sync the clock by SNTP (reported through
//! `on_time_synced`), then drop the driver and switch the C6 off. The next
//! session follows `sync_interval_h` later (sooner after a failure), and
//! only while the display is awake. Without credentials a session is one
//! scan, which reports how many networks are in range. [`Wifi::status`]
//! is the latest state, for display.
//!
//! [`Wifi::run`] asks for a session now, for a job that needs the network
//! (TIDAL): it runs on the `wifi` thread once connected, before the clock
//! sync, and learns why if no connection came about.
//!
//! While AirDrop is wanted ([`crate::airdrop::set_wanted`], the sound
//! page's Files tab), the thread gives the C6 to it instead: a session
//! under way ends early (an interrupted clock sync is done after AirDrop),
//! and jobs wait.

use crate::airdrop;
use crate::board::C6Link;
use crate::board::ioexp::Switch;
use crate::c6::{self, C6};
use crate::config::{self, WifiConfig};
use crate::http;
use crate::runtime::Thread;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::sys::{self, ESP_ERR_TIMEOUT, EspError};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Connection attempts per session, and the pause between them.
const ATTEMPTS: u32 = 3;
const ATTEMPT_PAUSE: Duration = Duration::from_secs(5);
/// How long a session waits for the NTP answer.
const NTP_TIMEOUT: Duration = Duration::from_secs(60);
/// After a failed session: the first retry, doubling up to the interval.
const RETRY_FIRST: Duration = Duration::from_secs(10 * 60);

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
    },
    /// Off until the next session; the last one synced the clock.
    Off,
    /// The last session failed; the next one follows after a back-off
    /// (10 min, doubling up to the interval).
    Failed {
        ssid: String,
        error: Failure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The C6, its transport or the driver did not come up.
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

/// Work for a session: called on the `wifi` thread with `Ok` while
/// connected, or with the reason there is no connection.
pub type Job = Box<dyn FnOnce(Result<(), Unavailable>) + Send>;

/// Why a job runs without a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No Wi-Fi credentials in `config.json`.
    NotConfigured,
    /// The network did not take the connection (wrong password, out of
    /// range).
    NoConnection { ssid: String },
    /// The C6 or the driver did not come up, or the `wifi` thread is gone.
    Failed,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::NotConfigured => f.write_str("no Wi-Fi configured"),
            Unavailable::NoConnection { ssid } => write!(f, "no connection to \"{ssid}\""),
            Unavailable::Failed => f.write_str("Wi-Fi failed"),
        }
    }
}

/// Makes sure a job hears back: dropped unrun, it gets an error.
struct JobGuard(Option<Job>);

impl JobGuard {
    fn run(mut self, result: Result<(), Unavailable>) {
        if let Some(job) = self.0.take() {
            job(result);
        }
    }

    /// The job back, unrun (for a later session).
    fn into_job(mut self) -> Option<Job> {
        self.0.take()
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            job(Err(Unavailable::Failed));
        }
    }
}

/// Handle on the `wifi` thread. Cheap to clone.
#[derive(Clone)]
pub struct Wifi {
    status: Arc<Mutex<Status>>,
    jobs: mpsc::Sender<Job>,
}

impl Wifi {
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Runs `job` in a session as soon as the thread is free (now, unless
    /// a session is under way).
    pub fn run(&self, job: Job) {
        if let Err(mpsc::SendError(job)) = self.jobs.send(job) {
            job(Err(Unavailable::Failed));
        }
    }
}

/// What the `wifi` thread needs.
pub struct Setup {
    pub modem: Modem<'static>,
    /// The C6's 3.3 V (expander pin WLAN_PWR_EN).
    pub c6_power: Switch,
    /// The C6's link for AirDrop's frames.
    pub c6_link: C6Link,
    /// The driver keeps its calibration here.
    pub nvs: EspDefaultNvsPartition,
    pub cfg: Option<WifiConfig>,
    /// Time between sessions.
    pub interval: Duration,
    /// The display sleeps: no periodic session then.
    pub display_asleep: fn() -> bool,
}

/// Starts the `wifi` thread. `on_time_synced` runs (on the `wifi` thread)
/// after each successful NTP sync.
pub fn start(setup: Setup, on_time_synced: impl Fn() + Send + 'static) -> Wifi {
    let status = Arc::new(Mutex::new(Status::Starting));
    let (jobs, job_rx) = mpsc::channel::<Job>();
    let wifi = Wifi {
        status: status.clone(),
        jobs,
    };
    // On core 1 at priority 1, below the audio decoder (5) and the card
    // writer (4), so neither starves: a download takes about half a core.
    // On core 0, at the UI's priority, it makes animations stutter.
    Thread::new(c"wifi", 16 * 1024)
        .core(Core::Core1)
        .priority(1)
        .spawn(move || {
            let sysloop = match EspSystemEventLoop::take() {
                Ok(l) => l,
                Err(e) => {
                    log::error!("wifi: event loop: {e}");
                    return;
                }
            };
            let station = Station {
                c6: C6::new(setup.c6_power),
                c6_link: setup.c6_link,
                sysloop,
                nvs: setup.nvs,
                cfg: setup.cfg,
                interval: setup.interval,
                display_asleep: setup.display_asleep,
                on_time_synced: Box::new(on_time_synced),
                status,
                jobs: job_rx,
            };
            station.run(setup.modem)
        })
        .expect("wifi thread");
    wifi
}

/// How a session ended.
enum Outcome {
    Synced,
    /// No credentials: one scan.
    Scanned,
    /// Ended early for AirDrop, the clock not synced; the job if it had
    /// not run yet (it runs in the session after AirDrop).
    Interrupted(Option<Job>),
}

/// The `wifi` thread.
struct Station {
    c6: C6,
    c6_link: C6Link,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    cfg: Option<WifiConfig>,
    interval: Duration,
    display_asleep: fn() -> bool,
    on_time_synced: Box<dyn Fn() + Send>,
    status: Arc<Mutex<Status>>,
    jobs: mpsc::Receiver<Job>,
}

impl Station {
    fn set(&self, s: Status) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    /// Sessions, AirDrop and jobs, forever.
    fn run(mut self, mut modem: Modem<'static>) {
        let mut retry = RETRY_FIRST;
        let mut job: Option<Job> = None;
        // A session is due: the clock sync at boot, after the interval, or
        // one AirDrop interrupted.
        let mut due = true;
        let mut next = Instant::now();
        loop {
            if airdrop::wanted() {
                airdrop::run(&mut self.c6_link, &self.c6);
            }
            if due || job.is_some() {
                self.set(Status::Starting);
                let t = Instant::now();
                let result = self.session(&mut modem, JobGuard(job.take()));
                self.c6.off();
                // The C6's images for AirDrop, if the session found them
                // missing or outdated (once).
                self.c6.set_up();
                let wait = match result {
                    Ok(Outcome::Synced) => {
                        (self.on_time_synced)();
                        self.set(Status::Off);
                        retry = RETRY_FIRST;
                        due = false;
                        self.interval
                    }
                    Ok(Outcome::Scanned) => {
                        due = false;
                        self.interval
                    }
                    Ok(Outcome::Interrupted(unrun)) => {
                        job = unrun;
                        self.set(Status::Off);
                        Duration::ZERO
                    }
                    Err(e) => {
                        log::warn!("wifi: session failed: {e:#}");
                        let failed = matches!(
                            *self.status.lock().unwrap_or_else(|e| e.into_inner()),
                            Status::Failed { .. }
                        );
                        if !failed {
                            self.set(Status::Failed {
                                ssid: self.ssid(),
                                error: Failure::Driver,
                            });
                        }
                        due = false;
                        let w = retry;
                        retry = (retry * 2).min(self.interval);
                        w
                    }
                };
                next = Instant::now() + wait;
                // SAFETY: plain heap statistics.
                let free = |caps| unsafe { sys::heap_caps_get_free_size(caps) } / 1024;
                log::info!(
                    "wifi: session took {} s, C6 off; next in {} min (free: internal {} KiB, PSRAM {} KiB)",
                    t.elapsed().as_secs(),
                    wait.as_secs() / 60,
                    free(sys::MALLOC_CAP_INTERNAL),
                    free(sys::MALLOC_CAP_SPIRAM)
                );
            }
            // Until AirDrop is wanted, a job comes, or the next session is
            // due (not while the display sleeps: it waits for it). A job a
            // session gave back unrun goes first: a new one received here
            // would take its place, and it would never answer.
            loop {
                if airdrop::wanted() || job.is_some() {
                    break;
                }
                if Instant::now() >= next && !(self.display_asleep)() {
                    due = true;
                    break;
                }
                if let Ok(j) = self.jobs.recv_timeout(Duration::from_millis(250)) {
                    job = Some(j);
                    break;
                }
            }
        }
    }

    fn ssid(&self) -> String {
        self.cfg.as_ref().map_or("WLAN".into(), |c| c.ssid.clone())
    }

    /// One session: C6 on, transport and driver up, connect and sync the
    /// clock (or scan once without credentials), driver and transport
    /// down. The caller switches the C6 off.
    fn session(&self, modem: &mut Modem<'static>, job: JobGuard) -> anyhow::Result<Outcome> {
        let t = Instant::now();
        let transport = self.c6.connect()?;
        // SAFETY: the reborrowed modem lives only as long as this session's
        // driver, which is dropped before the next session reborrows it.
        let modem = unsafe { modem.reborrow() };
        let wifi = BlockingWifi::wrap(
            EspWifi::new(modem, self.sysloop.clone(), Some(self.nvs.clone()))?,
            self.sysloop.clone(),
        )?;
        log::info!("wifi: driver up after {} ms", t.elapsed().as_millis());
        c6::check(&transport);
        // The driver (dropped by these, `esp_wifi_deinit`) goes before the
        // transport.
        match &self.cfg {
            Some(cfg) => self.sync_clock(wifi, cfg, job),
            None => {
                job.run(Err(Unavailable::NotConfigured));
                self.scan_once(wifi).map(|()| Outcome::Scanned)
            }
        }
    }

    /// No credentials: one scan for the status line.
    fn scan_once(&self, mut wifi: BlockingWifi<EspWifi<'_>>) -> anyhow::Result<()> {
        log::info!("wifi: no credentials ({}), scanning once", config::FILE);
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
        self.set(Status::NotConfigured { networks });
        wifi.stop()?;
        Ok(())
    }

    /// Connects (up to [`ATTEMPTS`] tries), runs `job` and waits for SNTP.
    /// Jobs that come in while connected run in this session too: a TIDAL
    /// tab opened during a slow NTP sync does not wait for the next session.
    /// Ends early when AirDrop is wanted.
    fn sync_clock(
        &self,
        mut wifi: BlockingWifi<EspWifi<'_>>,
        cfg: &WifiConfig,
        job: JobGuard,
    ) -> anyhow::Result<Outcome> {
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
        wifi_mode();
        let ssid = cfg.ssid.clone();
        let mut attempt = 0;
        loop {
            if airdrop::wanted() {
                let _ = wifi.stop();
                return Ok(Outcome::Interrupted(job.into_job()));
            }
            attempt += 1;
            self.set(Status::Connecting { ssid: ssid.clone() });
            log::info!("wifi: connecting to \"{ssid}\" (attempt {attempt})");
            match wifi.connect().and_then(|()| wifi.wait_netif_up()) {
                Ok(()) => break,
                Err(e) => {
                    log::warn!("wifi: connecting failed: {e}");
                    self.set(Status::Failed {
                        ssid: ssid.clone(),
                        error: Failure::from(&e),
                    });
                    let _ = wifi.disconnect();
                    if attempt >= ATTEMPTS {
                        let _ = wifi.stop();
                        job.run(Err(Unavailable::NoConnection { ssid: ssid.clone() }));
                        anyhow::bail!("no connection to \"{ssid}\"");
                    }
                    std::thread::sleep(ATTEMPT_PAUSE);
                }
            }
        }
        let ip = wifi
            .wifi()
            .sta_netif()
            .get_ip_info()
            .map(|i| i.ip.to_string())
            .unwrap_or_default();
        log::info!("wifi: connected, IP {ip}");
        self.set(Status::Connected {
            ssid: ssid.clone(),
            ip,
            rssi: wifi.wifi().driver().get_rssi().ok(),
        });
        // SNTP first: lwIP syncs in the background while the job runs, so a
        // long job (an open TIDAL tab) does not hold the clock back. The
        // clock from the RTC is good enough for TLS meanwhile.
        let sntp = EspSntp::new_default();
        let t = Instant::now();
        job.run(Ok(()));
        // Jobs that come in meanwhile; not once AirDrop is wanted (they wait
        // for the session after it).
        let run_more = || {
            while !airdrop::wanted()
                && let Ok(job) = self.jobs.try_recv()
            {
                job(Ok(()));
            }
        };
        run_more();
        let sntp = match sntp {
            Ok(sntp) => sntp,
            Err(e) => {
                // The jobs may have left connections in the pool.
                http::close_all();
                return Err(e.into());
            }
        };
        let synced = loop {
            run_more();
            if airdrop::wanted() {
                break None;
            }
            if sntp.get_sync_status() == SyncStatus::Completed {
                log::info!("wifi: NTP sync done after {} ms", t.elapsed().as_millis());
                break Some(true);
            }
            if t.elapsed() > NTP_TIMEOUT {
                log::warn!("wifi: no NTP answer within {} s", NTP_TIMEOUT.as_secs());
                break Some(false);
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        drop(sntp);
        run_more();
        // Kept HTTPS connections (`http`) end with the session.
        http::close_all();
        let _ = wifi.disconnect();
        let _ = wifi.stop();
        match synced {
            None => {
                log::info!("wifi: session ended for AirDrop");
                Ok(Outcome::Interrupted(None))
            }
            Some(true) => Ok(Outcome::Synced),
            Some(false) => {
                self.set(Status::Failed {
                    ssid,
                    error: Failure::Timeout,
                });
                anyhow::bail!("no NTP sync")
            }
        }
    }
}

/// 802.11b/g/n, not ax: with 802.11ax (HE20) downloads through the C6
/// are ~25 % slower.
fn wifi_mode() {
    let bgn = (sys::WIFI_PROTOCOL_11B | sys::WIFI_PROTOCOL_11G | sys::WIFI_PROTOCOL_11N) as u8;
    // SAFETY: a plain driver call after `start`, before `connect`.
    let err = unsafe { sys::esp_wifi_set_protocol(sys::wifi_interface_t_WIFI_IF_STA, bgn) };
    if err != 0 {
        log::warn!("wifi: 802.11b/g/n only: {err}");
    }
}
