//! Wi-Fi station through the ESP32-C6, and NTP, only when needed.
//!
//! The P4 has no radio. The C6 runs Espressif's esp_hosted slave firmware
//! (flashed by M5Stack) on SDIO; the esp_hosted and esp_wifi_remote
//! components make it look like the normal `esp_wifi` driver, so `EspWifi`
//! works unchanged.
//!
//! [`start`] runs a `wifi` thread that works in sessions: power the C6,
//! bring up the driver, connect, sync the clock by SNTP (reported through
//! `on_time_synced`), then drop the driver and switch the C6 off. The next
//! session follows `sync_interval_h` later (sooner after a failure), and
//! only while the display is awake. Without credentials a session is one
//! scan, so the UI can show that networks are in range. Between sessions
//! nothing of Wi-Fi runs, which light sleep needs. [`Wifi::status`] is the
//! latest state, for display.
//!
//! [`Wifi::run`] asks for a session now, for a job that needs the network
//! (TIDAL): it runs on the `wifi` thread once connected, before the clock
//! sync, and learns why if no connection came about.

use crate::board::ioexp::Switch;
use crate::config::WifiConfig;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::cpu::Core;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::sys::{ESP_ERR_TIMEOUT, EspError};
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Work for a session: called on the `wifi` thread with `Ok` while
/// connected, or with the reason there is no connection.
pub type Job = Box<dyn FnOnce(Result<(), String>) + Send>;

/// Makes sure a job hears back: dropped unrun, it gets an error.
struct JobGuard(Option<Job>);

impl JobGuard {
    fn run(mut self, result: Result<(), String>) {
        if let Some(job) = self.0.take() {
            job(result);
        }
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            job(Err("WLAN-Fehler".into()));
        }
    }
}

/// Handle on the `wifi` thread. Cheap to clone.
#[derive(Clone)]
pub struct Wifi {
    status: Arc<Mutex<Status>>,
    active: Arc<AtomicBool>,
    jobs: mpsc::Sender<Job>,
}

impl Wifi {
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// True during a session (the C6 powered, the driver up).
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Runs `job` in a session as soon as the thread is free (now, unless
    /// a session is under way).
    pub fn run(&self, job: Job) {
        if let Err(mpsc::SendError(job)) = self.jobs.send(job) {
            job(Err("WLAN nicht verfügbar".into()));
        }
    }
}

/// Starts the `wifi` thread. `power` switches the C6's 3.3 V (expander pin
/// WLAN_PWR_EN); the driver keeps its calibration in `nvs`;
/// `on_time_synced` runs (on the `wifi` thread) after each successful NTP
/// sync; `interval` is the time between sessions.
pub fn start(
    mut modem: Modem<'static>,
    power: Switch,
    nvs: EspDefaultNvsPartition,
    cfg: Option<WifiConfig>,
    interval: Duration,
    on_time_synced: impl Fn() + Send + 'static,
) -> Wifi {
    let status = Arc::new(Mutex::new(Status::Starting));
    let active = Arc::new(AtomicBool::new(false));
    let (jobs, job_rx) = mpsc::channel::<Job>();
    let wifi = Wifi {
        status: status.clone(),
        active: active.clone(),
        jobs,
    };
    // On core 1 at priority 1, below the audio decoder (5) and the card
    // writer (4), so neither starves: a download takes about half a core.
    // On core 0 it shared the UI's priority there and made animations
    // stutter (a list row's repaint went from ~20 to ~40 ms). Core 1 is
    // mostly idle; a download there is as fast (`netbench`).
    let previous = ThreadSpawnConfiguration::get().unwrap_or_default();
    let pinned = ThreadSpawnConfiguration {
        name: Some(c"wifi"),
        pin_to_core: Some(Core::Core1),
        priority: 1,
        ..ThreadSpawnConfiguration::default()
    };
    if let Err(e) = pinned.set() {
        log::warn!("wifi: thread not pinned: {e}");
    }
    std::thread::Builder::new()
        .name("wifi".into())
        .stack_size(16 * 1024)
        .spawn(crate::taskstats::named("wifi", move || {
            let set = |s: Status| *status.lock().unwrap_or_else(|e| e.into_inner()) = s;
            let sysloop = match EspSystemEventLoop::take() {
                Ok(l) => l,
                Err(e) => {
                    log::error!("wifi: event loop: {e}");
                    return;
                }
            };
            #[cfg(feature = "wifi-leak-test")]
            leak_test(&mut modem, &power, &sysloop, &nvs, cfg.as_ref());
            let ssid = cfg.as_ref().map_or("WLAN".into(), |c| c.ssid.clone());
            let mut retry = RETRY_FIRST;
            let mut job: Option<Job> = None;
            loop {
                set(Status::Starting);
                active.store(true, Ordering::Relaxed);
                let t = Instant::now();
                let result = session(
                    &mut modem,
                    &power,
                    &sysloop,
                    nvs.clone(),
                    cfg.as_ref(),
                    &set,
                    JobGuard(job.take()),
                    &job_rx,
                );
                end_session(&power);
                active.store(false, Ordering::Relaxed);
                let wait = match result {
                    Ok(synced) => {
                        if synced {
                            on_time_synced();
                            set(Status::Off);
                        }
                        retry = RETRY_FIRST;
                        interval
                    }
                    Err(e) => {
                        log::warn!("wifi: session failed: {e:?}");
                        if !matches!(
                            *status.lock().unwrap_or_else(|e| e.into_inner()),
                            Status::Failed { .. }
                        ) {
                            set(Status::Failed {
                                ssid: ssid.clone(),
                                error: Failure::Driver,
                            });
                        }
                        let w = retry;
                        retry = (retry * 2).min(interval);
                        w
                    }
                };
                let free = |caps| unsafe { esp_idf_svc::sys::heap_caps_get_free_size(caps) } / 1024;
                log::info!(
                    "wifi: session took {} s, C6 off; next in {} min (free: internal {} KiB, PSRAM {} KiB)",
                    t.elapsed().as_secs(),
                    wait.as_secs() / 60,
                    free(esp_idf_svc::sys::MALLOC_CAP_INTERNAL),
                    free(esp_idf_svc::sys::MALLOC_CAP_SPIRAM)
                );
                // The next scheduled session, or a job sooner.
                job = job_rx.recv_timeout(wait).ok();
                // Not while the display sleeps: the next session waits for it.
                while job.is_none() && crate::ui::power::asleep() {
                    job = job_rx.recv_timeout(Duration::from_secs(10)).ok();
                }
            }
        }))
        .expect("wifi thread");
    let _ = previous.set();
    wifi
}

/// Feature `wifi-leak-test`: Wi-Fi sessions in growing stages, five each,
/// logging the free heap after every one (`wifi: leak test …` lines):
/// B driver up and down, C also connected, D also one HTTPS request. The
/// first round of a stage may keep memory for good; rounds 2–5 show what
/// a session loses.
#[cfg(feature = "wifi-leak-test")]
fn leak_test(
    modem: &mut Modem<'static>,
    power: &Switch,
    sysloop: &EspSystemEventLoop,
    nvs: &EspDefaultNvsPartition,
    cfg: Option<&WifiConfig>,
) {
    use esp_idf_svc::sys::{MALLOC_CAP_INTERNAL, MALLOC_CAP_SPIRAM, heap_caps_get_free_size};
    let free = || unsafe {
        (
            heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
            heap_caps_get_free_size(MALLOC_CAP_SPIRAM),
        )
    };
    let Some(cfg) = cfg else {
        log::warn!("wifi: leak test needs Wi-Fi credentials");
        return;
    };
    // E: one transport session (esp_hosted init … deinit), the driver up
    // and down five times in it: does the driver or the transport lose it?
    {
        let start = free();
        let _ = power(true);
        std::thread::sleep(Duration::from_millis(500));
        let err = unsafe { esp_idf_svc::sys::esp_hosted_init() };
        log::info!("wifi: leak test E: esp_hosted_init {err}");
        for round in 1..=5 {
            let before = free();
            let modem = unsafe { modem.reborrow() };
            let r = EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))
                .and_then(|w| BlockingWifi::wrap(w, sysloop.clone()));
            let ok = r.is_ok();
            drop(r);
            std::thread::sleep(Duration::from_millis(500));
            let after = free();
            log::info!(
                "wifi: leak test E{round}: driver {}; internal {:+} B, PSRAM {:+} B",
                if ok { "up/down" } else { "failed" },
                after.0 as i64 - before.0 as i64,
                after.1 as i64 - before.1 as i64
            );
        }
        end_session(power);
        std::thread::sleep(Duration::from_secs(1));
        let end = free();
        log::info!(
            "wifi: leak test E total with the transport: internal {:+} B, PSRAM {:+} B",
            end.0 as i64 - start.0 as i64,
            end.1 as i64 - start.1 as i64
        );
    }
    for stage in ['B', 'C', 'D'] {
        for round in 1..=5 {
            let before = free();
            let result = (|| -> anyhow::Result<()> {
                power(true)?;
                std::thread::sleep(Duration::from_millis(500));
                let err = unsafe { esp_idf_svc::sys::esp_hosted_init() };
                anyhow::ensure!(err == 0, "esp_hosted_init: {err}");
                let modem = unsafe { modem.reborrow() };
                let mut wifi = BlockingWifi::wrap(
                    EspWifi::new(modem, sysloop.clone(), Some(nvs.clone()))?,
                    sysloop.clone(),
                )?;
                if stage != 'B' {
                    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
                        ssid: cfg.ssid.as_str().try_into().map_err(|_| anyhow::anyhow!("ssid"))?,
                        password: cfg.password.as_str().try_into().map_err(|_| anyhow::anyhow!("pw"))?,
                        auth_method: if cfg.password.is_empty() { AuthMethod::None } else { AuthMethod::WPA2Personal },
                        ..Default::default()
                    }))?;
                    wifi.start()?;
                    wifi.connect()?;
                    wifi.wait_netif_up()?;
                    if stage == 'D' {
                        let r = crate::http::download("https://example.com/", &mut |_| Ok(()), &mut |_, _| {});
                        log::info!("wifi: leak test: request {:?}", r.map_err(|e| e.to_string()));
                        crate::http::close_all();
                    }
                    let _ = wifi.disconnect();
                    let _ = wifi.stop();
                }
                drop(wifi);
                Ok(())
            })();
            end_session(power);
            std::thread::sleep(Duration::from_secs(1));
            let after = free();
            log::info!(
                "wifi: leak test {stage}{round}: {:?}; internal {} → {} ({:+} B), PSRAM {:+} B",
                result.err(),
                before.0,
                after.0,
                after.0 as i64 - before.0 as i64,
                after.1 as i64 - before.1 as i64
            );
        }
    }
}

/// After a session (the driver dropped): tears down esp_hosted's transport
/// to the C6, then powers the C6 off. Powered off with the transport still
/// up, the next session's link finds the C6 "unresponsive" and aborts.
///
/// esp_hosted's tasks have their stacks in PSRAM
/// (`CONFIG_ESP_HOSTED_DFLT_TASK_FROM_SPIRAM`): created static, with stack
/// and control block from the heap, and deleted without freeing them
/// (`hosted_thread_cancel`): 2136 B internal RAM and ~28.7 KB PSRAM lost per
/// session. Their buffers are taken before the deinit and freed once the
/// tasks are gone.
fn end_session(power: &Switch) {
    let leftovers = hosted_task_buffers();
    let err = unsafe { esp_idf_svc::sys::esp_hosted_deinit() };
    if err != 0 {
        log::warn!("wifi: esp_hosted_deinit: {err}");
    }
    free_task_buffers(leftovers);
    let _ = power(false);
}

/// esp_hosted's tasks (SDIO transport and RPC; `hosted_thread_create`).
const HOSTED_TASKS: [&core::ffi::CStr; 7] = [
    c"sdio_rx_buf",
    c"sdio_read",
    c"sdio_process_rx",
    c"sdio_write",
    c"rpc_rx",
    c"rpc_tx",
    c"rpc_supp_cb",
];

/// Stack and control block of each running esp_hosted task that was
/// created static (by name).
fn hosted_task_buffers() -> Vec<(&'static core::ffi::CStr, *mut u8, *mut u8)> {
    use esp_idf_svc::sys;
    HOSTED_TASKS
        .iter()
        .filter_map(|name| {
            // SAFETY: plain FreeRTOS queries on a running task.
            unsafe {
                let task = sys::xTaskGetHandle(name.as_ptr());
                if task.is_null() {
                    return None;
                }
                let mut stack: *mut sys::StackType_t = core::ptr::null_mut();
                let mut tcb: *mut sys::StaticTask_t = core::ptr::null_mut();
                (sys::xTaskGetStaticBuffers(task, &mut stack, &mut tcb) == 1)
                    .then(|| (*name, stack.cast(), tcb.cast()))
            }
        })
        .collect()
}

/// Frees the buffers of tasks that are gone (deleted by
/// `esp_hosted_deinit`; a task deleting itself is removed by the idle
/// task, hence the wait). One still there keeps them.
fn free_task_buffers(buffers: Vec<(&'static core::ffi::CStr, *mut u8, *mut u8)>) {
    use esp_idf_svc::sys;
    if buffers.is_empty() {
        return;
    }
    std::thread::sleep(Duration::from_millis(100));
    let mut freed = 0;
    for (name, stack, tcb) in buffers {
        // SAFETY: the task no longer exists (looked up by name, including
        // tasks waiting for deletion), so nothing uses its buffers; both
        // came from `heap_caps_*` (`hosted_thread_create`).
        unsafe {
            if !sys::xTaskGetHandle(name.as_ptr()).is_null() {
                log::warn!("wifi: task {name:?} still there, its buffers kept");
                continue;
            }
            sys::heap_caps_free(stack.cast());
            sys::heap_caps_free(tcb.cast());
        }
        freed += 1;
    }
    log::info!("wifi: freed the buffers of {freed} esp_hosted tasks");
}

/// One session: C6 on, driver up, connect and sync the clock (or scan once
/// without credentials), driver down. True if the clock was synced. The
/// caller switches the C6 off.
fn session(
    modem: &mut Modem<'static>,
    power: &Switch,
    sysloop: &EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    cfg: Option<&WifiConfig>,
    set: &dyn Fn(Status),
    job: JobGuard,
    more: &mpsc::Receiver<Job>,
) -> anyhow::Result<bool> {
    power(true)?;
    std::thread::sleep(Duration::from_millis(500));
    // The transport to the C6 (SDIO, RPC): set up at boot by esp_hosted
    // itself, torn down after each session (`end_session`); again here.
    // A no-op when it is up.
    let err = unsafe { esp_idf_svc::sys::esp_hosted_init() };
    anyhow::ensure!(err == 0, "esp_hosted_init: {err}");
    let t = Instant::now();
    // SAFETY: the reborrowed modem lives only as long as this session's
    // driver, which is dropped before the next session reborrows it.
    let modem = unsafe { modem.reborrow() };
    let wifi = BlockingWifi::wrap(
        EspWifi::new(modem, sysloop.clone(), Some(nvs))?,
        sysloop.clone(),
    )?;
    log::info!("wifi: driver up after {} ms", t.elapsed().as_millis());
    if let Some(_version) = log_c6_version() {
        #[cfg(feature = "c6-update")]
        crate::c6_update::run(_version);
    }
    // The driver goes down when `wifi` is dropped (`esp_wifi_deinit`).
    match cfg {
        Some(cfg) => sync_clock(wifi, cfg, set, job, more),
        None => {
            job.run(Err("Kein WLAN eingerichtet (config.json)".into()));
            scan_once(wifi, set).map(|()| false)
        }
    }
}

/// The C6 firmware this firmware expects: esp_hosted slave, the version of
/// the host component (`Cargo.toml`) and of the image in `firmware/c6/`.
/// The factory firmware (M5Stack's 1.4.1) works, but with warnings at start
/// and a known RX stall; feature `c6-update` installs this one.
pub const C6_FIRMWARE: [u32; 3] = [2, 13, 1];

/// Logs the C6 firmware's version once per boot (`GetCoprocessorFwVersion`
/// RPC, slave ≥ 1.0.0; the startup event of the factory firmware carries
/// none and shows as 0.0.0) and warns if it is not [`C6_FIRMWARE`]. Returns
/// it (major, minor, patch) the first time.
fn log_c6_version() -> Option<[u32; 3]> {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let mut v = esp_idf_svc::sys::esp_hosted_coprocessor_fwver_t::default();
    // SAFETY: `v` is a plain out-parameter, valid for the call.
    let err = unsafe { esp_idf_svc::sys::esp_hosted_get_coprocessor_fwversion(&mut v) };
    if err == 0 {
        log::info!(
            "wifi: C6 firmware {}.{}.{} (revision {}, prerelease {}, build {})",
            v.major1,
            v.minor1,
            v.patch1,
            v.revision,
            v.prerelease,
            v.build
        );
        let version = [v.major1, v.minor1, v.patch1];
        if version != C6_FIRMWARE {
            log::warn!(
                "wifi: the C6 runs {version:?}, expected {C6_FIRMWARE:?}: update it \
                 (feature c6-update, firmware/c6/README.md)"
            );
        }
        Some(version)
    } else {
        log::warn!("wifi: C6 firmware version not available: {err}");
        None
    }
}

/// 802.11b/g/n, not ax: with 802.11ax (HE20) downloads through the C6
/// were ~25 % slower (1160 against 1553 KiB/s, same router and place).
/// Feature `download-bench` with `WAKEY_BENCH_WIFI=ax` at build time keeps
/// ax for comparison.
fn wifi_mode() {
    use esp_idf_svc::sys;
    #[cfg(feature = "download-bench")]
    if option_env!("WAKEY_BENCH_WIFI") == Some("ax") {
        log::warn!("wifi: bench: 802.11ax left on");
        return;
    }
    let bgn = (sys::WIFI_PROTOCOL_11B | sys::WIFI_PROTOCOL_11G | sys::WIFI_PROTOCOL_11N) as u8;
    // SAFETY: a plain driver call after `start`, before `connect`.
    let err = unsafe { sys::esp_wifi_set_protocol(sys::wifi_interface_t_WIFI_IF_STA, bgn) };
    if err != 0 {
        log::warn!("wifi: 802.11b/g/n only: {err}");
    }
}

/// No credentials: one scan for the status line.
fn scan_once(mut wifi: BlockingWifi<EspWifi<'_>>, set: &dyn Fn(Status)) -> anyhow::Result<()> {
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
    Ok(())
}

/// Connects (up to [`ATTEMPTS`] tries), runs `job` and waits for SNTP. Jobs
/// that come in (`more`) while connected run in this session too: a TIDAL
/// tab opened during a slow NTP sync does not wait for the next session.
/// True if the clock was synced.
fn sync_clock(
    mut wifi: BlockingWifi<EspWifi<'_>>,
    cfg: &WifiConfig,
    set: &dyn Fn(Status),
    job: JobGuard,
    more: &mpsc::Receiver<Job>,
) -> anyhow::Result<bool> {
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
        attempt += 1;
        set(Status::Connecting { ssid: ssid.clone() });
        log::info!("wifi: connecting to \"{ssid}\" (attempt {attempt})");
        match wifi.connect().and_then(|()| wifi.wait_netif_up()) {
            Ok(()) => break,
            Err(e) => {
                log::warn!("wifi: connecting failed: {e}");
                set(Status::Failed {
                    ssid: ssid.clone(),
                    error: Failure::from(&e),
                });
                let _ = wifi.disconnect();
                if attempt >= ATTEMPTS {
                    let _ = wifi.stop();
                    job.run(Err(format!("Keine Verbindung zu „{ssid}“")));
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
    set(Status::Connected {
        ssid: ssid.clone(),
        ip,
        rssi: wifi.wifi().driver().get_rssi().ok(),
    });
    // The job first: the clock (from the RTC) is good enough for TLS.
    job.run(Ok(()));
    let run_more = || {
        while let Ok(job) = more.try_recv() {
            job(Ok(()));
        }
    };
    run_more();
    let sntp = EspSntp::new_default()?;
    let t = Instant::now();
    let synced = loop {
        run_more();
        if sntp.get_sync_status() == SyncStatus::Completed {
            log::info!("wifi: NTP sync done after {} ms", t.elapsed().as_millis());
            break true;
        }
        if t.elapsed() > NTP_TIMEOUT {
            log::warn!("wifi: no NTP answer within {} s", NTP_TIMEOUT.as_secs());
            break false;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    drop(sntp);
    run_more();
    // Kept HTTPS connections (`http`) end with the session.
    crate::http::close_all();
    let _ = wifi.disconnect();
    let _ = wifi.stop();
    if !synced {
        set(Status::Failed {
            ssid,
            error: Failure::Timeout,
        });
        anyhow::bail!("no NTP sync");
    }
    Ok(true)
}
