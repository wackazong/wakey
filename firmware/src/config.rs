//! `config.json` in the SD card root (format: architecture doc, "Data
//! formats"). Only the parts the firmware uses so far are read; unknown
//! fields are ignored, missing ones get defaults.
//!
//! ```json
//! { "wifi": { "ssid": "MeinNetz", "password": "geheim" } }
//! ```
//!
//! Without a card or file, Wi-Fi credentials can come from the build
//! environment (`WAKEY_WIFI_SSID`, `WAKEY_WIFI_PASSWORD`), for development.

use serde::Deserialize;

pub const PATH: &str = "/sdcard/config.json";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub wifi: Option<WifiConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WifiConfig {
    pub ssid: String,
    #[serde(default)]
    pub password: String,
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = match std::fs::read_to_string(PATH) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::error!("config: {PATH}: {e}");
                Config::default()
            }),
            Err(e) => {
                log::info!("config: no {PATH} ({e})");
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
}
