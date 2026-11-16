//! Loading `config.json` from the SD card root (model and format:
//! [`wakey_core::config`]).
//!
//! For development the build environment can provide Wi-Fi credentials
//! when the file has none (`WAKEY_WIFI_SSID`, `WAKEY_WIFI_PASSWORD`) and
//! replace the alarms with one test alarm (`WAKEY_ALARM=HH:MM`: every day).

use crate::board::sd;
use wakey_core::alarm::{self, AlarmConfig};
pub use wakey_core::config::{Config, TidalConfig, TidalSession, WifiConfig};

/// One writer at a time: alarms from the UI thread, the TIDAL session from
/// the `wifi` thread.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub const FILE: &str = "config.json";

/// Reads the configuration; a missing file gives the defaults (first
/// setup). A file that cannot be read is an error (in words, for the
/// screen): the clock must not run on defaults and overwrite it later. An
/// old `alarm` section is migrated and settings missing from the file are
/// added to it.
pub fn load() -> Result<Config, String> {
    let path = sd::path(FILE);
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => match Config::load(&text) {
            Ok((cfg, updated)) => {
                if let Some(updated) = updated {
                    match sd::replace(&path, updated.as_bytes()) {
                        Ok(()) => log::info!("config: updated {FILE} (new or missing settings)"),
                        Err(e) => log::error!("config: {e}"),
                    }
                }
                cfg
            }
            Err(e) => {
                log::error!("config: {FILE}: {e}");
                return Err(e.to_string());
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!("config: no {FILE} ({e})");
            Config::default()
        }
        Err(e) => {
            log::error!("config: {FILE}: {e}");
            return Err(e.to_string());
        }
    };
    if cfg.wifi.is_none()
        && let Some(ssid) = option_env!("WAKEY_WIFI_SSID")
    {
        log::info!("config: Wi-Fi credentials from the build environment");
        cfg.wifi = Some(WifiConfig {
            ssid: ssid.into(),
            password: option_env!("WAKEY_WIFI_PASSWORD").unwrap_or("").into(),
        });
    }
    if let Some((h, m)) = option_env!("WAKEY_ALARM").and_then(|t| t.split_once(':'))
        && let (Ok(hour), Ok(minute)) = (h.parse(), m.parse())
    {
        log::warn!("config: test alarm {hour:02}:{minute:02} from the build environment");
        cfg.alarms = vec![AlarmConfig {
            id: 1,
            name: "Test".into(),
            hour,
            minute,
            weekdays: [1; 7],
            ..AlarmConfig::default()
        }];
        alarm::sanitize_list(&mut cfg.alarms);
    }
    Ok(cfg)
}

/// Writes `alarms` into `config.json`, keeping the rest of the file.
pub fn save_alarms(alarms: &[AlarmConfig]) -> std::io::Result<()> {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = sd::path(FILE);
    let old = std::fs::read_to_string(&path).ok();
    sd::replace(
        &path,
        Config::with_alarms(old.as_deref(), alarms).as_bytes(),
    )?;
    log::info!("config: alarms saved to {FILE}");
    Ok(())
}

/// Writes the TIDAL session into `config.json` (`None`: removes it),
/// keeping the rest of the file.
pub fn save_tidal_session(session: Option<&TidalSession>) -> std::io::Result<()> {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = sd::path(FILE);
    let old = std::fs::read_to_string(&path).ok();
    sd::replace(
        &path,
        Config::with_tidal_session(old.as_deref(), session).as_bytes(),
    )?;
    log::info!(
        "config: TIDAL session {}",
        if session.is_some() {
            "saved"
        } else {
            "removed"
        }
    );
    Ok(())
}
