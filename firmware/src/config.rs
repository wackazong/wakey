//! Loading `config.json` from the SD card root (model and format:
//! [`wakey_core::config`]).
//!
//! Without a card or file, Wi-Fi credentials can come from the build
//! environment (`WAKEY_WIFI_SSID`, `WAKEY_WIFI_PASSWORD`), for development.

use crate::board::sd;
pub use wakey_core::config::{Config, WifiConfig};

pub const FILE: &str = "config.json";

/// Reads the configuration; a missing or broken file gives the defaults.
pub fn load() -> Config {
    let path = format!("{}/{FILE}", sd::MOUNT_POINT);
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text).unwrap_or_else(|e| {
            log::error!("config: {path}: {e}");
            Config::default()
        }),
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
    cfg
}
