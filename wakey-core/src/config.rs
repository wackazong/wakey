//! The `config.json` model. Unknown fields are ignored and missing ones get
//! defaults, so the file can grow ahead of the firmware;
//! [`Config::fill_in_defaults`] adds the missing ones to the file's text so
//! they show up for editing.
//!
//! ```json
//! {
//!   "wifi": { "ssid": "MeinNetz", "password": "geheim" },
//!   "alarm": { "enabled": true, "hour": 6, "minute": 30, … }
//! }
//! ```

use crate::alarm::AlarmConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wifi: Option<WifiConfig>,
    /// See [`crate::alarm`].
    pub alarm: AlarmConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiConfig {
    pub ssid: String,
    /// Empty for an open network.
    #[serde(default)]
    pub password: String,
}

impl Config {
    /// Parses and brings out-of-range values into range.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        let mut c: Self = serde_json::from_str(json)?;
        c.alarm.sanitize();
        Ok(c)
    }

    /// `json` with every missing field added at its default, pretty-printed;
    /// `None` if nothing is missing (or `json` is not an object). Values
    /// already there, unknown fields included, stay as they are.
    pub fn fill_in_defaults(json: &str) -> Option<String> {
        fn merge(into: &mut Value, defaults: Value) -> bool {
            let (Value::Object(into), Value::Object(defaults)) = (into, defaults) else {
                return false;
            };
            let mut changed = false;
            for (key, default) in defaults {
                match into.get_mut(&key) {
                    Some(v) => changed |= merge(v, default),
                    None => {
                        into.insert(key, default);
                        changed = true;
                    }
                }
            }
            changed
        }
        let mut value: Value = serde_json::from_str(json).ok()?;
        if !value.is_object() {
            return None;
        }
        let defaults = serde_json::to_value(Self::default()).ok()?;
        merge(&mut value, defaults).then(|| {
            let mut text = serde_json::to_string_pretty(&value).expect("JSON value");
            text.push('\n');
            text
        })
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
    fn alarm_section() {
        let c =
            Config::parse(r#"{ "alarm": { "enabled": true, "hour": 7, "minute": 99 } }"#).unwrap();
        assert!(c.alarm.enabled);
        assert_eq!(
            (c.alarm.hour, c.alarm.minute, c.alarm.snooze_min),
            (7, 59, 9)
        );
    }

    #[test]
    fn defaults_written_back() {
        let text = r#"{ "wifi": { "ssid": "Netz" }, "alarm": { "hour": 7 }, "future": 1 }"#;
        let filled = Config::fill_in_defaults(text).unwrap();
        let v: Value = serde_json::from_str(&filled).unwrap();
        assert_eq!(v["alarm"]["hour"], 7);
        assert_eq!(v["alarm"]["minute"], 30);
        assert_eq!(v["alarm"]["max_snoozes"], 3);
        assert_eq!(v["future"], 1);
        assert_eq!(v["wifi"]["ssid"], "Netz");
        assert!(v["alarm"].get("sound").is_none());
        assert_eq!(
            Config::parse(&filled).unwrap(),
            Config::parse(text).unwrap()
        );
        // Complete files stay untouched.
        assert_eq!(Config::fill_in_defaults(&filled), None);
        assert_eq!(Config::fill_in_defaults("[]"), None);
        assert_eq!(Config::fill_in_defaults("not json"), None);
    }

    #[test]
    fn empty() {
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
        assert!(Config::parse("{ \"wifi\": 3 }").is_err());
    }
}
