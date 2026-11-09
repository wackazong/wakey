//! The `config.json` model. Only the parts the firmware uses so far;
//! unknown fields are ignored and missing ones get defaults, so the file can
//! grow ahead of the firmware.
//!
//! ```json
//! { "wifi": { "ssid": "MeinNetz", "password": "geheim" } }
//! ```

use serde::Deserialize;

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub wifi: Option<WifiConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WifiConfig {
    pub ssid: String,
    /// Empty for an open network.
    #[serde(default)]
    pub password: String,
}

impl Config {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi() {
        let c = Config::parse(r#"{ "wifi": { "ssid": "Netz", "password": "geheim" } }"#).unwrap();
        let w = c.wifi.unwrap();
        assert_eq!((w.ssid.as_str(), w.password.as_str()), ("Netz", "geheim"));
    }

    #[test]
    fn open_network_and_unknown_fields() {
        let c = Config::parse(r#"{ "wifi": { "ssid": "Gast" }, "alarm": { "hour": 6 } }"#).unwrap();
        assert_eq!(c.wifi.unwrap().password, "");
    }

    #[test]
    fn empty() {
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
        assert!(Config::parse("{ \"wifi\": 3 }").is_err());
    }
}
