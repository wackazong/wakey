//! Loading `config.json` from the SD card root (model and format:
//! [`wakey_core::config`]).
//!
//! For development the build environment can provide Wi-Fi credentials
//! when the file has none (`WAKEY_WIFI_SSID`, `WAKEY_WIFI_PASSWORD`) and
//! override the alarm time (`WAKEY_ALARM=HH:MM`: enabled, every day).

use crate::board::sd;
pub use wakey_core::config::{Config, WifiConfig};

pub const FILE: &str = "config.json";

/// Reads the configuration; a missing or broken file gives the defaults.
/// Settings missing from the file are added to it.
pub fn load() -> Config {
    let path = format!("{}/{FILE}", sd::MOUNT_POINT);
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => {
            // Show every setting in the file, at its default.
            if let Some(filled) = Config::fill_in_defaults(&text) {
                match std::fs::write(&path, filled) {
                    Ok(()) => log::info!("config: added missing settings to {path}"),
                    Err(e) => log::error!("config: writing {path}: {e}"),
                }
            }
            Config::parse(&text).unwrap_or_else(|e| {
                log::error!("config: {path}: {e}");
                Config::default()
            })
        }
        Err(e) => {
            log::info!("config: no {path} ({e})");
            Config::default()
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
        log::warn!("config: alarm {hour:02}:{minute:02} from the build environment");
        cfg.alarm.enabled = true;
        (cfg.alarm.hour, cfg.alarm.minute) = (hour, minute);
        cfg.alarm.weekdays = [1; 7];
        cfg.alarm.sanitize();
    }
    cfg
}
