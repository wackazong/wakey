//! Loading `config.json` from the SD card root (model and format:
//! [`wakey_core::config`]).
//!
//! For development the build environment can provide Wi-Fi credentials
//! when the file has none (`WAKEY_WIFI_SSID`, `WAKEY_WIFI_PASSWORD`) and
//! replace the alarms with one test alarm (`WAKEY_ALARM=HH:MM`: every day).

use crate::board::sd;
use wakey_core::alarm::{self, AlarmConfig};
pub use wakey_core::config::{Config, WifiConfig};

pub const FILE: &str = "config.json";

/// Reads the configuration; a missing or broken file gives the defaults.
/// An old `alarm` section is migrated and settings missing from the file
/// are added to it.
pub fn load() -> Config {
    let path = format!("{}/{FILE}", sd::MOUNT_POINT);
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(text) => match Config::load(&text) {
            Ok((cfg, updated)) => {
                if let Some(updated) = updated {
                    match replace(&updated) {
                        Ok(()) => log::info!("config: updated {path} (new or missing settings)"),
                        Err(e) => log::error!("config: writing {path}: {e}"),
                    }
                }
                cfg
            }
            Err(e) => {
                log::error!("config: {path}: {e}");
                Config::default()
            }
        },
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
    cfg
}

/// Writes `alarms` into `config.json`, keeping the rest of the file.
pub fn save_alarms(alarms: &[AlarmConfig]) -> std::io::Result<()> {
    let path = format!("{}/{FILE}", sd::MOUNT_POINT);
    let old = std::fs::read_to_string(&path).ok();
    replace(&Config::with_alarms(old.as_deref(), alarms))?;
    log::info!("config: alarms saved to {path}");
    Ok(())
}

/// Replaces `config.json` through a temporary file, so a failed write
/// leaves the old file intact.
fn replace(text: &str) -> std::io::Result<()> {
    let path = format!("{}/{FILE}", sd::MOUNT_POINT);
    let tmp = format!("{}/config.tmp", sd::MOUNT_POINT);
    std::fs::write(&tmp, text)?;
    // FAT cannot rename onto an existing file.
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::rename(&tmp, &path)
}
