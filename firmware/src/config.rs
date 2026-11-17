//! `config.json` in the SD card root: loading it, and writing the parts
//! the clock changes itself, the alarms and the TIDAL session (model and
//! format: [`wakey_core::config`]).
//!
//! For development the build environment can provide Wi-Fi credentials
//! when the file has none (`WAKEY_WIFI_SSID`, `WAKEY_WIFI_PASSWORD`) and
//! replace the alarms with one test alarm (`WAKEY_ALARM=HH:MM`: every day).

use crate::board::sd;
use std::io;
use std::sync::Mutex;
use wakey_core::alarm::{self, AlarmConfig};
pub use wakey_core::config::{Config, TidalConfig, TidalSession, WifiConfig};

pub const FILE: &str = "config.json";

/// One writer at a time: alarms from the UI thread, the TIDAL session from
/// the `wifi` thread.
static WRITE: Mutex<()> = Mutex::new(());

/// Reads the configuration; a missing file gives the defaults (first
/// setup). A file that cannot be read is an error (in words, for the
/// screen): the clock must not run on defaults and overwrite it later.
/// Settings missing from the file are added to it.
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
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
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
pub fn save_alarms(alarms: &[AlarmConfig]) -> io::Result<()> {
    rewrite(|old| Config::with_alarms(old, alarms))?;
    log::info!("config: alarms saved to {FILE}");
    Ok(())
}

/// Writes the TIDAL session into `config.json` (`None`: removes it),
/// keeping the rest of the file.
pub fn save_tidal_session(session: Option<&TidalSession>) -> io::Result<()> {
    rewrite(|old| Config::with_tidal_session(old, session))?;
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

/// Replaces `config.json` with what `edit` makes of its text (`None`: no
/// file yet). A file that is there but cannot be read is an error: it must
/// not be replaced by one that holds only what `edit` adds.
fn rewrite(edit: impl FnOnce(Option<&str>) -> String) -> io::Result<()> {
    let _writer = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let path = sd::path(FILE);
    let old = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    sd::replace(&path, edit(old.as_deref()).as_bytes())
}
